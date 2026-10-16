//! Public reports use an explicit projection. Raw snapshots and arbitrary evidence text never
//! enter HTML, JSON, or the localization layer.
use crate::model::*;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const TRANSLATIONS: &str = include_str!("../assets/translations.json");
const TEMPLATE: &str = include_str!("../assets/report.html");
pub const LANGUAGES: [&str; 1] = ["en"];
const FINDING_IDS: &[&str] = &[
    "collection_issues",
    "service_state",
    "process_stability",
    "host_memory",
    "service_memory",
    "cgroup_limits",
    "memory_events",
    "file_descriptors",
    "data_filesystem",
    "storage_scan",
    "aged_files",
    "app_sync",
    "metrics_evidence",
    "signing_latency",
    "storage_budget",
    "shared_data_filesystem",
];

fn dictionary() -> BTreeMap<String, Vec<String>> {
    serde_json::from_str(TRANSLATIONS).expect("embedded translations are tested")
}
fn language_index(lang: &str) -> Result<usize, String> {
    LANGUAGES
        .iter()
        .position(|l| *l == lang)
        .ok_or_else(|| "Unsupported language; use en".into())
}
pub fn translate(key: &str, lang: &str) -> Result<String, String> {
    let index = language_index(lang)?;
    Ok(dictionary()
        .get(key)
        .and_then(|v| v.get(index))
        .cloned()
        .unwrap_or_else(|| key.to_owned()))
}

fn number(n: Option<u64>) -> Value {
    n.map(|n| Value::String(n.to_string()))
        .unwrap_or(Value::Null)
}
fn float(n: Option<f64>) -> Value {
    n.filter(|n| n.is_finite())
        .map(|n| Value::String(n.to_string()))
        .unwrap_or(Value::Null)
}
fn enumeration<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}
fn status_string(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pass => "PASS",
        CheckStatus::Warn => "WARN",
        CheckStatus::Fail => "FAIL",
        CheckStatus::Inconclusive => "INCONCLUSIVE",
        CheckStatus::Unsupported => "UNSUPPORTED",
    }
}
fn safe_version(s: &str) -> Option<&str> {
    let version = s.strip_prefix('v').unwrap_or(s);
    let numeric = version.strip_suffix("-mocha").unwrap_or(version);
    let parts: Vec<_> = numeric.split('.').collect();
    (parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|c| c.is_ascii_digit())))
    .then_some(s)
}
fn known_role(key: &str, service: Option<&ServiceSnapshot>, index: usize) -> String {
    let role = service.map(|s| s.role.as_str()).unwrap_or(key);
    if role == "app" || key == "celestia-appd.service" {
        "app".into()
    } else if role == "fibre" || key == "fibre.service" {
        "fibre".into()
    } else {
        format!("service_{}", index + 1)
    }
}
fn limit(value: &Option<MemoryLimit>) -> Value {
    match value {
        Some(MemoryLimit::Finite(n)) => json!({"kind":"finite","value":n.to_string()}),
        Some(MemoryLimit::Unlimited) => json!({"kind":"unlimited"}),
        None => Value::Null,
    }
}
fn fs(value: &Option<FilesystemSnapshot>) -> Value {
    value.as_ref().map(|v| json!({"total_bytes":number(v.total_bytes), "available_bytes":number(v.available_bytes), "total_inodes":number(v.total_inodes), "available_inodes":number(v.available_inodes)})).unwrap_or(Value::Null)
}
fn usage(value: &Option<ResourceUsage>) -> Value {
    value.as_ref().map(|v| {
        let scope = match v.scope.as_str() {
            "supervisor process only; excludes collection worker and scanners" => "supervisor",
            "collection worker process only; excludes storage scanners and helper commands" => "collector_worker",
            "storage scanner process only" => "storage_scanner",
            _ => "unknown",
        };
        json!({"elapsed_ms":float(Some(v.elapsed_ms)),"user_cpu_ms":float(Some(v.user_cpu_ms)),"sys_cpu_ms":float(Some(v.sys_cpu_ms)),"max_rss_bytes":v.max_rss_bytes.to_string(),"input_block_operations":number(v.input_block_operations),"complete":v.complete,"scope":scope})
    }).unwrap_or(Value::Null)
}

fn scan_stop_reason(reason: &str) -> &'static str {
    match reason {
        "end_of_directory" | "complete" => "end_of_directory",
        "time_budget" | "time_budget_parent" | "deadline" | "timeout" | "PARTIAL_TIMEOUT" => {
            "time_budget"
        }
        "entry_budget" | "entry_limit" | "ENTRY_LIMIT" => "entry_budget",
        "open:PermissionDenied" | "permission_denied" => "permission_denied",
        "open:NotFound" | "missing" => "missing",
        "enumeration_error" => "enumeration_error",
        "worker_incomplete" => "worker_incomplete",
        "store_path_unresolved" => "store_path_unresolved",
        "in_progress" => "in_progress",
        "scanner unavailable" => "scanner_unavailable",
        _ => "unknown",
    }
}

/// Prometheus names are normalized to known suffixes; labels and arbitrary namespaces are private.
fn public_metric_name(name: &str) -> Option<&'static str> {
    const SUFFIXES: &[&str] = &[
        "signing_latency_median_seconds",
        "signing_latency_seconds_count",
        "signing_latency_seconds_sum",
        "occupancy_bytes",
        "budget_bytes",
        "prune_errors_total",
        "prune_error_total",
        "prune_success_total",
        "prune_total",
        "prune_last_success_timestamp_seconds",
        "last_success_timestamp_seconds",
        "rejected_total",
        "rejected_bytes_total",
    ];
    SUFFIXES.iter().copied().find(|suffix| {
        name == *suffix || name.strip_suffix(*suffix).is_some_and(|p| p.ends_with('_'))
    })
}

/// This is deliberately a projection, not a redaction pass over serialized SnapshotV1.
pub fn public_snapshot(snapshot: &SnapshotV1) -> Value {
    let services: Vec<Value> = snapshot.services.iter().enumerate().map(|(i,(key,s))| {
        let events: BTreeMap<&str, String> = ["low", "high", "max", "oom", "oom_kill", "oom_group_kill"].into_iter().filter_map(|key| s.memory_events.get(key).map(|v| (key,v.to_string()))).collect();
        let active = s.active_state.as_deref().filter(|v| ["active","inactive","failed","activating","deactivating","reloading"].contains(v));
        let ancestor_limits: Vec<Value> = s.ancestor_limits.values().map(|v| limit(&Some(v.clone()))).collect();
        json!({"role":known_role(key,Some(s),i),"collection_status":enumeration(s.status),"active_state":active,
            "version":s.version.as_ref().and_then(|v|safe_version(&v.version)),"version_source":s.version.as_ref().map(|v|v.source),
            "stable_during_capture":s.identity.stable_during_capture,"restart_count":number(s.identity.restart_count),
            "rss_bytes":number(s.rss_bytes),"memory_current_bytes":number(s.memory_current_bytes),"memory_peak_bytes":number(s.memory_peak_bytes),
            "memory_anon_bytes":number(s.memory_anon_bytes),"memory_file_bytes":number(s.memory_file_bytes),"memory_kernel_bytes":number(s.memory_kernel_bytes),
            "memory_max":limit(&s.memory_max),"memory_high":limit(&s.memory_high),"ancestor_limits":ancestor_limits,"ancestor_limits_complete":s.ancestor_limits_complete,
            "cpu_usage_ns":number(s.cpu_usage_ns),"memory_events":events,"fd_count":number(s.fd_count),"fd_soft_limit":limit(&s.fd_soft_limit),"fd_hard_limit":limit(&s.fd_hard_limit),"fd_inspection_complete":s.fd_inspection_complete,"data_filesystem":fs(&s.data_filesystem)})
    }).collect();
    let mut config = Map::new();
    for key in [
        "root.max_connections",
        "root.max_concurrent_streams",
        "root.min_upload_size",
        "root.upload_verify_workers",
        "root.unlimited_budget",
        "max_connections",
        "max_concurrent_streams",
        "min_upload_size",
        "upload_verify_workers",
        "unlimited_budget",
    ] {
        if let Some(v) = snapshot.fibre_config.get(key) {
            let value = if let Some(n) = v.value.as_u64() {
                json!(n.to_string())
            } else if let Some(b) = v.value.as_bool() {
                json!(b)
            } else {
                Value::Null
            };
            config.insert(key.into(), json!({"value":value,"source":v.source}));
        }
    }
    let scans: Vec<Value> = snapshot.scans.iter().enumerate().map(|(i,s)| {
        let directory = match s.directory.as_str() { "staging" | "shards" => s.directory.clone(), _ => format!("directory_{}",i+1) };
        let open = s.aged_files.iter().filter(|f|f.has_open_fd == Some(true)).count();
        let closed = s.aged_files.iter().filter(|f|f.has_open_fd == Some(false)).count();
        let unknown = s.aged_files.iter().filter(|f|f.has_open_fd.is_none()).count();
        let reason = scan_stop_reason(&s.stop_reason);
        json!({"directory":directory,"status":s.status,"observed_apparent_bytes":number(s.observed_apparent_bytes),"observed_allocated_bytes":number(s.observed_allocated_bytes),"entries_visited":s.entries_visited.to_string(),"files_counted":s.files_counted.to_string(),"aged_apparent_bytes":s.aged_apparent_bytes.to_string(),"aged_allocated_bytes":s.aged_allocated_bytes.to_string(),"aged_files_count":s.aged_files_count.to_string(),"aged_open_fd_true":open.to_string(),"aged_open_fd_false":closed.to_string(),"aged_open_fd_unknown":unknown.to_string(),"aged_details_truncated":s.aged_details_truncated,"skipped_symlinks":s.skipped_symlinks.to_string(),"unexpected_directories":s.unexpected_directories.to_string(),"errors":s.errors.to_string(),"complete":s.complete,"stop_reason":reason,"elapsed_ms":float(Some(s.elapsed_ms)),"cleanup_pending":s.cleanup_pending,"usage":usage(&s.usage)})
    }).collect();
    let metrics: Vec<Value> = snapshot.metrics.samples.iter().filter_map(|s| public_metric_name(&s.name).map(|name|json!({"name":name,"value":s.exact_integer.map(|n|json!(n.to_string())).unwrap_or_else(||float(Some(s.value))),"timestamp_epoch_seconds":float(s.timestamp_epoch_seconds)}))).collect();
    json!({"schema_version":snapshot.schema_version,"collector_version":safe_version(&snapshot.collector_version),
        "capture":{"started_epoch_seconds":float(snapshot.capture.started_epoch_seconds),"finished_epoch_seconds":float(snapshot.capture.finished_epoch_seconds),"input_format":if snapshot.capture.input_format == "legacy_v0" {"legacy_v0"} else {"v1"}},
        "host":{"memory_total_bytes":number(snapshot.host.memory_total_bytes),"memory_available_bytes":number(snapshot.host.memory_available_bytes),"swap_total_bytes":number(snapshot.host.swap_total_bytes),"swap_free_bytes":number(snapshot.host.swap_free_bytes),"cpu_count":snapshot.host.cpu_count.map(|v|v.to_string()),"root_filesystem":fs(&snapshot.host.root_filesystem)},
        "services":services,"config":config,"scans":scans,
        "metrics":{"status":snapshot.metrics.status,"target_verified":snapshot.metrics.target_verified,"freshness_verified":snapshot.metrics.freshness_verified,"samples":metrics},
        "app_status":{"status":snapshot.app_status.status,"block_height":number(snapshot.app_status.block_height),"catching_up":snapshot.app_status.catching_up},
        "collection_issue_count":snapshot.issues.len().to_string(),"self_usage":usage(&snapshot.self_usage),"worker_usage":snapshot.worker_usage.iter().map(|v|usage(&Some(v.clone()))).collect::<Vec<_>>()})
}

const EVIDENCE_KEYS: &[&str] = &[
    "rss_bytes",
    "memory_current_bytes",
    "memory_file_bytes",
    "memory_anon_bytes",
    "memory_kernel_bytes",
    "memory_peak_bytes",
    "fd_count",
    "fd_soft_limit",
    "available_bytes",
    "total_bytes",
    "aged_files_count",
    "apparent_bytes",
    "allocated_bytes",
    "median_seconds",
    "successful_count",
    "age_seconds",
    "count",
    "oom",
    "oom_kill",
    "oom_group_kill",
    "high",
    "max",
    "restart_count",
    "files_counted",
    "entries_visited",
    "count_series",
    "threshold_seconds",
    "median_series_count",
    "sample_count",
    "ancestor_count",
    "effective_limit_bytes",
    "open_file_count",
    "issue_count",
    "connections",
    "streams",
    "buffer_per_stream_bytes",
    "future_timestamp_count",
    "pressure_threshold_bytes",
    "available_inodes",
    "block_height",
    "errors",
    "host_total_bytes",
    "host_available_bytes",
    "occupancy_series_count",
    "budget_series_count",
    "occupancy_bytes",
    "budget_bytes",
];
const BOOLEAN_EVIDENCE_KEYS: &[&str] = &[
    "incomplete",
    "core_version_inferred",
    "target_verified",
    "freshness_verified",
    "fd_evidence_complete",
    "core_supported",
    "core_version_observed",
    "successful_count_valid",
    "profile_supported",
    "profile_explicit_assumption",
    "profile_observed",
    "supported_version_observed",
    "connections_valid",
    "arithmetic_overflow",
    "configuration_complete",
    "ancestor_limits_complete",
    "same_filesystem",
    "catching_up",
    "capture_stable",
    "median_valid",
    "warmup_complete",
    "samples_complete",
    "threshold_supported",
    "chain_scope_known",
    "budget_enforced_observed",
];
fn public_evidence(evidence: &[String]) -> Vec<String> {
    let mut result = BTreeSet::new();
    for item in evidence {
        for token in item.split_whitespace() {
            if let Some((key, value)) = token
                .trim_matches(|c: char| c == ',' || c == ';')
                .split_once('=')
            {
                if EVIDENCE_KEYS.contains(&key)
                    && !value.is_empty()
                    && value.len() <= 48
                    && value
                        .chars()
                        .all(|c| c.is_ascii_digit() || ".-+eE".contains(c))
                    && value.parse::<f64>().is_ok_and(|v| v.is_finite())
                {
                    result.insert(format!("{key}={value}"));
                } else if key == "service"
                    && ["app", "fibre", "fibre.service", "celestia-appd.service"].contains(&value)
                {
                    result.insert(format!(
                        "service={}",
                        if value.starts_with("fibre") {
                            "fibre"
                        } else {
                            "app"
                        }
                    ));
                } else if (BOOLEAN_EVIDENCE_KEYS.contains(&key)
                    && ["true", "false", "unknown"].contains(&value))
                    || (key == "core_version" && ["v0.42.3", "0.42.3"].contains(&value))
                    || (key == "status"
                        && [
                            "active",
                            "inactive",
                            "failed",
                            "activating",
                            "deactivating",
                            "reloading",
                            "complete",
                            "partial",
                            "missing",
                            "error",
                            "unknown",
                            "unsupported",
                            "skipped",
                            "timeout",
                        ]
                        .contains(&value))
                {
                    result.insert(format!("{key}={value}"));
                }
            }
        }
    }
    result.into_iter().collect()
}
pub fn public_findings(findings: &[Finding]) -> Value {
    let dict = dictionary();
    Value::Array(findings.iter().map(|f| {
        let id = if FINDING_IDS.contains(&f.id.as_str()){f.id.as_str()}else{"unknown_finding"};
        let field = |name:&str| dict.get(&format!("finding.{id}.{name}")).map(|v|v[0].clone()).unwrap_or_default();
        json!({"id":id,"status":f.status,"title":field("title"),"impact":field("impact"),"recommendation":field("recommendation"),"verification":field("verification"),"evidence":public_evidence(&f.evidence)})
    }).collect())
}
pub fn public_capacity(capacity: &CapacityModel) -> Value {
    json!({"status":capacity.status,"profile":capacity.profile.as_deref().filter(|v|*v == "v10.4.0-mocha"),"profile_source":capacity.profile_source,"connections":number(capacity.connections),"streams":number(capacity.streams),"receive_buffer_bytes":number(capacity.receive_buffer_bytes),"evidence":public_evidence(&capacity.evidence),"limitations":[translate("capacity_note","en").unwrap_or_default(),translate("capacity_assumption","en").unwrap_or_default()],"scenarios":capacity.scenarios.iter().map(|s|json!({"connections":s.connections.to_string(),"streams":s.streams.to_string(),"receive_buffer_bytes":s.receive_buffer_bytes.to_string()})).collect::<Vec<_>>()})
}
fn reason_code(reason: &str) -> &'static str {
    match reason {
        "monotonic" => "monotonic",
        "estimated_wall_clock" => "estimated_wall_clock",
        "observation missing" | "counter observation missing" | "service observation missing on one side" => "missing_observation",
        "counter delta only; positive compatible duration unavailable" | "No positive compatible capture duration; rates are unavailable." => "invalid_duration",
        "positive compatible duration unavailable; counter delta and rate withheld" => "invalid_duration",
        "temporal gauge continuity or positive duration unavailable; observations shown separately" => "gauge_unverified",
        "counter decreased or reset; delta and rate unavailable" => "counter_reset",
        "difference between observed gauges; does not prove continuous growth" => "gauge_difference",
        "difference between environments; not a temporal change" => "environment_difference",
        "cross-environment counters are not temporal deltas" => "environment_counters",
        "counter continuity is not verified or explicitly accepted" => "counter_unverified",
        "host identity differs" | "boot identity differs" | "main PID differs" | "process start ticks differ" | "service start timestamp differs" | "recorded restart counter differs" => "identity_mismatch",
        "process changed during collection" => "capture_unstable",
        "host, boot, main PID and process start ticks verified" => "identity_verified",
        "legacy PID, service start timestamp, restart counter and cgroup path match" => "legacy_match",
        "host and boot identity and cgroup generation remain unverified" => "legacy_unknown",
        "required identity evidence is missing; absence is not a mismatch" => "identity_missing",
        "legacy continuity assumption explicitly enabled" => "legacy_accepted",
        "legacy counter comparison requires explicit opt-in" => "legacy_not_accepted",
        "cgroup scope differs; cgroup counters cannot be compared" => "cgroup_mismatch",
        "cgroup lifetime continuity is unverified" => "cgroup_unverified",
        "Legacy interval uses wall-clock capture starts; clocks may change and sections were read sequentially." => "estimated_wall_clock",
        _ => "unavailable",
    }
}
fn public_delta(metric: &MetricDelta) -> Option<Value> {
    const KEYS: &[&str] = &[
        "rss_bytes",
        "memory_current_bytes",
        "memory_peak_bytes",
        "memory_anon_bytes",
        "memory_file_bytes",
        "memory_kernel_bytes",
        "memory_available_bytes",
        "memory_total_bytes",
        "swap_free_bytes",
        "root_available_bytes",
        "root_fs_available_bytes",
        "cpu_usage_ns",
        "fd_count",
        "oom",
        "oom_kill",
        "oom_group_kill",
        "high",
        "max",
        "memory_events.low",
        "memory_events.oom",
        "memory_events.oom_kill",
        "memory_events.oom_group_kill",
        "memory_events.high",
        "memory_events.max",
    ];
    let key = metric
        .metric
        .strip_prefix("host.")
        .unwrap_or(&metric.metric);
    KEYS.contains(&key).then(||json!({"metric":metric.metric,"before":number(metric.before),"after":number(metric.after),"delta":metric.delta.map(|v|v.to_string()),"rate_per_second":float(metric.rate_per_second),"reason_code":reason_code(&metric.reason)}))
}
pub fn public_comparison(
    a: &SnapshotV1,
    b: &SnapshotV1,
    comparison: &Comparison,
    findings_a: &[Finding],
    findings_b: &[Finding],
    capacity_a: &CapacityModel,
    capacity_b: &CapacityModel,
) -> Value {
    let services: Vec<Value> = comparison.services.iter().enumerate().map(|(i,s)|json!({"service":known_role(&s.service,a.services.get(&s.service).or_else(||b.services.get(&s.service)),i),"identity_match":s.identity_match,"reason_codes":s.reasons.iter().map(|s|reason_code(s)).collect::<Vec<_>>(),"metrics":s.metrics.iter().filter_map(public_delta).collect::<Vec<_>>()})).collect();
    json!({"report_schema_version":1,"redaction":"allowlisted_public_projection","snapshots":[public_snapshot(a),public_snapshot(b)],"findings":[public_findings(findings_a),public_findings(findings_b)],"capacity":[public_capacity(capacity_a),public_capacity(capacity_b)],"comparison":{"mode":if comparison.mode == "temporal" {"temporal"}else{"side_by_side"},"elapsed_seconds":float(comparison.elapsed_seconds),"clock_basis":match comparison.clock_basis.as_str(){"monotonic"=>"monotonic","estimated_wall_clock"=>"estimated_wall_clock",_=>"unavailable"},"services":services,"host_metrics":comparison.host_metrics.iter().filter_map(public_delta).collect::<Vec<_>>(),"warning_count":comparison.warnings.len().to_string(),"warning_codes":comparison.warnings.iter().map(|s|reason_code(s)).collect::<Vec<_>>()}})
}
fn script_json(value: &Value) -> String {
    serde_json::to_string(value)
        .expect("JSON value serializable")
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}
// Keep each snapshot beside its corresponding findings and capacity model.
#[allow(clippy::too_many_arguments)]
pub fn render_comparison(
    a: &SnapshotV1,
    b: &SnapshotV1,
    comparison: &Comparison,
    findings_a: &[Finding],
    findings_b: &[Finding],
    capacity_a: &CapacityModel,
    capacity_b: &CapacityModel,
    lang: &str,
) -> Result<String, String> {
    language_index(lang)?;
    let report = public_comparison(
        a, b, comparison, findings_a, findings_b, capacity_a, capacity_b,
    );
    let dict: Value = serde_json::from_str(TRANSLATIONS).map_err(|e| e.to_string())?;
    Ok(TEMPLATE
        .replace("__REPORT_JSON__", &script_json(&report))
        .replace("__TRANSLATIONS_JSON__", &script_json(&dict))
        .replace("__INITIAL_LANGUAGE__", lang))
}
pub fn render_doctor_text(findings: &[Finding], lang: &str) -> Result<String, String> {
    let mut result = format!("{}\n\n", translate("doctor_title", lang)?);
    for finding in findings {
        let id = if FINDING_IDS.contains(&finding.id.as_str()) {
            finding.id.as_str()
        } else {
            "unknown_finding"
        };
        result.push_str(&format!(
            "[{}] {} ({})\n",
            status_string(finding.status),
            translate(&format!("finding.{id}.title"), lang)?,
            id
        ));
        for evidence in public_evidence(&finding.evidence) {
            result.push_str(&format!("  {evidence}\n"));
        }
        for field in ["impact", "recommendation", "verification"] {
            result.push_str(&format!(
                "  {}: {}\n",
                translate(field, lang)?,
                translate(&format!("finding.{id}.{field}"), lang)?
            ));
        }
        result.push('\n');
    }
    Ok(result)
}
pub fn render_capacity_text(capacity: &CapacityModel, lang: &str) -> Result<String, String> {
    let mut result = format!(
        "{} [{}]\n{}\n",
        translate("capacity_title", lang)?,
        status_string(capacity.status),
        translate("capacity_note", lang)?
    );
    result.push_str(&format!(
        "profile={} source={}\n",
        capacity
            .profile
            .as_deref()
            .filter(|v| *v == "v10.4.0-mocha")
            .unwrap_or("UNKNOWN"),
        enumeration(capacity.profile_source)
    ));
    if let Some(bytes) = capacity.receive_buffer_bytes {
        result.push_str(&format!(
            "receive_buffer_bytes={bytes} ({:.4} GiB)\n",
            bytes as f64 / 1073741824.0
        ));
    }
    for scenario in &capacity.scenarios {
        result.push_str(&format!(
            "max_connections={} max_concurrent_streams={} receive_buffer_bytes={} ({:.4} GiB)\n",
            scenario.connections,
            scenario.streams,
            scenario.receive_buffer_bytes,
            scenario.receive_buffer_bytes as f64 / 1073741824.0
        ));
    }
    result.push_str(&format!("{}\n", translate("capacity_assumption", lang)?));
    Ok(result)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)] // Tests populate individual evidence sections.
    use super::*;
    fn capacity() -> CapacityModel {
        CapacityModel {
            status: CheckStatus::Inconclusive,
            profile: None,
            profile_source: ConfigSource::Unknown,
            connections: None,
            streams: None,
            receive_buffer_bytes: None,
            scenarios: vec![],
            evidence: vec![],
            limitations: vec![],
        }
    }
    fn comparison() -> Comparison {
        Comparison {
            mode: "temporal".into(),
            elapsed_seconds: Some(83.4120102),
            clock_basis: "wall_clock".into(),
            services: vec![],
            host_metrics: vec![],
            warnings: vec![],
        }
    }
    #[test]
    fn all_report_copy_keys_have_one_nonempty_english_value() {
        let dict = dictionary();
        for (key, values) in &dict {
            assert_eq!(values.len(), 1, "{key}");
            assert!(values.iter().all(|v| !v.trim().is_empty()), "{key}");
        }
        for id in FINDING_IDS.iter().copied().chain(["unknown_finding"]) {
            for field in ["title", "impact", "recommendation", "verification"] {
                assert!(dict.contains_key(&format!("finding.{id}.{field}")));
            }
        }
        for marker in ["t('", "paragraph('", "section('"] {
            for suffix in TEMPLATE.split(marker).skip(1) {
                let key = suffix.split('\'').next().unwrap();
                assert!(
                    dict.contains_key(key),
                    "Missing static UI translation: {key}"
                );
            }
        }
    }
    #[test]
    fn public_projection_omits_arbitrary_text_and_preserves_large_numbers() {
        let secret = "TOP_SECRET_99";
        let mut snapshot = SnapshotV1::default();
        snapshot.capture.host_hash = Some(secret.into());
        snapshot.capture.supplied_provenance = Some(secret.into());
        snapshot.issues.push(Issue {
            section: secret.into(),
            code: secret.into(),
            detail: secret.into(),
        });
        let mut service = ServiceSnapshot::default();
        service.role = secret.into();
        service.identity.cgroup_path = Some(secret.into());
        service.rss_bytes = Some(u64::MAX);
        snapshot.services.insert(secret.into(), service);
        snapshot.metrics.source = secret.into();
        snapshot.metrics.samples.push(MetricSample {
            name: secret.into(),
            labels: BTreeMap::from([("token".into(), secret.into())]),
            value: 1.0,
            ..Default::default()
        });
        let public = public_snapshot(&snapshot).to_string();
        assert!(!public.contains(secret));
        assert!(public.contains(&u64::MAX.to_string()));
    }
    #[test]
    fn html_escapes_script_payload_and_has_no_network_dependencies() {
        assert_eq!(
            script_json(&json!("</script><script>&\u{2028}")),
            "\"\\u003c/script\\u003e\\u003cscript\\u003e\\u0026\\u2028\""
        );
        let report = render_comparison(
            &SnapshotV1::default(),
            &SnapshotV1::default(),
            &comparison(),
            &[],
            &[],
            &capacity(),
            &capacity(),
            "en",
        )
        .unwrap();
        for forbidden in [
            "__REPORT_JSON__",
            "__TRANSLATIONS_JSON__",
            "https://",
            "http://",
            "fetch(",
            "innerHTML",
            "eval(",
            "document.write",
        ] {
            assert!(!report.contains(forbidden), "{forbidden}");
        }
        assert!(report.contains("lang=\"en\""));
        assert!(report.contains("DOCTOR FIBRE KIT"));
        assert!(!report.contains("data-lang"));
        assert!(!report.contains("TTT Fibre"));
    }
    #[test]
    fn evidence_is_allowlisted_and_never_echoes_freeform_credentials() {
        let evidence = vec![
            "service=fibre.service rss_bytes=9007199254740993 token=secret".into(),
            "password=hidden hostname=private.invalid".into(),
        ];
        assert_eq!(
            public_evidence(&evidence),
            vec!["rss_bytes=9007199254740993", "service=fibre"]
        );
    }
    #[test]
    fn invalid_language_is_rejected() {
        for lang in ["<script>", "zh-CN", "ko", "ja"] {
            assert!(render_doctor_text(&[], lang).is_err());
        }
    }
    #[test]
    fn actual_delta_names_and_integer_precision_survive_public_projection() {
        for name in [
            "memory_available_bytes",
            "root_available_bytes",
            "host.memory_available_bytes",
            "host.root_available_bytes",
            "memory_events.oom",
            "memory_events.oom_kill",
        ] {
            let metric = MetricDelta {
                metric: name.into(),
                before: Some(9007199254740993),
                after: Some(9007199254740994),
                delta: Some(1),
                rate_per_second: Some(0.01),
                reason: "monotonic".into(),
            };
            let value = public_delta(&metric).unwrap();
            assert_eq!(value["before"], "9007199254740993");
            assert_eq!(value["delta"], "1");
        }
        let mut snapshot = SnapshotV1::default();
        snapshot.metrics.samples.push(MetricSample {
            name: "fibre_occupancy_bytes".into(),
            value: 9007199254740993u64 as f64,
            exact_integer: Some(9007199254740993),
            ..Default::default()
        });
        assert_eq!(
            public_snapshot(&snapshot)["metrics"]["samples"][0]["value"],
            "9007199254740993"
        );
    }
    #[test]
    fn cost_scope_and_partial_scan_evidence_survive_without_private_text() {
        let mut snapshot = SnapshotV1::default();
        snapshot.self_usage = Some(ResourceUsage {
            scope: "supervisor process only; excludes collection worker and scanners".into(),
            complete: true,
            ..Default::default()
        });
        snapshot.worker_usage.push(ResourceUsage {
            scope: "collection worker process only; excludes storage scanners and helper commands"
                .into(),
            complete: false,
            ..Default::default()
        });
        snapshot.scans.push(ScanResult {
            directory: "staging".into(),
            stop_reason: "time_budget_parent".into(),
            cleanup_pending: true,
            usage: Some(ResourceUsage {
                scope: "storage scanner process only".into(),
                complete: false,
                ..Default::default()
            }),
            ..Default::default()
        });
        let value = public_snapshot(&snapshot);
        assert_eq!(value["self_usage"]["scope"], "supervisor");
        assert_eq!(value["worker_usage"][0]["scope"], "collector_worker");
        assert_eq!(value["worker_usage"][0]["complete"], false);
        assert_eq!(value["scans"][0]["usage"]["scope"], "storage_scanner");
        assert_eq!(value["scans"][0]["stop_reason"], "time_budget");
        assert_eq!(value["scans"][0]["cleanup_pending"], true);
        assert!(TEMPLATE.contains("snapshot.worker_usage"));
    }
    #[test]
    fn all_reason_and_stop_codes_have_english_copy() {
        let dict = dictionary();
        for code in [
            "monotonic",
            "estimated_wall_clock",
            "missing_observation",
            "invalid_duration",
            "counter_reset",
            "gauge_difference",
            "gauge_unverified",
            "environment_difference",
            "environment_counters",
            "counter_unverified",
            "identity_mismatch",
            "capture_unstable",
            "identity_verified",
            "legacy_match",
            "legacy_unknown",
            "identity_missing",
            "legacy_accepted",
            "legacy_not_accepted",
            "cgroup_mismatch",
            "cgroup_unverified",
            "unavailable",
        ] {
            assert!(dict.contains_key(&format!("reason.{code}")), "{code}");
        }
        for raw in [
            "end_of_directory",
            "time_budget",
            "time_budget_parent",
            "entry_budget",
            "open:PermissionDenied",
            "open:NotFound",
            "enumeration_error",
            "worker_incomplete",
            "store_path_unresolved",
            "in_progress",
            "scanner unavailable",
            "private path must never be echoed",
        ] {
            assert!(
                dict.contains_key(&format!("stop.{}", scan_stop_reason(raw))),
                "{raw}"
            );
        }
        for scope in [
            "supervisor",
            "collector_worker",
            "storage_scanner",
            "unknown",
        ] {
            assert!(dict.contains_key(&format!("scope.{scope}")));
        }
    }
    #[test]
    fn versions_cannot_smuggle_ip_addresses_or_freeform_tokens() {
        assert_eq!(safe_version("v10.4.0-mocha"), Some("v10.4.0-mocha"));
        assert_eq!(safe_version("0.1.0"), Some("0.1.0"));
        for version in [
            "192.168.1.2",
            "v1.2.3-token-secret",
            "1.2.3+private",
            "secret.12.3",
            "1.2.3.4",
        ] {
            assert!(safe_version(version).is_none());
        }
    }
}
