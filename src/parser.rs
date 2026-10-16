//! Bounded JSON input and legacy_v0 snapshot conversion.
use crate::model::*;
use serde_json::{Map, Value};
use std::io::Read;

pub fn read_snapshot(reader: impl Read) -> Result<SnapshotV1, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_SNAPSHOT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("snapshot read: {e}"))?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err("snapshot exceeds 8 MiB".into());
    }
    parse_snapshot(&bytes)
}

pub fn parse_snapshot(bytes: &[u8]) -> Result<SnapshotV1, String> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err("snapshot exceeds 8 MiB".into());
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let raw: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid snapshot JSON: {e}"))?;
    let obj = raw.as_object().ok_or("snapshot must be a JSON object")?;
    if obj.contains_key("error") && !obj.contains_key("services") {
        return Err("input is a collector error, not a snapshot (probe may have expired)".into());
    }
    if let Some(schema) = obj.get("schema_version") {
        if schema.as_u64() != Some(1) {
            return Err("unsupported snapshot schema_version".into());
        }
        let snapshot: SnapshotV1 =
            serde_json::from_value(raw).map_err(|e| format!("invalid v1 snapshot: {e}"))?;
        validate_v1(&snapshot)?;
        return Ok(snapshot);
    }
    if !obj.contains_key("timestamp_epoch") || !obj.contains_key("services") {
        return Err("unrecognized snapshot: expected v1 or legacy_v0 probe output".into());
    }
    adapt_legacy(obj)
}

fn validate_v1(s: &SnapshotV1) -> Result<(), String> {
    for time in [
        s.capture.started_epoch_seconds,
        s.capture.finished_epoch_seconds,
    ]
    .into_iter()
    .flatten()
    {
        if !time.is_finite() || time < 0.0 {
            return Err("invalid capture timestamp".into());
        }
    }
    if let (Some(a), Some(b)) = (
        s.capture.started_epoch_seconds,
        s.capture.finished_epoch_seconds,
    ) {
        if b < a {
            return Err("capture finished before it started".into());
        }
    }
    for (key, cfg) in &s.fibre_config {
        if !valid_config(key, &cfg.value) {
            return Err(format!("invalid typed configuration: {key}"));
        }
    }
    for m in &s.metrics.samples {
        if !m.value.is_finite() {
            return Err("non-finite metric value".into());
        }
    }
    Ok(())
}

fn issue(issues: &mut Vec<Issue>, path: &str, code: &str) {
    issues.push(Issue {
        section: path.into(),
        code: code.into(),
        detail: "Source field unavailable or malformed; no healthy value inferred.".into(),
    });
}

fn unsigned(v: Option<&Value>, path: &str, issues: &mut Vec<Issue>) -> Option<u64> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    let n = v.as_u64().or_else(|| {
        v.as_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|s| s.parse().ok())
    });
    if n.is_none() {
        issue(issues, path, "invalid_unsigned_integer");
    }
    n
}

fn kib(v: Option<&Value>, path: &str, issues: &mut Vec<Issue>) -> Option<u64> {
    let n = unsigned(v, path, issues)?;
    match n.checked_mul(1024) {
        Some(x) => Some(x),
        None => {
            issue(issues, path, "byte_conversion_overflow");
            None
        }
    }
}

fn limit(v: Option<&Value>, path: &str, issues: &mut Vec<Issue>) -> Option<MemoryLimit> {
    if matches!(
        v.and_then(Value::as_str),
        Some("infinity" | "max" | "unlimited")
    ) {
        return Some(MemoryLimit::Unlimited);
    }
    unsigned(v, path, issues).map(MemoryLimit::Finite)
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|s| s.chars().take(512).collect())
}
fn section<'a>(
    v: Option<&'a Value>,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<&'a Map<String, Value>> {
    let Some(v) = v else {
        issue(issues, path, "missing");
        return None;
    };
    let Some(o) = v.as_object() else {
        issue(issues, path, "invalid_object");
        return None;
    };
    if o.contains_key("error") {
        issue(issues, path, "collection_error");
        return None;
    }
    if o.contains_key("skipped") {
        issue(issues, path, "skipped");
        return None;
    }
    Some(o)
}

fn adapt_legacy(o: &Map<String, Value>) -> Result<SnapshotV1, String> {
    let mut s = SnapshotV1 {
        collector_version: "legacy-probe-unversioned".into(),
        ..SnapshotV1::default()
    };
    let timestamp = o
        .get("timestamp_epoch")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or("legacy timestamp_epoch must be a nonnegative number")?;
    s.capture = Capture {
        started_epoch_seconds: Some(timestamp),
        input_format: "legacy_v0".into(),
        ..Capture::default()
    };
    issue(
        &mut s.issues,
        "capture.identity",
        "legacy_host_boot_identity_missing",
    );
    if let Some(units) = section(o.get("services"), "services", &mut s.issues) {
        for (name, value) in units {
            let role = match name.as_str() {
                "celestia-appd.service" => "app",
                "fibre.service" => "fibre",
                _ => "unknown",
            };
            let mut service = ServiceSnapshot {
                role: role.into(),
                status: CollectionStatus::Complete,
                ..ServiceSnapshot::default()
            };
            if let Some(p) = section(Some(value), &format!("services.{name}"), &mut s.issues) {
                let u = |key: &str, issues: &mut Vec<Issue>| {
                    unsigned(p.get(key), &format!("services.{name}.{key}"), issues)
                };
                service.active_state = text(p.get("ActiveState"));
                service.sub_state = text(p.get("SubState"));
                service.identity.pid = u("MainPID", &mut s.issues)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|p| *p > 0);
                service.identity.restart_count = u("NRestarts", &mut s.issues);
                service.identity.service_start_monotonic_us =
                    u("ExecMainStartTimestampMonotonic", &mut s.issues);
                service.identity.cgroup_path = text(p.get("ControlGroup"));
                service.memory_current_bytes = u("MemoryCurrent", &mut s.issues);
                service.memory_peak_bytes = u("MemoryPeak", &mut s.issues);
                service.memory_max = limit(p.get("MemoryMax"), "MemoryMax", &mut s.issues);
                service.memory_high = limit(p.get("MemoryHigh"), "MemoryHigh", &mut s.issues);
                service.cpu_usage_ns = u("CPUUsageNSec", &mut s.issues);
                if let Some(cg) = section(p.get("cgroup"), "cgroup", &mut s.issues) {
                    if let Some(stat) = section(cg.get("memory.stat"), "memory.stat", &mut s.issues)
                    {
                        service.memory_anon_bytes =
                            unsigned(stat.get("anon"), "memory.stat.anon", &mut s.issues);
                        service.memory_file_bytes =
                            unsigned(stat.get("file"), "memory.stat.file", &mut s.issues);
                        service.memory_kernel_bytes =
                            unsigned(stat.get("kernel"), "memory.stat.kernel", &mut s.issues);
                    }
                    if let Some(events) =
                        section(cg.get("memory.events"), "memory.events", &mut s.issues)
                    {
                        for key in ["low", "high", "max", "oom", "oom_kill", "oom_group_kill"] {
                            if let Some(n) = unsigned(events.get(key), key, &mut s.issues) {
                                service.memory_events.insert(key.into(), n);
                            }
                        }
                    }
                }
                if let Some(pid) = service.identity.pid {
                    service.rss_bytes = o
                        .get("top_rss")
                        .and_then(Value::as_array)
                        .and_then(|rows| {
                            rows.iter().find(|row| {
                                row.get("pid").and_then(Value::as_u64) == Some(pid as u64)
                            })
                        })
                        .and_then(|row| kib(row.get("rss_kib"), "top_rss.rss_kib", &mut s.issues));
                }
            } else {
                service.status = CollectionStatus::Error;
            }
            s.services.insert(name.clone(), service);
        }
    }
    if let Some(mem) = section(o.get("meminfo_kib"), "meminfo_kib", &mut s.issues) {
        s.host.memory_total_bytes = kib(mem.get("MemTotal"), "MemTotal", &mut s.issues);
        s.host.memory_available_bytes = kib(mem.get("MemAvailable"), "MemAvailable", &mut s.issues);
        s.host.swap_total_bytes = kib(mem.get("SwapTotal"), "SwapTotal", &mut s.issues);
        s.host.swap_free_bytes = kib(mem.get("SwapFree"), "SwapFree", &mut s.issues);
    }
    if let Some(fs) = section(o.get("root_fs_bytes"), "root_fs_bytes", &mut s.issues) {
        s.host.root_filesystem = Some(FilesystemSnapshot {
            total_bytes: unsigned(fs.get("total"), "root_fs.total", &mut s.issues),
            available_bytes: unsigned(fs.get("available"), "root_fs.available", &mut s.issues),
            ..FilesystemSnapshot::default()
        });
    }
    if let Some(cfg) = section(
        o.get("fibre_config_on_disk"),
        "fibre_config_on_disk",
        &mut s.issues,
    ) {
        for key in [
            "root.max_connections",
            "root.max_concurrent_streams",
            "root.unlimited_budget",
            "root.min_upload_size",
            "root.upload_verify_workers",
        ] {
            if let Some(entry) = cfg.get(key).and_then(Value::as_object) {
                if let Some(value) = entry.get("value") {
                    if valid_config(key, value) {
                        let source = match entry.get("source").and_then(Value::as_str) {
                            Some("file") => ConfigSource::ConfigFile,
                            Some("default") => ConfigSource::DefaultAssumed,
                            _ => ConfigSource::Unknown,
                        };
                        s.fibre_config.insert(key.into(),ConfigValue {value:value.clone(),source,evidence:format!("legacy_v0 fibre_config_on_disk.{key}; disk values are not proven loaded configuration")});
                    } else {
                        issue(&mut s.issues, key, "invalid_config_value");
                    }
                }
            }
        }
    }
    if let Some(app) = section(o.get("app_status"), "app_status", &mut s.issues) {
        s.app_status.status = CollectionStatus::Partial;
        s.app_status.chain_id = app.get("node_info").and_then(|n| text(n.get("network")));
        if let Some(sync) = app.get("sync_info").and_then(Value::as_object) {
            s.app_status.block_height = unsigned(
                sync.get("latest_block_height"),
                "app_status.height",
                &mut s.issues,
            );
            s.app_status.block_time = text(sync.get("latest_block_time"));
            s.app_status.catching_up = sync.get("catching_up").and_then(Value::as_bool);
        }
        if s.app_status.block_height.is_some() && s.app_status.catching_up.is_some() {
            s.app_status.status = CollectionStatus::Complete;
        }
    } else {
        s.app_status.status = if o
            .get("app_status")
            .is_some_and(|v| v.get("error").is_some())
        {
            CollectionStatus::Error
        } else {
            CollectionStatus::Missing
        };
    }
    Ok(s)
}

fn valid_config(key: &str, v: &Value) -> bool {
    match key {
        "root.unlimited_budget" => v.is_boolean(),
        "root.max_connections"
        | "root.max_concurrent_streams"
        | "root.min_upload_size"
        | "root.upload_verify_workers" => v.as_u64().is_some_and(|n| n > 0),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn base() -> Value {
        json!({"timestamp_epoch":100.5,"services":{"fibre.service":{"MainPID":"42","NRestarts":"0","MemoryCurrent":"9007199254740993","MemoryMax":"infinity","cgroup":{"memory.stat":{"anon":0},"memory.events":{"oom":0}}}},"meminfo_kib":{"MemTotal":123},"fibre_config_on_disk":{"root.max_connections":{"value":16,"source":"file"}},"app_status":{"error":"LookupError"}})
    }
    #[test]
    fn preserves_large_integers_and_zero_without_fabricating_missing() {
        let s = parse_snapshot(&serde_json::to_vec(&base()).unwrap()).unwrap();
        let f = &s.services["fibre.service"];
        assert_eq!(f.memory_current_bytes, Some(9007199254740993));
        assert_eq!(f.memory_anon_bytes, Some(0));
        assert_eq!(f.memory_file_bytes, None);
        assert_eq!(f.memory_max, Some(MemoryLimit::Unlimited));
        assert_eq!(s.host.memory_total_bytes, Some(125952));
        assert_eq!(s.app_status.status, CollectionStatus::Error);
        assert!(f.version.is_none());
        assert!(s.capture.boot_hash.is_none());
        assert_eq!(
            s.fibre_config["root.max_connections"].source,
            ConfigSource::ConfigFile
        );
    }
    #[test]
    fn rejects_bool_negative_and_overflow_metrics_as_values() {
        for value in [json!(true), json!(-1), json!("18446744073709551616")] {
            let mut v = base();
            v["services"]["fibre.service"]["MemoryCurrent"] = value;
            let s = parse_snapshot(&serde_json::to_vec(&v).unwrap()).unwrap();
            assert!(s.services["fibre.service"].memory_current_bytes.is_none());
            assert!(s
                .issues
                .iter()
                .any(|i| i.code == "invalid_unsigned_integer"));
        }
    }
    #[test]
    fn errors_are_not_empty_successful_snapshots() {
        for v in [
            br#"{}"#.as_slice(),
            br#"{"error":"Expired"}"#,
            br#"{"schema_version":2}"#,
            br#"{"timestamp_epoch":NaN,"services":{}}"#,
        ] {
            assert!(parse_snapshot(v).is_err());
        }
    }
    #[test]
    fn bounded_and_bom_inputs() {
        assert!(read_snapshot(vec![b' '; MAX_SNAPSHOT_BYTES + 1].as_slice()).is_err());
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend(serde_json::to_vec(&base()).unwrap());
        assert!(parse_snapshot(&bytes).is_ok());
    }
    #[test]
    fn v1_roundtrip_and_validation() {
        let mut s = SnapshotV1::default();
        s.capture.started_epoch_seconds = Some(20.0);
        s.capture.finished_epoch_seconds = Some(21.0);
        assert!(parse_snapshot(&serde_json::to_vec(&s).unwrap()).is_ok());
        s.capture.finished_epoch_seconds = Some(19.0);
        assert!(parse_snapshot(&serde_json::to_vec(&s).unwrap()).is_err());
    }
    #[test]
    fn synthetic_legacy_pair_preserves_identity_memory_and_collection_errors() {
        let a = parse_snapshot(include_bytes!("../tests/fixtures/legacy/snapshot-a.json")).unwrap();
        let b = parse_snapshot(include_bytes!("../tests/fixtures/legacy/snapshot-b.json")).unwrap();
        assert!(
            ((b.capture.started_epoch_seconds.unwrap() - a.capture.started_epoch_seconds.unwrap())
                - 83.4120102)
                .abs()
                < 0.000001
        );
        assert_eq!(
            a.services["celestia-appd.service"]
                .identity
                .service_start_monotonic_us,
            Some(1000000)
        );
        assert_eq!(
            a.services["fibre.service"].identity.pid,
            b.services["fibre.service"].identity.pid
        );
        assert_eq!(
            a.services["celestia-appd.service"].memory_current_bytes,
            Some(4294967296)
        );
        assert!(a.capture.host_hash.is_none());
        assert!(a.capture.boot_hash.is_none());
        assert!(a.services["fibre.service"].version.is_none());
        assert_eq!(
            a.fibre_config["root.max_connections"].source,
            ConfigSource::ConfigFile
        );
        assert_eq!(b.app_status.status, CollectionStatus::Error);
    }
}
