use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConfigSource {
    ObservedRuntime,
    ConfigFile,
    DefaultAssumed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Inconclusive,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CollectionStatus {
    Complete,
    Partial,
    Timeout,
    Missing,
    Error,
    Unsupported,
    Skipped,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProcessIdentityMatch {
    Verified,
    HeuristicLegacy,
    Mismatch,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Capture {
    pub started_epoch_seconds: Option<f64>,
    pub finished_epoch_seconds: Option<f64>,
    pub monotonic_ns: Option<u64>,
    pub host_hash: Option<String>,
    pub boot_hash: Option<String>,
    pub input_format: String,
    pub supplied_provenance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Issue {
    pub section: String,
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "bytes", rename_all = "snake_case")]
pub enum MemoryLimit {
    Finite(u64),
    Unlimited,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProcessIdentity {
    pub pid: Option<u32>,
    pub starttime_ticks: Option<u64>,
    pub service_start_monotonic_us: Option<u64>,
    pub restart_count: Option<u64>,
    pub stable_during_capture: Option<bool>,
    pub cgroup_path: Option<String>,
    pub cgroup_inode: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct VersionEvidence {
    pub version: String,
    pub commit: Option<String>,
    pub core_version: Option<String>,
    pub source: ConfigSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ServiceSnapshot {
    pub role: String,
    pub status: CollectionStatus,
    pub active_state: Option<String>,
    pub sub_state: Option<String>,
    pub identity: ProcessIdentity,
    pub version: Option<VersionEvidence>,
    pub rss_bytes: Option<u64>,
    pub memory_current_bytes: Option<u64>,
    pub memory_peak_bytes: Option<u64>,
    pub memory_anon_bytes: Option<u64>,
    pub memory_file_bytes: Option<u64>,
    pub memory_kernel_bytes: Option<u64>,
    pub memory_max: Option<MemoryLimit>,
    pub memory_high: Option<MemoryLimit>,
    pub ancestor_limits: BTreeMap<String, MemoryLimit>,
    pub ancestor_limits_complete: bool,
    pub cpu_usage_ns: Option<u64>,
    pub memory_events: BTreeMap<String, u64>,
    pub fd_count: Option<u64>,
    pub fd_soft_limit: Option<MemoryLimit>,
    pub fd_hard_limit: Option<MemoryLimit>,
    pub fd_inspection_complete: bool,
    pub data_filesystem: Option<FilesystemSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HostSnapshot {
    pub memory_total_bytes: Option<u64>,
    pub memory_available_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub swap_free_bytes: Option<u64>,
    pub cpu_count: Option<u32>,
    pub architecture: Option<String>,
    pub root_filesystem: Option<FilesystemSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FilesystemSnapshot {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub total_inodes: Option<u64>,
    pub available_inodes: Option<u64>,
    pub device_id: Option<u64>,
    pub mount_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigValue {
    pub value: serde_json::Value,
    pub source: ConfigSource,
    #[serde(default)]
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ResourceUsage {
    pub elapsed_ms: f64,
    pub user_cpu_ms: f64,
    pub sys_cpu_ms: f64,
    pub max_rss_bytes: u64,
    pub input_block_operations: Option<u64>,
    pub complete: bool,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgedFile {
    pub device_id: u64,
    pub inode: u64,
    pub apparent_bytes: u64,
    pub allocated_bytes: u64,
    pub age_seconds: u64,
    pub has_open_fd: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScanResult {
    pub directory: String,
    pub status: CollectionStatus,
    pub observed_apparent_bytes: Option<u64>,
    pub observed_allocated_bytes: Option<u64>,
    pub entries_visited: u64,
    pub files_counted: u64,
    pub aged_apparent_bytes: u64,
    pub aged_allocated_bytes: u64,
    pub aged_files_count: u64,
    pub aged_files: Vec<AgedFile>,
    pub aged_details_truncated: bool,
    pub skipped_symlinks: u64,
    pub unexpected_directories: u64,
    pub errors: u64,
    pub complete: bool,
    pub stop_reason: String,
    pub elapsed_ms: f64,
    pub cleanup_pending: bool,
    pub usage: Option<ResourceUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MetricSample {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
    pub exact_integer: Option<u64>,
    pub timestamp_epoch_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MetricsSnapshot {
    pub status: CollectionStatus,
    pub source: String,
    pub collected_epoch_seconds: Option<f64>,
    pub samples: Vec<MetricSample>,
    pub target_verified: bool,
    pub freshness_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppStatus {
    pub status: CollectionStatus,
    pub chain_id: Option<String>,
    pub block_height: Option<u64>,
    pub block_time: Option<String>,
    pub catching_up: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotV1 {
    pub schema_version: u32,
    pub collector_version: String,
    pub capture: Capture,
    #[serde(default)]
    pub host: HostSnapshot,
    #[serde(default)]
    pub services: BTreeMap<String, ServiceSnapshot>,
    #[serde(default)]
    pub fibre_config: BTreeMap<String, ConfigValue>,
    #[serde(default)]
    pub scans: Vec<ScanResult>,
    #[serde(default)]
    pub metrics: MetricsSnapshot,
    #[serde(default)]
    pub app_status: AppStatus,
    #[serde(default)]
    pub issues: Vec<Issue>,
    #[serde(default)]
    pub self_usage: Option<ResourceUsage>,
    #[serde(default)]
    pub worker_usage: Vec<ResourceUsage>,
}
impl Default for SnapshotV1 {
    fn default() -> Self {
        Self {
            schema_version: 1,
            collector_version: env!("CARGO_PKG_VERSION").into(),
            capture: Capture {
                input_format: "v1".into(),
                ..Capture::default()
            },
            host: HostSnapshot::default(),
            services: BTreeMap::new(),
            fibre_config: BTreeMap::new(),
            scans: vec![],
            metrics: MetricsSnapshot::default(),
            app_status: AppStatus::default(),
            issues: vec![],
            self_usage: None,
            worker_usage: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub status: CheckStatus,
    pub title: String,
    pub evidence: Vec<String>,
    pub impact: String,
    pub recommendation: String,
    pub verification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityScenario {
    pub connections: u64,
    pub streams: u64,
    pub receive_buffer_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityModel {
    pub status: CheckStatus,
    pub profile: Option<String>,
    pub profile_source: ConfigSource,
    pub connections: Option<u64>,
    pub streams: Option<u64>,
    pub receive_buffer_bytes: Option<u64>,
    pub scenarios: Vec<CapacityScenario>,
    pub evidence: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDelta {
    pub metric: String,
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub delta: Option<i128>,
    pub rate_per_second: Option<f64>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceComparison {
    pub service: String,
    pub identity_match: ProcessIdentityMatch,
    pub reasons: Vec<String>,
    pub metrics: Vec<MetricDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comparison {
    pub mode: String,
    pub elapsed_seconds: Option<f64>,
    pub clock_basis: String,
    pub services: Vec<ServiceComparison>,
    pub host_metrics: Vec<MetricDelta>,
    pub warnings: Vec<String>,
}
