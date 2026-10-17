use super::{issue, parsers, CollectOptions};
use crate::model::*;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

pub(super) struct Reader {
    pub root: PathBuf,
    pub fixture: bool,
}
impl Reader {
    pub fn path(&self, path: &str) -> Result<PathBuf, String> {
        if path.contains('\\') || path.split('/').any(|p| p == "..") || !path.starts_with('/') {
            return Err("invalid evidence path".into());
        }
        let p = self.root.join(path.trim_start_matches('/'));
        if self.fixture && p.exists() {
            let root = self
                .root
                .canonicalize()
                .map_err(|_| "fixture root unavailable")?;
            let actual = p.canonicalize().map_err(|_| "fixture path unavailable")?;
            if !actual.starts_with(root) {
                return Err("fixture path escapes sysroot".into());
            }
        }
        Ok(p)
    }
    pub fn read(&self, path: &str, cap: usize) -> Result<Vec<u8>, String> {
        read_bounded(&self.path(path)?, cap)
    }
    pub fn text(&self, path: &str) -> Result<String, String> {
        String::from_utf8(self.read(path, 65536)?).map_err(|_| "evidence is not UTF-8".into())
    }
}
pub(super) fn read_bounded(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    // Opening a FIFO read-only can block before the metadata check. Nonblocking
    // open lets us reject special files using the opened descriptor; regular
    // files (including procfs/sysfs evidence) keep their usual read semantics.
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = File::open(path);
    let f = opened.map_err(|e| format!("read failed: {:?}", e.kind()))?;
    if !f.metadata().map_err(|_| "metadata failed")?.is_file() {
        return Err("not a regular evidence file".into());
    }
    let mut data = Vec::new();
    f.take((cap + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|_| "read failed")?;
    if data.len() > cap {
        return Err("evidence size limit exceeded".into());
    }
    Ok(data)
}

pub(super) struct ProcessContext {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub home: Option<String>,
    pub config: Option<toml::Value>,
    pub starttime: Option<u64>,
}
pub(super) fn process_evidence(
    reader: &Reader,
    props: &BTreeMap<String, String>,
    role: &str,
    explicit_home: Option<&Path>,
) -> (ServiceSnapshot, ProcessContext) {
    let pid = props
        .get("MainPID")
        .and_then(|p| p.parse::<u32>().ok())
        .filter(|p| *p > 0);
    let mut service = ServiceSnapshot {
        role: role.into(),
        status: CollectionStatus::Partial,
        active_state: props.get("ActiveState").cloned(),
        sub_state: props.get("SubState").cloned(),
        ..Default::default()
    };
    service.identity.pid = pid;
    service.identity.restart_count = props.get("NRestarts").and_then(|v| v.parse().ok());
    service.identity.service_start_monotonic_us = props
        .get("ExecMainStartTimestampMonotonic")
        .and_then(|v| v.parse().ok());
    let mut ctx = ProcessContext {
        args: vec![],
        env: BTreeMap::new(),
        home: None,
        config: None,
        starttime: None,
    };
    let Some(pid) = pid else {
        service.status = CollectionStatus::Missing;
        return (service, ctx);
    };
    let base = format!("/proc/{pid}");
    ctx.starttime = reader
        .text(&format!("{base}/stat"))
        .ok()
        .and_then(|t| parsers::starttime(&t));
    service.identity.starttime_ticks = ctx.starttime;
    ctx.args = reader
        .read(&format!("{base}/cmdline"), 65536)
        .map(|b| parsers::nul_arguments(&b))
        .unwrap_or_default();
    ctx.env = reader
        .read(&format!("{base}/environ"), 65536)
        .map(|b| parsers::safe_env(&b))
        .unwrap_or_default();
    if let Ok(status) = reader.text(&format!("{base}/status")) {
        service.rss_bytes = parsers::kib(parsers::colon_fields(&status).get("VmRSS"));
    }
    if let Ok(limits) = reader.text(&format!("{base}/limits")) {
        (service.fd_soft_limit, service.fd_hard_limit) = parsers::fd_limits(&limits);
    }
    let cg = reader
        .text(&format!("{base}/cgroup"))
        .ok()
        .and_then(|s| parsers::cgroup_path(&s))
        .or_else(|| {
            props
                .get("ControlGroup")
                .filter(|p| p.starts_with('/') && !p.split('/').any(|s| s == ".."))
                .cloned()
        });
    if let Some(cg) = cg {
        service.identity.cgroup_path = Some(cg.clone());
        let base = format!("/sys/fs/cgroup{}", cg.trim_end_matches('/'));
        let num = |file: &str| {
            reader
                .text(&format!("{base}/{file}"))
                .ok()?
                .trim()
                .parse::<u64>()
                .ok()
        };
        service.memory_current_bytes = num("memory.current");
        service.memory_peak_bytes = num("memory.peak");
        service.memory_max = reader
            .text(&format!("{base}/memory.max"))
            .ok()
            .and_then(|v| parsers::limit(&v));
        service.memory_high = reader
            .text(&format!("{base}/memory.high"))
            .ok()
            .and_then(|v| parsers::limit(&v));
        if let Ok(t) = reader.text(&format!("{base}/memory.stat")) {
            let c = parsers::counters(&t);
            service.memory_anon_bytes = c.get("anon").copied();
            service.memory_file_bytes = c.get("file").copied();
            service.memory_kernel_bytes = c.get("kernel").copied();
        }
        if let Ok(t) = reader.text(&format!("{base}/memory.events")) {
            service.memory_events = parsers::counters(&t)
                .into_iter()
                .filter(|(k, _)| {
                    ["low", "high", "max", "oom", "oom_kill", "oom_group_kill"]
                        .contains(&k.as_str())
                })
                .collect();
        }
        service.cpu_usage_ns = reader
            .text(&format!("{base}/cpu.stat"))
            .ok()
            .and_then(|t| parsers::counters(&t).get("usage_usec").copied())
            .and_then(|n| n.checked_mul(1000))
            .or_else(|| props.get("CPUUsageNSec").and_then(|v| v.parse().ok()));
        let mut ancestor = Path::new(&cg).parent();
        let mut count = 0;
        service.ancestor_limits_complete = true;
        while let Some(path) = ancestor {
            if count >= 32 {
                service.ancestor_limits_complete = false;
                break;
            }
            count += 1;
            let name = path.to_string_lossy().replace('\\', "/");
            let parent = format!("/sys/fs/cgroup{}", name.trim_end_matches('/'));
            for field in ["memory.max", "memory.high"] {
                if let Some(lim) = reader
                    .text(&format!("{parent}/{field}"))
                    .ok()
                    .and_then(|s| parsers::limit(&s))
                {
                    service
                        .ancestor_limits
                        .insert(format!("{name}:{field}"), lim);
                // cgroup v2's root is not resource controlled and does not
                // expose memory.high/max. Every non-root ancestor must read.
                } else if name != "/" {
                    service.ancestor_limits_complete = false;
                }
            }
            ancestor = path.parent();
        }
    }
    ctx.home = explicit_home
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .or_else(|| parsers::argument(&ctx.args, "--home"))
        .or_else(|| {
            if role == "fibre" {
                ctx.env.get("FIBRE_HOME").filter(|v| !v.is_empty()).cloned()
            } else {
                None
            }
        })
        .or_else(|| {
            ctx.env.get("HOME").filter(|h| !h.is_empty()).map(|h| {
                format!(
                    "{}/{}",
                    h.trim_end_matches('/'),
                    if role == "fibre" {
                        ".celestia-fibre"
                    } else {
                        ".celestia-app"
                    }
                )
            })
        })
        .or_else(|| (role == "fibre").then(|| ".celestia-fibre".into()));
    if let Some(home) = ctx.home.as_ref() {
        let name = if role == "fibre" {
            "server_config.toml"
        } else {
            "config.toml"
        };
        // Live reads resolve against the process root/cwd so a namespace does not change the meaning.
        let config_path = if reader.fixture {
            format!("{home}/config/{name}")
        } else if home.starts_with('/') {
            format!("{base}/root{home}/config/{name}")
        } else {
            format!("{base}/cwd/{home}/config/{name}")
        };
        ctx.config = reader.text(&config_path).ok().and_then(|s| s.parse().ok());
    }
    let end = reader
        .text(&format!("{base}/stat"))
        .ok()
        .and_then(|s| parsers::starttime(&s));
    service.identity.stable_during_capture = ctx.starttime.zip(end).map(|(a, b)| a == b);
    if service.identity.stable_during_capture == Some(true) {
        service.status = CollectionStatus::Complete;
    }
    (service, ctx)
}

pub(super) fn collect_fixture(options: &CollectOptions) -> Result<SnapshotV1, String> {
    let root = options
        .sysroot
        .as_ref()
        .ok_or("Missing fixture root")?
        .canonicalize()
        .map_err(|_| "Fixture root unavailable")?;
    let reader = Reader {
        root,
        fixture: true,
    };
    let mut out = SnapshotV1::default();
    // Fixture capture time comes solely from supplied evidence, never the PC clock.
    if let Ok(bytes) = reader.read("/run/ttt-fibre/clock.json", 4096) {
        let clock: Capture = serde_json::from_slice(&bytes).map_err(|_| "Invalid fixture clock")?;
        if [clock.started_epoch_seconds, clock.finished_epoch_seconds]
            .into_iter()
            .flatten()
            .any(|t| !t.is_finite() || t < 0.)
            || clock
                .started_epoch_seconds
                .zip(clock.finished_epoch_seconds)
                .is_some_and(|(a, b)| b < a)
        {
            return Err("Invalid fixture clock interval".into());
        }
        out.capture.started_epoch_seconds = clock.started_epoch_seconds;
        out.capture.finished_epoch_seconds = clock.finished_epoch_seconds;
    }
    out.capture.input_format = "fixture_v1".into();
    out.capture.supplied_provenance = Some(
        "Offline fixture; no live commands, network, Unix metadata or process observations.".into(),
    );
    read_host(&reader, &mut out);
    for (unit, role, home) in [
        (&options.app_unit, "app", options.app_home.as_deref()),
        (&options.fibre_unit, "fibre", options.fibre_home.as_deref()),
    ] {
        if !valid_unit(unit) {
            return Err("Invalid service unit name".into());
        }
        let props = reader
            .text(&format!("/run/ttt-fibre/{unit}.show"))
            .map(|s| parsers::properties(&s))
            .unwrap_or_default();
        let (svc, ctx) = process_evidence(&reader, &props, role, home);
        if role == "fibre" {
            if let Some(cfg) = ctx.config.as_ref() {
                out.fibre_config = parsers::fibre_config(cfg, &ctx.args);
            }
        }
        out.services.insert(unit.clone(), svc);
    }
    for label in ["staging", "shards"] {
        let path = format!("/run/ttt-fibre/{label}.scan.json");
        let scan = reader
            .read(&path, 524288)
            .ok()
            .and_then(|b| serde_json::from_slice::<ScanResult>(&b).ok());
        out.scans.push(scan.unwrap_or_else(|| ScanResult {
            directory: label.into(),
            status: CollectionStatus::Unsupported,
            stop_reason: "Unix metadata must be supplied as a fixture scan record".into(),
            ..Default::default()
        }));
    }
    if let Ok(bytes) = reader.read("/run/ttt-fibre/metrics.prom", 1024 * 1024) {
        if let Ok(text) = String::from_utf8(bytes) {
            out.metrics = super::metrics::parse_metrics_at(
                &text,
                "fixture",
                false,
                out.capture.finished_epoch_seconds,
            );
        }
    }
    if let Ok(text) = reader.text("/run/ttt-fibre/app-status.json") {
        out.app_status = super::metrics::parse_app_status(&text);
    }
    issue(
        &mut out,
        "collection",
        "FIXTURE_ONLY",
        "Fixture results are supplied evidence and do not validate live Linux behavior.",
    );
    Ok(out)
}
pub(super) fn valid_unit(unit: &str) -> bool {
    !unit.is_empty()
        && unit.len() <= 128
        && unit
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.@-".contains(&c))
        && !unit.starts_with('-')
}
pub(super) fn read_host(reader: &Reader, out: &mut SnapshotV1) {
    if let Ok(t) = reader.text("/proc/meminfo") {
        let f = parsers::colon_fields(&t);
        out.host.memory_total_bytes = parsers::kib(f.get("MemTotal"));
        out.host.memory_available_bytes = parsers::kib(f.get("MemAvailable"));
        out.host.swap_total_bytes = parsers::kib(f.get("SwapTotal"));
        out.host.swap_free_bytes = parsers::kib(f.get("SwapFree"));
    }
    out.capture.boot_hash = reader
        .text("/proc/sys/kernel/random/boot_id")
        .ok()
        .filter(|s| {
            let s = s.trim();
            s.len() == 36
                && s.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
        })
        .map(|s| parsers::hash(s.trim().as_bytes()));
    out.capture.host_hash = reader
        .text("/etc/machine-id")
        .ok()
        .filter(|s| s.trim().len() == 32 && s.trim().bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|s| parsers::hash(s.trim().as_bytes()));
    if let Ok(text) = reader.text("/proc/stat") {
        out.host.cpu_count = Some(
            text.lines()
                .filter(|l| {
                    l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit)
                })
                .count() as u32,
        );
    }
    out.capture.monotonic_ns = reader
        .text("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.)
        .map(|s| (s * 1e9) as u64);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_regular_file_keeps_bytes_and_rejects_oversize() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("evidence");
        std::fs::write(&path, b"sample").unwrap();
        assert_eq!(read_bounded(&path, 6).unwrap(), b"sample");
        assert_eq!(
            read_bounded(&path, 5).unwrap_err(),
            "evidence size limit exceeded"
        );
    }
    #[cfg(unix)]
    #[test]
    fn evidence_fifo_without_writer_is_rejected_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, time::Duration};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("evidence.fifo");
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated path retained for this call.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(read_bounded(&path, 65536));
        });
        // Keep the regression bounded even if a future change restores a
        // blocking open: the test process does not join the stuck reader.
        let result = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("reading FIFO evidence blocked waiting for a writer");
        assert_eq!(result.unwrap_err(), "not a regular evidence file");
    }
    #[test]
    fn fixture_clock_and_metrics_are_deterministic_supplied_evidence() {
        let root = tempfile::tempdir().unwrap();
        let evidence = root.path().join("run/ttt-fibre");
        std::fs::create_dir_all(&evidence).unwrap();
        std::fs::write(
            evidence.join("clock.json"),
            br#"{"started_epoch_seconds":1000.0,"finished_epoch_seconds":1001.0}"#,
        )
        .unwrap();
        std::fs::write(
            evidence.join("metrics.prom"),
            "fibre_server_upload_shard_occupancy_bytes 123\n",
        )
        .unwrap();
        let options = CollectOptions {
            sysroot: Some(root.path().into()),
            ..Default::default()
        };
        let first = collect_fixture(&options).unwrap();
        let second = collect_fixture(&options).unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        assert_eq!(first.capture.started_epoch_seconds, Some(1000.0));
        assert_eq!(first.metrics.collected_epoch_seconds, Some(1001.0));
        std::fs::remove_file(evidence.join("clock.json")).unwrap();
        let missing = collect_fixture(&options).unwrap();
        assert!(missing.capture.started_epoch_seconds.is_none());
        assert!(missing.metrics.collected_epoch_seconds.is_none());
    }
    #[test]
    fn fixture_is_offline_and_uses_explicit_linux_metadata() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proc")).unwrap();
        std::fs::write(
            root.path().join("proc/meminfo"),
            "MemTotal: 1024 kB\nMemAvailable: 512 kB\n",
        )
        .unwrap();
        let result = collect_fixture(&CollectOptions {
            sysroot: Some(root.path().into()),
            metrics_url: Some("http://127.0.0.1:1/metrics".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(result.host.memory_total_bytes, Some(1048576));
        assert!(!result.metrics.target_verified);
        assert!(result
            .scans
            .iter()
            .all(|s| s.status == CollectionStatus::Unsupported));
    }
    #[test]
    fn fixture_rejects_parent_escape() {
        let reader = Reader {
            root: PathBuf::from("fixture"),
            fixture: true,
        };
        assert!(reader.path("/proc/../../secret").is_err());
    }
    #[test]
    fn ancestor_completeness_requires_every_nonroot_limit() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proc/7")).unwrap();
        std::fs::write(root.path().join("proc/7/cgroup"), "0::/slice/unit\n").unwrap();
        std::fs::create_dir_all(root.path().join("sys/fs/cgroup/slice/unit")).unwrap();
        std::fs::write(root.path().join("sys/fs/cgroup/slice/memory.max"), "max\n").unwrap();
        let reader = Reader {
            root: root.path().into(),
            fixture: true,
        };
        let props = BTreeMap::from([("MainPID".into(), "7".into())]);
        let (service, _) = process_evidence(&reader, &props, "fibre", None);
        assert!(!service.ancestor_limits_complete);
        std::fs::write(
            root.path().join("sys/fs/cgroup/slice/memory.high"),
            "12345\n",
        )
        .unwrap();
        let (service, _) = process_evidence(&reader, &props, "fibre", None);
        assert!(service.ancestor_limits_complete);
        assert_eq!(
            service.ancestor_limits["/slice:memory.high"],
            MemoryLimit::Finite(12345)
        );
    }
    #[test]
    fn malformed_config_is_missing_not_an_invented_default() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("fibre/config")).unwrap();
        std::fs::write(
            root.path().join("fibre/config/server_config.toml"),
            "max_connections = [bad",
        )
        .unwrap();
        let reader = Reader {
            root: root.path().into(),
            fixture: true,
        };
        let props = BTreeMap::from([("MainPID".into(), "7".into())]);
        let (_, ctx) = process_evidence(&reader, &props, "fibre", Some(Path::new("/fibre")));
        assert!(ctx.config.is_none());
    }
}
