use super::{issue, CollectOptions};
use crate::model::*;
use serde::de::DeserializeOwned;
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(super) const TOTAL: Duration = Duration::from_secs(20);
pub(super) const GRACE: Duration = Duration::from_millis(100);
pub(super) fn emit<T: serde::Serialize>(value: &T) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| "Checkpoint encoding failed")?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err("Checkpoint exceeds size limit".into());
    }
    bytes.push(b'\n');
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes)
        .and_then(|_| out.flush())
        .map_err(|_| "Checkpoint channel closed".into())
}
fn nonblocking(child: &Child) -> Result<(), String> {
    let fd = child
        .stdout
        .as_ref()
        .ok_or("Child stdout unavailable")?
        .as_raw_fd();
    let old = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if old < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, old | libc::O_NONBLOCK) } < 0 {
        return Err("Cannot make child pipe nonblocking".into());
    }
    Ok(())
}
pub(super) fn command(program: &std::ffi::OsStr) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C");
    c
}
pub(super) fn kill_and_reap(child: &mut Child, group: bool) -> bool {
    // Never wait for EOF after kill: a D-state child can retain this pipe.
    child.stdout.take();
    if group {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    } else {
        let _ = child.kill();
    }
    bounded_reap(|| child.try_wait().map(|v| v.is_some()), GRACE)
}
fn bounded_reap<F: FnMut() -> std::io::Result<bool>>(mut exited: F, grace: Duration) -> bool {
    let end = Instant::now() + grace;
    loop {
        match exited() {
            Ok(true) => return false,
            Err(_) => return true,
            _ => {}
        }
        if Instant::now() >= end {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
pub(super) struct Monitored<T> {
    pub latest: Option<T>,
    pub timed_out: bool,
    pub cleanup_pending: bool,
    pub success: bool,
    pub invalid: bool,
}
pub(super) fn monitor<T: DeserializeOwned>(
    mut child: Child,
    budget: Duration,
    group: bool,
    cap: usize,
) -> Monitored<T> {
    let mut out = Monitored {
        latest: None,
        timed_out: false,
        cleanup_pending: false,
        success: false,
        invalid: false,
    };
    if nonblocking(&child).is_err() {
        out.invalid = true;
        out.cleanup_pending = kill_and_reap(&mut child, group);
        return out;
    }
    let end = Instant::now() + budget;
    let mut pending = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        // Drain a bounded batch so a noisy child cannot starve deadline checks.
        for _ in 0..32 {
            match child.stdout.as_mut().unwrap().read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buffer[..n]);
                    while let Some(i) = pending.iter().position(|b| *b == b'\n') {
                        if i > cap {
                            out.invalid = true;
                            break;
                        }
                        if let Ok(value) = serde_json::from_slice::<T>(&pending[..i]) {
                            out.latest = Some(value);
                        } else {
                            out.invalid = true;
                        }
                        pending.drain(..=i);
                    }
                    if pending.len() > cap || out.invalid {
                        out.invalid = true;
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    out.invalid = true;
                    break;
                }
            }
        }
        if out.invalid {
            out.cleanup_pending = kill_and_reap(&mut child, group);
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                out.success = status.success();
                // Child has exited; drain remaining pipe bytes without blocking.
                loop {
                    match child.stdout.as_mut().unwrap().read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            pending.extend_from_slice(&buffer[..n]);
                            if pending.len() > cap {
                                out.invalid = true;
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                for line in pending.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                    match serde_json::from_slice(line) {
                        Ok(value) => out.latest = Some(value),
                        Err(_) => out.invalid = true,
                    }
                }
                break;
            }
            Err(_) => {
                out.invalid = true;
                out.cleanup_pending = kill_and_reap(&mut child, group);
                break;
            }
            _ => {}
        }
        if Instant::now() >= end {
            out.timed_out = true;
            out.cleanup_pending = kill_and_reap(&mut child, group);
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    out
}
pub(super) fn run(
    program: &str,
    args: &[&str],
    budget: Duration,
    cap: usize,
) -> Result<Vec<u8>, String> {
    if budget.is_zero() {
        return Err("collection deadline".into());
    }
    let mut child = command(std::ffi::OsStr::new(program))
        .args(args)
        .spawn()
        .map_err(|_| "command unavailable")?;
    if let Err(e) = nonblocking(&child) {
        kill_and_reap(&mut child, false);
        return Err(e);
    }
    let end = Instant::now() + budget;
    let mut out = Vec::new();
    let mut b = [0u8; 4096];
    loop {
        for _ in 0..32 {
            match child.stdout.as_mut().unwrap().read(&mut b) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&b[..n]);
                    if out.len() > cap {
                        kill_and_reap(&mut child, false);
                        return Err("command output limit".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    kill_and_reap(&mut child, false);
                    return Err("command read failed".into());
                }
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            loop {
                match child.stdout.as_mut().unwrap().read(&mut b) {
                    Ok(0) => break,
                    Ok(n) => {
                        out.extend_from_slice(&b[..n]);
                        if out.len() > cap {
                            return Err("command output limit".into());
                        }
                    }
                    Err(_) => break,
                }
            }
            return if status.success() {
                Ok(out)
            } else {
                Err("command failed".into())
            };
        }
        if Instant::now() >= end {
            kill_and_reap(&mut child, false);
            return Err("command timeout".into());
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}
pub(super) fn usage(start: Instant, who: i32, scope: &str, complete: bool) -> ResourceUsage {
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::getrusage(who, &mut r) } == 0;
    ResourceUsage {
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.,
        user_cpu_ms: r.ru_utime.tv_sec as f64 * 1000. + r.ru_utime.tv_usec as f64 / 1000.,
        sys_cpu_ms: r.ru_stime.tv_sec as f64 * 1000. + r.ru_stime.tv_usec as f64 / 1000.,
        max_rss_bytes: (r.ru_maxrss.max(0) as u64).saturating_mul(1024),
        input_block_operations: ok.then_some(r.ru_inblock.max(0) as u64),
        complete: complete && ok,
        scope: scope.into(),
    }
}
pub(super) fn collect_supervised(options: &CollectOptions) -> Result<SnapshotV1, String> {
    let start = Instant::now();
    let exe = std::env::current_exe().map_err(|_| "Cannot locate collector executable")?;
    let encoded = serde_json::to_string(options).map_err(|_| "Cannot encode collector options")?;
    if encoded.len() > 16384 {
        return Err("Collector options exceed limit".into());
    }
    let mut c = command(exe.as_os_str());
    c.arg("__collect-worker").arg(encoded).process_group(0);
    let child = c
        .spawn()
        .map_err(|_| "Cannot start isolated collection worker")?;
    let result = monitor::<SnapshotV1>(child, TOTAL, true, MAX_SNAPSHOT_BYTES);
    let mut snapshot = result.latest.unwrap_or_default();
    if result.timed_out || result.invalid || !result.success {
        invalidate_unfinished_capture(&mut snapshot);
    }
    if result.timed_out {
        issue(
            &mut snapshot,
            "collection",
            "COLLECTION_TIMEOUT",
            "20-second collection deadline reached; last complete checkpoint retained.",
        );
    }
    if result.cleanup_pending {
        issue(
            &mut snapshot,
            "collection",
            "CLEANUP_PENDING",
            "Kill was requested; worker exit was not observed within the 100ms cleanup grace.",
        );
    }
    if result.invalid || (!result.success && !result.timed_out) {
        issue(
            &mut snapshot,
            "collection",
            "COLLECTION_INCOMPLETE",
            "Worker failed or checkpoint was invalid; partial evidence retained.",
        );
    }
    snapshot.self_usage = Some(usage(
        start,
        libc::RUSAGE_SELF,
        "supervisor process only; excludes collection worker and scanners",
        true,
    ));
    if result.timed_out || result.cleanup_pending {
        for u in &mut snapshot.worker_usage {
            u.complete = false;
        }
    }
    snapshot.capture.finished_epoch_seconds = Some(super::epoch());
    Ok(snapshot)
}

fn invalidate_unfinished_capture(snapshot: &mut SnapshotV1) {
    for usage in &mut snapshot.worker_usage {
        usage.complete = false;
    }
    snapshot.metrics.target_verified = false;
    if snapshot.app_status.status == CollectionStatus::Complete {
        snapshot.app_status.status = CollectionStatus::Partial;
    }
    for service in snapshot.services.values_mut() {
        if service.identity.stable_during_capture != Some(false) {
            service.identity.stable_during_capture = None;
        }
        service.status = CollectionStatus::Partial;
    }
    for scan in &mut snapshot.scans {
        for file in &mut scan.aged_files {
            file.has_open_fd = None;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unfinished_capture_withholds_identity_fd_and_rpc_conclusions() {
        let mut snapshot = SnapshotV1 {
            app_status: AppStatus {
                status: CollectionStatus::Complete,
                ..Default::default()
            },
            metrics: MetricsSnapshot {
                target_verified: true,
                ..Default::default()
            },
            ..Default::default()
        };
        snapshot.services.insert(
            "app.service".into(),
            ServiceSnapshot {
                identity: ProcessIdentity {
                    stable_during_capture: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        snapshot.scans.push(ScanResult {
            aged_files: vec![AgedFile {
                has_open_fd: Some(false),
                ..Default::default()
            }],
            ..Default::default()
        });
        invalidate_unfinished_capture(&mut snapshot);
        assert_eq!(snapshot.app_status.status, CollectionStatus::Partial);
        assert!(!snapshot.metrics.target_verified);
        assert_eq!(
            snapshot.services["app.service"]
                .identity
                .stable_during_capture,
            None
        );
        assert_eq!(snapshot.scans[0].aged_files[0].has_open_fd, None);
    }
    #[test]
    fn unkillable_child_simulation_never_blocks_reaping() {
        let start = Instant::now();
        let mut polls = 0;
        assert!(bounded_reap(
            || {
                polls += 1;
                Ok(false)
            },
            Duration::from_millis(15)
        ));
        assert!(polls > 1);
        assert!(start.elapsed() < Duration::from_millis(250));
    }
    #[test]
    fn exited_child_has_no_pending_cleanup() {
        assert!(!bounded_reap(|| Ok(true), GRACE));
    }
    #[test]
    fn usage_measures_rss_bytes_separately_from_virtual_memory() {
        let usage = usage(Instant::now(), libc::RUSAGE_SELF, "test", true);
        assert!(usage.complete);
        assert!(usage.max_rss_bytes > 0);
        assert_eq!(usage.max_rss_bytes % 1024, 0);
    }
    #[test]
    fn timeout_keeps_last_valid_checkpoint() {
        let child = command(std::ffi::OsStr::new("/bin/sh"))
            .args(["-c", "printf '%s\\n' '{\"value\":7}'; exec sleep 5"])
            .spawn()
            .unwrap();
        let start = Instant::now();
        let result = monitor::<serde_json::Value>(child, Duration::from_millis(100), false, 1024);
        assert!(result.timed_out);
        assert_eq!(result.latest.unwrap()["value"], 7);
        assert!(start.elapsed() < Duration::from_millis(500));
    }
    #[test]
    fn partial_json_is_not_a_checkpoint() {
        let child = command(std::ffi::OsStr::new("/bin/sh"))
            .args(["-c", "printf '%s' '{\"value\":'; exec sleep 5"])
            .spawn()
            .unwrap();
        let result = monitor::<serde_json::Value>(child, Duration::from_millis(30), false, 1024);
        assert!(result.timed_out);
        assert!(result.latest.is_none());
    }
}
