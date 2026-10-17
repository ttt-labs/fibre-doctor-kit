//! Bounded, evidence-only collection. Live operations are Linux-only.
use crate::model::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod fixture;
#[cfg(target_os = "linux")]
mod linux;
mod metrics;
mod parsers;
#[cfg(target_os = "linux")]
mod scanner;
#[cfg(target_os = "linux")]
mod supervisor;
#[cfg(any(target_os = "linux", test))]
mod version;
#[cfg(any(target_os = "linux", test))]
mod yaml;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CollectOptions {
    pub sysroot: Option<PathBuf>,
    pub app_unit: String,
    pub fibre_unit: String,
    pub app_home: Option<PathBuf>,
    pub fibre_home: Option<PathBuf>,
    pub metrics_url: Option<String>,
    pub metrics_file: Option<PathBuf>,
    pub otel_config: Option<PathBuf>,
    pub profile: Option<String>,
    pub inspect_open_fds: bool,
}
impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            sysroot: None,
            app_unit: "celestia-appd.service".into(),
            fibre_unit: "fibre.service".into(),
            app_home: None,
            fibre_home: None,
            metrics_url: None,
            metrics_file: None,
            otel_config: None,
            profile: None,
            inspect_open_fds: true,
        }
    }
}

pub fn collect(options: &CollectOptions) -> Result<SnapshotV1, String> {
    if options.sysroot.is_some() {
        return fixture::collect_fixture(options);
    }
    #[cfg(target_os = "linux")]
    {
        supervisor::collect_supervised(options)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("Live collection requires Linux; use analyze or an explicit fixture sysroot.".into())
    }
}

/// Hidden self-reexec entrypoint: directory, display label, timeout milliseconds, entry cap.
pub fn worker_entry(args: &[String]) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        scanner::worker_entry(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("Storage worker requires Linux.".into())
    }
}

/// Hidden collection child. Checkpoints go to stdout, never raw configuration or credentials.
pub fn collection_worker_entry(args: &[String]) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if args.len() != 1 || args[0].len() > 16384 {
            return Err("Invalid collection worker arguments.".into());
        }
        let options: CollectOptions =
            serde_json::from_str(&args[0]).map_err(|_| "Invalid worker options.")?;
        if options.sysroot.is_some() {
            return Err("Fixture collection does not use a live worker.".into());
        }
        linux::collect_live(&options)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("Collection worker requires Linux.".into())
    }
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub(super) fn issue(snapshot: &mut SnapshotV1, section: &str, code: &str, detail: &str) {
    if snapshot.issues.len() < 128 {
        snapshot.issues.push(Issue {
            section: section.into(),
            code: code.into(),
            detail: detail.into(),
        });
    }
}
