use super::{epoch, supervisor};
use crate::model::*;
use std::{
    collections::HashSet,
    ffi::{CStr, CString},
    os::unix::ffi::OsStrExt,
    path::Path,
    time::{Duration, Instant},
};

struct Directory(*mut libc::DIR);
impl Drop for Directory {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}
fn open_directory(path: &Path) -> Result<Directory, String> {
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| "Invalid directory path")?;
    // O_NOFOLLOW rejects a substituted final symlink. Once opened, all children are
    // resolved by fstatat relative to this stable descriptor, never concatenated paths.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(format!("open:{:?}", std::io::Error::last_os_error().kind()));
    }
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        unsafe {
            libc::close(fd);
        }
        return Err("Cannot enumerate directory".into());
    }
    Ok(Directory(dir))
}
pub(super) fn scan<F: FnMut(&ScanResult) -> Result<(), String>>(
    path: &Path,
    label: &str,
    budget: Duration,
    cap: u64,
    mut checkpoint: F,
) -> Result<ScanResult, String> {
    let start = Instant::now();
    let mut out = ScanResult {
        directory: label.into(),
        status: CollectionStatus::Partial,
        stop_reason: "in_progress".into(),
        ..Default::default()
    };
    checkpoint(&out)?;
    let dir = match open_directory(path) {
        Ok(d) => d,
        Err(e) => {
            out.status = if e.contains("NotFound") {
                CollectionStatus::Missing
            } else {
                CollectionStatus::Error
            };
            out.errors = 1;
            out.stop_reason = e;
            return Ok(out);
        }
    };
    let dirfd = unsafe { libc::dirfd(dir.0) };
    let mut root: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(dirfd, &mut root) } != 0 {
        return Err("Cannot identify scan directory".into());
    }
    out.observed_apparent_bytes = Some(0);
    out.observed_allocated_bytes = Some(0);
    let mut seen = HashSet::new();
    let now = epoch() as i64;
    let mut last = Instant::now();
    loop {
        // Emit even when every observed entry is skipped or unreadable.
        if last.elapsed() >= Duration::from_millis(100) {
            out.elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
            out.usage = Some(supervisor::usage(
                start,
                libc::RUSAGE_SELF,
                "storage scanner process only",
                false,
            ));
            checkpoint(&out)?;
            last = Instant::now();
        }
        if start.elapsed() >= budget {
            out.status = CollectionStatus::Timeout;
            out.stop_reason = "time_budget".into();
            break;
        }
        if out.entries_visited >= cap {
            out.stop_reason = "entry_budget".into();
            break;
        }
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(dir.0) };
        if entry.is_null() {
            let err = unsafe { *libc::__errno_location() };
            if err == 0 {
                out.complete = out.errors == 0 && out.unexpected_directories == 0;
                out.status = if out.complete {
                    CollectionStatus::Complete
                } else {
                    CollectionStatus::Partial
                };
                out.stop_reason = "end_of_directory".into();
            } else {
                out.errors += 1;
                out.stop_reason = "enumeration_error".into();
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        out.entries_visited += 1;
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                dirfd,
                name.as_ptr(),
                &mut metadata,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            out.errors += 1;
            continue;
        }
        let mode = metadata.st_mode & libc::S_IFMT;
        if mode == libc::S_IFLNK {
            out.skipped_symlinks += 1;
            continue;
        }
        if mode == libc::S_IFDIR {
            out.unexpected_directories += 1;
            continue;
        }
        if mode != libc::S_IFREG || metadata.st_dev != root.st_dev {
            out.errors += 1;
            continue;
        }
        if !seen.insert((metadata.st_dev, metadata.st_ino)) {
            continue;
        }
        let apparent = metadata.st_size.max(0) as u64;
        let allocated = (metadata.st_blocks.max(0) as u64).saturating_mul(512);
        out.observed_apparent_bytes = Some(
            out.observed_apparent_bytes
                .unwrap()
                .saturating_add(apparent),
        );
        out.observed_allocated_bytes = Some(
            out.observed_allocated_bytes
                .unwrap()
                .saturating_add(allocated),
        );
        out.files_counted += 1;
        let age = now.saturating_sub(metadata.st_mtime).max(0) as u64;
        if label == "staging" && age > 900 {
            out.aged_files_count += 1;
            out.aged_apparent_bytes = out.aged_apparent_bytes.saturating_add(apparent);
            out.aged_allocated_bytes = out.aged_allocated_bytes.saturating_add(allocated);
            if out.aged_files.len() < 128 {
                out.aged_files.push(AgedFile {
                    device_id: metadata.st_dev as u64,
                    inode: metadata.st_ino as u64,
                    apparent_bytes: apparent,
                    allocated_bytes: allocated,
                    age_seconds: age,
                    has_open_fd: None,
                });
            } else {
                out.aged_details_truncated = true;
            }
        }
        if last.elapsed() >= Duration::from_millis(100) || out.entries_visited % 512 == 0 {
            out.elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
            out.usage = Some(supervisor::usage(
                start,
                libc::RUSAGE_SELF,
                "storage scanner process only",
                false,
            ));
            checkpoint(&out)?;
            last = Instant::now();
        }
    }
    out.elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
    out.usage = Some(supervisor::usage(
        start,
        libc::RUSAGE_SELF,
        "storage scanner process only",
        true,
    ));
    Ok(out)
}
pub(super) fn worker_entry(args: &[String]) -> Result<(), String> {
    if args.len() != 4 || !["staging", "shards"].contains(&args[1].as_str()) {
        return Err("Invalid scan worker arguments".into());
    }
    let millis = args[2]
        .parse::<u64>()
        .map_err(|_| "Invalid scan budget")?
        .min(2000);
    let cap = args[3]
        .parse::<u64>()
        .map_err(|_| "Invalid scan cap")?
        .min(50000);
    let out = scan(
        Path::new(&args[0]),
        &args[1],
        Duration::from_millis(millis),
        cap,
        supervisor::emit,
    )?;
    supervisor::emit(&out)
}
pub(super) fn supervised(path: &Path, label: &str, budget: Duration) -> ScanResult {
    let fallback = || ScanResult {
        directory: label.into(),
        status: CollectionStatus::Error,
        stop_reason: "scanner unavailable".into(),
        ..Default::default()
    };
    let Ok(exe) = std::env::current_exe() else {
        return fallback();
    };
    let child = supervisor::command(exe.as_os_str())
        .arg("__scan-worker")
        .arg(path)
        .arg(label)
        .arg(budget.as_millis().min(2000).to_string())
        .arg("50000")
        .spawn();
    let Ok(child) = child else {
        return fallback();
    };
    let result = supervisor::monitor::<ScanResult>(child, budget, false, 262144);
    let mut scan = result.latest.unwrap_or_else(fallback);
    scan.cleanup_pending = result.cleanup_pending;
    if result.timed_out {
        scan.complete = false;
        scan.status = CollectionStatus::Timeout;
        scan.stop_reason = "time_budget_parent".into();
        if let Some(u) = scan.usage.as_mut() {
            u.complete = false;
        }
    }
    if result.invalid || (!result.success && !result.timed_out) {
        scan.complete = false;
        scan.status = CollectionStatus::Partial;
        scan.stop_reason = "worker_incomplete".into();
    }
    scan
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    #[test]
    fn counts_hardlinks_once_and_never_traverses_links_or_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), b"1234567").unwrap();
        fs::hard_link(dir.path().join("a"), dir.path().join("b")).unwrap();
        symlink("/", dir.path().join("link")).unwrap();
        fs::create_dir(dir.path().join("unexpected")).unwrap();
        let out = scan(dir.path(), "shards", Duration::from_secs(2), 50000, |_| {
            Ok(())
        })
        .unwrap();
        assert_eq!(out.files_counted, 1);
        assert_eq!(out.observed_apparent_bytes, Some(7));
        assert_eq!(out.skipped_symlinks, 1);
        assert_eq!(out.unexpected_directories, 1);
        assert!(!out.complete);
    }
    #[test]
    fn entry_budget_counts_nonfiles_and_retains_partial_values() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            fs::write(dir.path().join(i.to_string()), b"a").unwrap();
        }
        let out = scan(dir.path(), "staging", Duration::from_secs(2), 3, |_| Ok(())).unwrap();
        assert_eq!(out.entries_visited, 3);
        assert_eq!(out.observed_apparent_bytes, Some(3));
        assert!(!out.complete);
        assert_eq!(out.stop_reason, "entry_budget");
    }
    #[test]
    fn final_directory_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        symlink(dir.path(), dir.path().join("link")).unwrap();
        let out = scan(
            &dir.path().join("link"),
            "shards",
            Duration::from_secs(2),
            10,
            |_| Ok(()),
        )
        .unwrap();
        assert_ne!(out.status, CollectionStatus::Complete);
    }
    #[test]
    fn missing_directory_does_not_report_zero_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let out = scan(
            &dir.path().join("absent"),
            "staging",
            Duration::from_secs(2),
            10,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(out.status, CollectionStatus::Missing);
        assert_eq!(out.observed_apparent_bytes, None);
        assert_eq!(out.observed_allocated_bytes, None);
    }
    #[test]
    fn sparse_aged_file_has_separate_apparent_and_allocated_bytes() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let f = fs::File::create(dir.path().join("sparse")).unwrap();
        f.set_len(1024 * 1024).unwrap();
        let t = libc::timespec {
            tv_sec: epoch() as i64 - 1800,
            tv_nsec: 0,
        };
        assert_eq!(unsafe { libc::futimens(f.as_raw_fd(), [t, t].as_ptr()) }, 0);
        let out = scan(
            dir.path(),
            "staging",
            Duration::from_secs(2),
            10,
            |_| Ok(()),
        )
        .unwrap();
        assert!(out.complete);
        assert_eq!(out.observed_apparent_bytes, Some(1024 * 1024));
        assert!(out.observed_allocated_bytes.unwrap() < 1024 * 1024);
        assert_eq!(out.aged_files_count, 1);
        assert_eq!(out.aged_files[0].has_open_fd, None);
    }
    #[test]
    fn permission_denied_is_missing_evidence() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o0)).unwrap();
        let out = scan(
            dir.path(),
            "staging",
            Duration::from_secs(2),
            10,
            |_| Ok(()),
        )
        .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(out.status, CollectionStatus::Error);
            assert_eq!(out.observed_apparent_bytes, None);
        } else {
            assert!(out.complete, "root may bypass directory read permission");
        }
    }
    #[test]
    fn zero_time_budget_is_partial_not_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), b"123").unwrap();
        let out = scan(dir.path(), "staging", Duration::ZERO, 10, |_| Ok(())).unwrap();
        assert_eq!(out.status, CollectionStatus::Timeout);
        assert!(!out.complete);
        assert_eq!(out.entries_visited, 0);
    }
}
