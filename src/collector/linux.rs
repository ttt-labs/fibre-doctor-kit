use super::{
    epoch,
    fixture::{self, ProcessContext, Reader},
    issue, metrics, parsers, scanner, supervisor, version, CollectOptions,
};
use crate::model::*;
use std::{
    collections::HashSet,
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    net::TcpStream,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn remaining(start: Instant, max: Duration) -> Duration {
    supervisor::TOTAL.saturating_sub(start.elapsed()).min(max)
}
fn checkpoint(out: &mut SnapshotV1, start: Instant, complete: bool) -> Result<(), String> {
    out.worker_usage = vec![supervisor::usage(
        start,
        libc::RUSAGE_SELF,
        "collection worker process only; excludes storage scanners and helper commands",
        complete,
    )];
    supervisor::emit(out)
}
fn filesystem(path: &Path) -> Option<FilesystemSnapshot> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let md = fs::metadata(path).ok()?;
    Some(FilesystemSnapshot {
        total_bytes: (stat.f_blocks as u64).checked_mul(stat.f_frsize as u64),
        available_bytes: (stat.f_bavail as u64).checked_mul(stat.f_frsize as u64),
        total_inodes: Some(stat.f_files as u64),
        available_inodes: Some(stat.f_favail as u64),
        device_id: Some(md.dev()),
        mount_id: None,
    })
}
fn process_path(pid: u32, path: &str) -> PathBuf {
    if path.starts_with('/') {
        PathBuf::from(format!("/proc/{pid}/root{path}"))
    } else {
        PathBuf::from(format!("/proc/{pid}/cwd/{path}"))
    }
}
fn home_path(pid: u32, ctx: &ProcessContext) -> Option<PathBuf> {
    Some(process_path(pid, ctx.home.as_ref()?))
}
fn store_path(pid: u32, ctx: &ProcessContext) -> Option<PathBuf> {
    // v10.4.0-mocha fibre/cmd/start_cmd.go sets cfg.Path = home; Path is
    // toml:"-". Neither home/data nor store.path is a supported store root.
    home_path(pid, ctx)
}
fn layout_supported(service: &ServiceSnapshot, profile: Option<&str>) -> bool {
    service
        .version
        .as_ref()
        .map(|v| v.version.as_str())
        .filter(|v| *v != "unknown")
        .or(profile)
        == Some("v10.4.0-mocha")
}
struct Fds {
    ids: HashSet<(u64, u64)>,
    count: u64,
    complete: bool,
}
fn fd_open_state(fds: &Fds, dev: u64, ino: u64) -> Option<bool> {
    if fds.ids.contains(&(dev, ino)) {
        Some(true)
    } else if fds.complete {
        Some(false)
    } else {
        None
    }
}
fn fds(pid: u32, budget: Duration, identify: bool) -> Fds {
    let start = Instant::now();
    let mut result = Fds {
        ids: HashSet::new(),
        count: 0,
        complete: true,
    };
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        result.complete = false;
        return result;
    };
    for entry in entries {
        if start.elapsed() >= budget || result.count >= 50000 {
            result.complete = false;
            break;
        }
        let Ok(entry) = entry else {
            result.complete = false;
            continue;
        };
        result.count += 1;
        if identify {
            match fs::metadata(entry.path()) {
                Ok(m) => {
                    if m.is_file() {
                        result.ids.insert((m.dev(), m.ino()));
                    }
                }
                Err(_) => result.complete = false,
            }
        }
    }
    result
}
fn listener_owned(pid: u32, address: std::net::SocketAddr, start: Instant) -> bool {
    let Ok(bytes) = supervisor::run(
        "/usr/bin/ss",
        &["-H", "-ltnp"],
        remaining(start, Duration::from_millis(800)),
        131072,
    ) else {
        return false;
    };
    String::from_utf8_lossy(&bytes).lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 5 {
            return false;
        }
        let local = fields[3];
        let exact = local == address.to_string();
        let wildcard = local == format!("*:{}", address.port())
            || local == format!("0.0.0.0:{}", address.port())
            || local == format!("[::]:{}", address.port());
        (exact || wildcard) && line.contains(&format!("pid={pid},"))
    })
}
fn listener_pid(address: std::net::SocketAddr, start: Instant) -> Option<u32> {
    let bytes = supervisor::run(
        "/usr/bin/ss",
        &["-H", "-ltnp"],
        remaining(start, Duration::from_millis(800)),
        131072,
    )
    .ok()?;
    let mut owners = HashSet::new();
    for line in String::from_utf8_lossy(&bytes).lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let Some(local) = fields.get(3) else {
            continue;
        };
        if *local != address.to_string()
            && *local != format!("*:{}", address.port())
            && *local != format!("0.0.0.0:{}", address.port())
            && *local != format!("[::]:{}", address.port())
        {
            continue;
        }
        for piece in line.split("pid=").skip(1) {
            if let Some(pid) = piece.split(',').next().and_then(|s| s.parse::<u32>().ok()) {
                owners.insert(pid);
            }
        }
    }
    if owners.len() == 1 {
        owners.into_iter().next()
    } else {
        None
    }
}
fn same_net_namespace(pid: u32) -> bool {
    fs::read_link(format!("/proc/{pid}/ns/net"))
        .ok()
        .zip(fs::read_link("/proc/self/ns/net").ok())
        .is_some_and(|(a, b)| a == b)
}
fn discover_otel(
    options: &CollectOptions,
    fibre: &Option<(u32, ProcessContext)>,
    start: Instant,
) -> Option<String> {
    let (fibre_pid, ctx) = fibre.as_ref()?;
    if !same_net_namespace(*fibre_pid) {
        return None;
    }
    let endpoint = parsers::argument(&ctx.args, "--otel-endpoint")
        .or_else(|| ctx.env.get("FIBRE_OTEL_ENDPOINT").cloned())?;
    // Treat the exact spelling localhost as a literal loopback, never DNS.
    let endpoint = endpoint.replacen("http://localhost:", "http://127.0.0.1:", 1);
    let input = metrics::loopback_target(&endpoint).ok()?;
    if input.path != "/" {
        return None;
    }
    let owner = listener_pid(input.addr, start)?;
    if !same_net_namespace(owner) {
        return None;
    }
    let reader = Reader {
        root: PathBuf::from("/"),
        fixture: false,
    };
    let initial = reader
        .text(&format!("/proc/{owner}/stat"))
        .ok()
        .and_then(|s| parsers::starttime(&s))?;
    let args = parsers::nul_arguments(&reader.read(&format!("/proc/{owner}/cmdline"), 65536).ok()?);
    let exe = fs::read_link(format!("/proc/{owner}/exe")).ok()?;
    let name = exe.file_name()?.to_str()?;
    if ![
        "otelcol",
        "otelcol-contrib",
        "otelcol-k8s",
        "otelcol-custom",
    ]
    .contains(&name)
    {
        return None;
    }
    // Multiple --config flags merge sources; avoid silently reasoning from one.
    if args
        .iter()
        .filter(|s| *s == "--config" || s.starts_with("--config="))
        .count()
        != 1
    {
        return None;
    }
    let config = parsers::argument(&args, "--config")?;
    let config = config.strip_prefix("file:").unwrap_or(&config);
    if config.contains("://") || config.starts_with("env:") {
        return None;
    }
    let actual_path = process_path(owner, config);
    if let Some(provided) = options.otel_config.as_ref() {
        if provided.canonicalize().ok()? != actual_path.canonicalize().ok()? {
            return None;
        }
    }
    let text = String::from_utf8(fixture::read_bounded(&actual_path, 65536).ok()?).ok()?;
    let url = metrics::otel_exporter_for_receiver(&text, Some(input.addr)).ok()?;
    let output = metrics::loopback_target(&url).ok()?;
    if !listener_owned(owner, output.addr, start) {
        return None;
    }
    let final_start = reader
        .text(&format!("/proc/{owner}/stat"))
        .ok()
        .and_then(|s| parsers::starttime(&s))?;
    (initial == final_start).then_some(url)
}
fn http_get(target: &metrics::HttpTarget, start: Instant) -> Result<String, String> {
    let budget = remaining(start, Duration::from_millis(1200));
    if budget.is_zero() {
        return Err("deadline".into());
    }
    let mut stream =
        TcpStream::connect_timeout(&target.addr, budget).map_err(|_| "HTTP connect failed")?;
    stream
        .set_read_timeout(Some(budget))
        .map_err(|_| "HTTP timeout setup failed")?;
    stream
        .set_write_timeout(Some(budget))
        .map_err(|_| "HTTP timeout setup failed")?;
    // Literal loopback sockets; no proxy environment, DNS, redirects or authentication.
    let request=format!("GET {} HTTP/1.0\r\nHost: {}\r\nAccept: text/plain, application/json\r\nConnection: close\r\n\r\n",target.path,target.addr);
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "HTTP write failed")?;
    let mut data = Vec::new();
    let mut b = [0u8; 8192];
    let end = Instant::now() + budget;
    loop {
        if Instant::now() >= end {
            return Err("HTTP deadline".into());
        }
        stream
            .set_read_timeout(Some(
                end.saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            ))
            .map_err(|_| "HTTP timeout setup failed")?;
        let n = stream.read(&mut b).map_err(|_| "HTTP read failed")?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&b[..n]);
        if data.len() > 1024 * 1024 {
            return Err("HTTP response limit".into());
        }
    }
    decode_http(&data)
}
fn decode_http(data: &[u8]) -> Result<String, String> {
    if data.len() > 1024 * 1024 {
        return Err("HTTP response limit".into());
    }
    let split = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("Invalid HTTP framing")?;
    if split > 16384 {
        return Err("HTTP header limit".into());
    }
    let head = std::str::from_utf8(&data[..split]).map_err(|_| "Invalid HTTP header")?;
    if !head
        .lines()
        .next()
        .is_some_and(|l| l.split_whitespace().nth(1) == Some("200"))
    {
        return Err("HTTP non-200 response".into());
    }
    if head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:"))
    {
        return Err("Unsupported HTTP transfer encoding".into());
    }
    let mut length = None;
    for line in head.lines().skip(1) {
        let (key, value) = line.split_once(':').ok_or("Invalid HTTP header field")?;
        if key.eq_ignore_ascii_case("content-length") {
            let n = value
                .trim()
                .parse::<usize>()
                .map_err(|_| "Invalid HTTP content length")?;
            if length.replace(n).is_some() {
                return Err("Duplicate HTTP content length".into());
            }
        }
        if key.eq_ignore_ascii_case("content-encoding")
            && !value.trim().eq_ignore_ascii_case("identity")
        {
            return Err("Unsupported HTTP content encoding".into());
        }
    }
    if length.is_some_and(|n| n != data.len() - split - 4) {
        return Err("Incomplete HTTP body".into());
    }
    String::from_utf8(data[split + 4..].to_vec()).map_err(|_| "HTTP response is not UTF-8".into())
}
fn process_config_endpoint(
    ctx: &ProcessContext,
    table: &str,
    key: &str,
    path: &str,
) -> Option<String> {
    let value = ctx.config.as_ref()?.get(table)?.get(key)?.as_str()?;
    let address = value.strip_prefix("tcp://").unwrap_or(value);
    // The listener may bind all interfaces while collection still connects
    // only to loopback, on the exact configured port with owner verification.
    let address = if address.starts_with(':') {
        format!("127.0.0.1{address}")
    } else {
        address.replacen("localhost:", "127.0.0.1:", 1)
    };
    let mut socket = address.parse::<std::net::SocketAddr>().ok()?;
    if socket.ip().is_unspecified() {
        socket.set_ip(if socket.is_ipv4() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        });
    }
    let url = format!("http://{socket}{path}");
    metrics::loopback_target(&url).ok()?;
    Some(url)
}
fn collect_metrics(
    options: &CollectOptions,
    out: &mut SnapshotV1,
    app: &Option<(u32, ProcessContext)>,
    fibre: &Option<(u32, ProcessContext)>,
    start: Instant,
) {
    if let Some(path) = options.metrics_file.as_ref() {
        match fixture::read_bounded(path, 1024 * 1024)
            .and_then(|b| String::from_utf8(b).map_err(|_| "metrics encoding".into()))
        {
            Ok(t) => {
                out.metrics = metrics::parse_metrics(
                    &t,
                    "supplied metrics file; identity and freshness unverified",
                    false,
                )
            }
            Err(_) => issue(
                out,
                "metrics",
                "METRICS_FILE_UNAVAILABLE",
                "Supplied metrics file could not be read within evidence limits.",
            ),
        }
        return;
    }
    let mut requests = Vec::new();
    if let Some(url) = options.metrics_url.clone() {
        requests.push((url, "explicit Fibre loopback endpoint", false));
    } else if let Some(url) = discover_otel(options, fibre, start) {
        requests.push((url,"Fibre OTLP receiver and Prometheus exporter share verified local collector pipeline; individual series identity unverified",false));
    } else if fibre.is_some() {
        issue(out,"metrics","OTEL_DISCOVERY_UNSUPPORTED","No unambiguous same-namespace OTel receiver/config/exporter chain was established; no port was guessed.");
    }
    if let Some((pid, ctx)) = app {
        if same_net_namespace(*pid) {
            if let Some(url) = process_config_endpoint(
                ctx,
                "instrumentation",
                "prometheus_listen_addr",
                "/metrics",
            ) {
                if !requests.iter().any(|(existing, _, _)| existing == &url) {
                    requests.push((url, "app configured loopback Prometheus endpoint", true));
                }
            }
        }
    }
    if requests.is_empty() {
        issue(out,"metrics","METRICS_NOT_AVAILABLE","No supported existing metrics endpoint was established; Fibre OTLP push does not imply a pull endpoint.");
        return;
    }
    for (url, source, app_source) in requests {
        let target = match metrics::loopback_target(&url) {
            Ok(t) => t,
            Err(_) => {
                issue(out,"metrics","METRICS_ENDPOINT_REJECTED","Metrics endpoint must be literal loopback HTTP without credentials or redirects.");
                continue;
            }
        };
        let owned = app.as_ref().is_some_and(|(pid, _)| {
            same_net_namespace(*pid) && listener_owned(*pid, target.addr, start)
        });
        if app_source && !owned {
            issue(
                out,
                "metrics",
                "METRICS_OWNERSHIP_UNKNOWN",
                "Configured app listener ownership could not be verified; request skipped.",
            );
            continue;
        }
        match http_get(&target, start) {
            Ok(t) => {
                let m = metrics::parse_metrics(&t, source, owned);
                if out.metrics.samples.is_empty() {
                    out.metrics = m;
                } else {
                    out.metrics.target_verified &= m.target_verified;
                    out.metrics.source.push_str("; ");
                    out.metrics.source.push_str(source);
                    if m.status != CollectionStatus::Complete {
                        out.metrics.status = CollectionStatus::Partial;
                    }
                    out.metrics.samples.extend(m.samples);
                    if out.metrics.samples.len() > 4096 {
                        out.metrics.samples.truncate(4096);
                        out.metrics.status = CollectionStatus::Partial;
                    }
                }
            }
            Err(_) => issue(
                out,
                "metrics",
                "METRICS_READ_FAILED",
                "Endpoint did not yield a bounded supported response.",
            ),
        }
    }
}
pub(super) fn collect_live(options: &CollectOptions) -> Result<(), String> {
    let start = Instant::now();
    let reader = Reader {
        root: PathBuf::from("/"),
        fixture: false,
    };
    let mut out = SnapshotV1::default();
    out.capture.started_epoch_seconds = Some(epoch());
    checkpoint(&mut out, start, false)?;
    fixture::read_host(&reader, &mut out);
    out.host.architecture = Some(std::env::consts::ARCH.into());
    out.host.root_filesystem = filesystem(Path::new("/"));
    if let Some(profile) = options.profile.as_ref() {
        issue(
            &mut out,
            "profile",
            "PROFILE_SUPPLIED",
            &format!(
                "User supplied profile {}; it does not prove the running version.",
                profile.chars().take(64).collect::<String>()
            ),
        );
    }
    checkpoint(&mut out, start, false)?;
    let mut fibre = None;
    let mut app = None;
    const PROPS:&str="--property=ActiveState,SubState,MainPID,NRestarts,ExecMainStartTimestampMonotonic,CPUUsageNSec,ControlGroup";
    for (unit, role, home) in [
        (&options.fibre_unit, "fibre", options.fibre_home.as_deref()),
        (&options.app_unit, "app", options.app_home.as_deref()),
    ] {
        if !fixture::valid_unit(unit) {
            return Err("Invalid service unit name".into());
        }
        let props = supervisor::run(
            "/usr/bin/systemctl",
            &["show", "--no-pager", PROPS, unit],
            remaining(start, Duration::from_secs(1)),
            16384,
        )
        .ok()
        .map(|b| parsers::properties(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
        let (mut service, ctx) = fixture::process_evidence(&reader, &props, role, home);
        if let Some(pid) = service.identity.pid {
            if let Some(cg) = service.identity.cgroup_path.as_ref() {
                service.identity.cgroup_inode =
                    fs::metadata(format!("/sys/fs/cgroup{}", cg.trim_end_matches('/')))
                        .ok()
                        .map(|m| m.ino());
            }
            let inspected = fds(pid, remaining(start, Duration::from_millis(300)), false);
            service.fd_count = inspected.complete.then_some(inspected.count);
            service.fd_inspection_complete = inspected.complete;
            if let Ok(mut exe) = File::open(format!("/proc/{pid}/exe")) {
                service.version = version::from_elf(&mut exe);
            }
            if service.version.is_none() {
                issue(&mut out,role,"VERSION_UNPROVEN","Bounded Go ELF build information did not establish a supported application version; no service binary was executed.");
            }
            service.data_filesystem = if role == "fibre" {
                if layout_supported(&service, options.profile.as_deref()) {
                    store_path(pid, &ctx).and_then(|p| filesystem(&p))
                } else {
                    None
                }
            } else {
                home_path(pid, &ctx).and_then(|p| filesystem(&p.join("data")))
            };
            if role == "fibre" {
                if let Some(cfg) = ctx.config.as_ref() {
                    out.fibre_config = parsers::fibre_config(cfg, &ctx.args);
                } else {
                    issue(
                        &mut out,
                        "fibre_config",
                        "CONFIG_UNAVAILABLE",
                        "Home or bounded TOML could not be resolved; no defaults asserted.",
                    );
                }
                fibre = Some((pid, ctx));
            } else {
                app = Some((pid, ctx));
            }
        }
        // The /proc reads established only this section's identity. Keep the
        // whole-capture claim unknown until the final recheck below.
        service.identity.stable_during_capture = None;
        out.services.insert(unit.clone(), service);
        checkpoint(&mut out, start, false)?;
    }
    // Every collection attempts both directories; unresolved paths remain explicitly missing.
    for label in ["staging", "shards"] {
        let supported = out
            .services
            .values()
            .find(|s| s.role == "fibre")
            .is_some_and(|s| layout_supported(s, options.profile.as_deref()));
        let scan = if !supported {
            ScanResult {
                directory: label.into(),
                status: CollectionStatus::Unsupported,
                stop_reason: "storage_layout_version_unproven_or_unsupported".into(),
                ..Default::default()
            }
        } else if let Some(path) = fibre.as_ref().and_then(|(pid, ctx)| store_path(*pid, ctx)) {
            scanner::supervised(
                &path.join(label),
                label,
                remaining(start, Duration::from_secs(2)),
            )
        } else {
            ScanResult {
                directory: label.into(),
                status: CollectionStatus::Missing,
                stop_reason: "store_path_unresolved".into(),
                ..Default::default()
            }
        };
        out.scans.push(scan);
        checkpoint(&mut out, start, false)?;
    }
    if options.inspect_open_fds {
        if let Some((pid, _)) = fibre.as_ref() {
            let inspected = fds(*pid, remaining(start, Duration::from_millis(300)), true);
            for scan in &mut out.scans {
                for file in &mut scan.aged_files {
                    file.has_open_fd = fd_open_state(&inspected, file.device_id, file.inode);
                }
            }
        }
    }
    checkpoint(&mut out, start, false)?;
    if let Some((pid, ctx)) = app.as_ref() {
        if let Some(url) = process_config_endpoint(ctx, "rpc", "laddr", "/status") {
            if let Ok(target) = metrics::loopback_target(&url) {
                if same_net_namespace(*pid) && listener_owned(*pid, target.addr, start) {
                    if let Ok(text) = http_get(&target, start) {
                        out.app_status = metrics::parse_app_status(&text);
                    }
                }
            }
        }
    }
    collect_metrics(options, &mut out, &app, &fibre, start);
    checkpoint(&mut out, start, false)?;
    for service in out.services.values_mut() {
        if let Some(pid) = service.identity.pid {
            let now = reader
                .text(&format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|s| parsers::starttime(&s));
            service.identity.stable_during_capture = service
                .identity
                .starttime_ticks
                .zip(now)
                .map(|(a, b)| a == b);
            if service.identity.stable_during_capture != Some(true) {
                service.status = CollectionStatus::Partial;
            }
        }
    }
    if out
        .services
        .values()
        .any(|s| s.identity.stable_during_capture != Some(true))
    {
        out.metrics.target_verified = false;
        issue(
            &mut out,
            "process",
            "PROCESS_CHANGED",
            "Process identity changed during capture; cross-section conclusions are unsafe.",
        );
    }
    if !out
        .services
        .values()
        .find(|s| s.role == "app")
        .is_some_and(|s| s.identity.stable_during_capture == Some(true))
        && out.app_status.status == CollectionStatus::Complete
    {
        out.app_status.status = CollectionStatus::Partial;
    }
    let fibre_stable = out
        .services
        .values()
        .find(|s| s.role == "fibre")
        .is_some_and(|s| s.identity.stable_during_capture == Some(true));
    if !fibre_stable {
        for scan in &mut out.scans {
            for file in &mut scan.aged_files {
                file.has_open_fd = None;
            }
        }
    }
    out.capture.finished_epoch_seconds = Some(epoch());
    checkpoint(&mut out, start, true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fd_identity_matches_inode_without_reading_contents() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let m = file.as_file().metadata().unwrap();
        let result = fds(std::process::id(), Duration::from_secs(1), true);
        assert!(result.ids.contains(&(m.dev(), m.ino())));
        assert!(result.count > 0);
    }
    #[test]
    fn filesystem_reports_available_not_free() {
        let f = filesystem(Path::new("/")).unwrap();
        assert!(f.total_bytes.unwrap() >= f.available_bytes.unwrap());
        assert!(f.device_id.is_some());
    }
    #[test]
    fn fd_negative_requires_complete_enumeration() {
        let mut f = Fds {
            ids: HashSet::new(),
            count: 0,
            complete: false,
        };
        assert_eq!(fd_open_state(&f, 1, 2), None);
        f.ids.insert((1, 2));
        assert_eq!(fd_open_state(&f, 1, 2), Some(true));
        assert_eq!(fd_open_state(&f, 1, 3), None);
        f.complete = true;
        assert_eq!(fd_open_state(&f, 1, 3), Some(false));
    }
    #[test]
    fn pinned_store_is_directly_under_home_and_ignores_invented_toml_path() {
        let ctx = ProcessContext {
            args: vec![],
            env: Default::default(),
            home: Some("/srv/fibre".into()),
            config: Some("[store]\npath='/wrong'".parse().unwrap()),
            starttime: None,
        };
        assert_eq!(
            store_path(42, &ctx),
            Some(PathBuf::from("/proc/42/root/srv/fibre"))
        );
    }
    #[test]
    fn explicit_profile_does_not_override_different_running_version() {
        let mut s = ServiceSnapshot::default();
        assert!(!layout_supported(&s, None));
        assert!(layout_supported(&s, Some("v10.4.0-mocha")));
        s.version = Some(VersionEvidence {
            version: "v11.0.0".into(),
            ..Default::default()
        });
        assert!(!layout_supported(&s, Some("v10.4.0-mocha")));
    }
    #[test]
    fn wildcard_app_listener_is_read_only_through_loopback() {
        let ctx = ProcessContext {
            args: vec![],
            env: Default::default(),
            home: None,
            config: Some(
                "[instrumentation]\nprometheus_listen_addr=':26660'"
                    .parse()
                    .unwrap(),
            ),
            starttime: None,
        };
        assert_eq!(
            process_config_endpoint(
                &ctx,
                "instrumentation",
                "prometheus_listen_addr",
                "/metrics"
            )
            .as_deref(),
            Some("http://127.0.0.1:26660/metrics")
        );
    }
    #[test]
    fn truncated_http_metrics_are_not_complete_evidence() {
        assert!(decode_http(b"HTTP/1.0 200 OK\r\nContent-Length: 20\r\n\r\nmetric 1\n").is_err());
        assert!(decode_http(b"HTTP/1.0 302 Found\r\nLocation: http://remote/\r\n\r\n").is_err());
        assert_eq!(
            decode_http(b"HTTP/1.0 200 OK\r\nContent-Length: 9\r\n\r\nmetric 1\n").unwrap(),
            "metric 1\n"
        );
        assert!(decode_http(b"HTTP/1.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").is_err());
    }
}
