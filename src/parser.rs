//! Bounded JSON input and legacy_v0 snapshot conversion.
use crate::model::*;
use serde_json::{Map, Value};
use std::io::Read;

// The byte cap alone does not bound expansion into findings and rendered views.
// These budgets allow headroom over live collection (two services, two scans,
// 128 aged-file details per scan and 128 issues) while bounding offline inputs.
const MAX_JSON_NODES: usize = 65_536;
const MAX_STRING_BYTES: usize = 65_536;
const MAX_SERVICES: usize = 16;
const MAX_SCANS: usize = 16;
const MAX_METRIC_SAMPLES: usize = 4_096;
const MAX_AGED_FILE_DETAILS: usize = 512;
const MAX_ISSUES: usize = 256;
const MAX_CONFIG_ENTRIES: usize = 64;
const MAX_WORKER_USAGE: usize = 32;
const MAX_ANCESTOR_LIMITS: usize = 64;
const MAX_MEMORY_EVENTS: usize = 32;
const STRUCTURE_LIMIT_ERROR: &str = "snapshot structure exceeds supported limits";

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
    let raw: Value = serde_json::from_slice(bytes).map_err(|e| {
        // Deserializer messages can quote values from an untrusted snapshot.
        format!(
            "invalid snapshot JSON at line {}, column {}",
            e.line(),
            e.column()
        )
    })?;
    validate_structure(&raw)?;
    let obj = raw.as_object().ok_or("snapshot must be a JSON object")?;
    if obj.contains_key("error") && !obj.contains_key("services") {
        return Err("input is a collector error, not a snapshot (probe may have expired)".into());
    }
    if let Some(schema) = obj.get("schema_version") {
        if schema.as_u64() != Some(1) {
            return Err("unsupported snapshot schema_version".into());
        }
        let snapshot: SnapshotV1 =
            serde_json::from_value(raw).map_err(|_| "invalid v1 snapshot fields or types")?;
        validate_v1(&snapshot)?;
        return Ok(snapshot);
    }
    if !obj.contains_key("timestamp_epoch") || !obj.contains_key("services") {
        return Err("unrecognized snapshot: expected v1 or legacy_v0 probe output".into());
    }
    adapt_legacy(obj)
}

fn collection_limit(value: Option<&Value>, limit: usize) -> Result<(), String> {
    let length = match value {
        Some(Value::Array(values)) => values.len(),
        Some(Value::Object(values)) => values.len(),
        _ => 0,
    };
    if length > limit {
        return Err(STRUCTURE_LIMIT_ERROR.into());
    }
    Ok(())
}

fn validate_structure(raw: &Value) -> Result<(), String> {
    // serde_json already limits nesting. Iterate here so budget checking does
    // not add recursion, and check pending work before growing the stack.
    let mut pending = vec![raw];
    let mut visited = 0usize;
    while let Some(value) = pending.pop() {
        visited += 1;
        let child_count = match value {
            Value::Array(values) => values.len(),
            Value::Object(values) => values.len(),
            _ => 0,
        };
        if visited + pending.len() + child_count > MAX_JSON_NODES {
            return Err(STRUCTURE_LIMIT_ERROR.into());
        }
        match value {
            Value::String(text) if text.len() > MAX_STRING_BYTES => {
                return Err(STRUCTURE_LIMIT_ERROR.into());
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => {
                if values.keys().any(|key| key.len() > MAX_STRING_BYTES) {
                    return Err(STRUCTURE_LIMIT_ERROR.into());
                }
                pending.extend(values.values());
            }
            _ => {}
        }
    }
    collection_limit(raw.get("services"), MAX_SERVICES)?;
    collection_limit(raw.get("scans"), MAX_SCANS)?;
    collection_limit(raw.pointer("/metrics/samples"), MAX_METRIC_SAMPLES)?;
    collection_limit(raw.get("issues"), MAX_ISSUES)?;
    collection_limit(raw.get("fibre_config"), MAX_CONFIG_ENTRIES)?;
    collection_limit(raw.get("worker_usage"), MAX_WORKER_USAGE)?;
    if let Some(scans) = raw.get("scans").and_then(Value::as_array) {
        for scan in scans {
            collection_limit(scan.get("aged_files"), MAX_AGED_FILE_DETAILS)?;
        }
    }
    if let Some(services) = raw.get("services").and_then(Value::as_object) {
        for service in services.values() {
            collection_limit(service.get("ancestor_limits"), MAX_ANCESTOR_LIMITS)?;
            collection_limit(service.get("memory_events"), MAX_MEMORY_EVENTS)?;
        }
    }
    if raw.get("schema_version").is_none() {
        // The legacy adapter reads only known fields, but still scans top_rss
        // and receives maps from an untrusted JSON source. Reject excess
        // structure instead of silently treating truncated evidence as whole.
        collection_limit(raw.get("top_rss"), 4_096)?;
        collection_limit(raw.get("fibre_config_on_disk"), MAX_CONFIG_ENTRIES)?;
        collection_limit(raw.get("meminfo_kib"), 256)?;
        collection_limit(raw.get("root_fs_bytes"), 32)?;
        if let Some(services) = raw.get("services").and_then(Value::as_object) {
            for service in services.values() {
                collection_limit(Some(service), 128)?;
                collection_limit(service.get("cgroup"), 32)?;
                collection_limit(service.pointer("/cgroup/memory.stat"), 256)?;
                collection_limit(service.pointer("/cgroup/memory.events"), MAX_MEMORY_EVENTS)?;
            }
        }
    }
    Ok(())
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
            return Err("invalid typed configuration".into());
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
    #[test]
    fn service_count_is_bounded_before_v1_and_legacy_expansion() {
        for legacy in [false, true] {
            let mut input = if legacy {
                json!({"timestamp_epoch":100.0,"services":{}})
            } else {
                serde_json::to_value(SnapshotV1::default()).unwrap()
            };
            for count in [MAX_SERVICES, 500] {
                input["services"] = Value::Object(
                    (0..count)
                        .map(|index| (format!("service-{index}"), json!({})))
                        .collect(),
                );
                let result = parse_snapshot(&serde_json::to_vec(&input).unwrap());
                if count == MAX_SERVICES {
                    assert_eq!(result.unwrap().services.len(), MAX_SERVICES);
                } else {
                    assert_eq!(result.unwrap_err(), STRUCTURE_LIMIT_ERROR);
                }
            }
        }
    }

    #[test]
    fn typed_collection_budgets_accept_boundary_and_reject_excess() {
        for (pointer, limit, is_map) in [
            ("/scans", MAX_SCANS, false),
            ("/metrics/samples", MAX_METRIC_SAMPLES, false),
            ("/issues", MAX_ISSUES, false),
            ("/fibre_config", MAX_CONFIG_ENTRIES, true),
            ("/worker_usage", MAX_WORKER_USAGE, false),
            ("/scans/0/aged_files", MAX_AGED_FILE_DETAILS, false),
            ("/services/test/ancestor_limits", MAX_ANCESTOR_LIMITS, true),
            ("/services/test/memory_events", MAX_MEMORY_EVENTS, true),
        ] {
            for count in [limit, limit + 1] {
                let mut input = json!({
                    "schema_version":1,"scans":[{"aged_files":[]}],
                    "metrics":{"samples":[]},"issues":[],"fibre_config":{},
                    "worker_usage":[],"services":{"test":{"ancestor_limits":{},"memory_events":{}}}
                });
                *input.pointer_mut(pointer).unwrap() = if is_map {
                    Value::Object((0..count).map(|i| (i.to_string(), Value::Null)).collect())
                } else {
                    Value::Array(vec![Value::Null; count])
                };
                assert_eq!(
                    validate_structure(&input).is_ok(),
                    count == limit,
                    "{pointer}"
                );
                if count > limit {
                    assert_eq!(
                        parse_snapshot(&serde_json::to_vec(&input).unwrap()).unwrap_err(),
                        STRUCTURE_LIMIT_ERROR
                    );
                }
            }
        }
    }

    #[test]
    fn legacy_maps_and_rss_rows_have_structural_budgets() {
        for (pointer, limit, is_map) in [
            ("/top_rss", 4_096, false),
            ("/fibre_config_on_disk", MAX_CONFIG_ENTRIES, true),
            ("/meminfo_kib", 256, true),
            ("/root_fs_bytes", 32, true),
            ("/services/test/cgroup/memory.stat", 256, true),
            (
                "/services/test/cgroup/memory.events",
                MAX_MEMORY_EVENTS,
                true,
            ),
        ] {
            let mut input = json!({
                "timestamp_epoch":100.0,"top_rss":[],"fibre_config_on_disk":{},
                "meminfo_kib":{},"root_fs_bytes":{},
                "services":{"test":{"cgroup":{"memory.stat":{},"memory.events":{}}}}
            });
            *input.pointer_mut(pointer).unwrap() = if is_map {
                Value::Object((0..=limit).map(|i| (i.to_string(), Value::Null)).collect())
            } else {
                Value::Array(vec![Value::Null; limit + 1])
            };
            assert_eq!(
                parse_snapshot(&serde_json::to_vec(&input).unwrap()).unwrap_err(),
                STRUCTURE_LIMIT_ERROR,
                "{pointer}"
            );
        }
    }

    #[test]
    fn unknown_fields_still_obey_tree_and_string_budgets() {
        let mut input = serde_json::to_value(SnapshotV1::default()).unwrap();
        input["unknown_field"] = Value::String("x".repeat(MAX_STRING_BYTES));
        assert!(parse_snapshot(&serde_json::to_vec(&input).unwrap()).is_ok());
        input["unknown_field"] = Value::String("x".repeat(MAX_STRING_BYTES + 1));
        assert_eq!(
            parse_snapshot(&serde_json::to_vec(&input).unwrap()).unwrap_err(),
            STRUCTURE_LIMIT_ERROR
        );
        input["unknown_field"] = Value::Array(vec![Value::Null; MAX_JSON_NODES]);
        assert_eq!(
            parse_snapshot(&serde_json::to_vec(&input).unwrap()).unwrap_err(),
            STRUCTURE_LIMIT_ERROR
        );
        let key = "x".repeat(MAX_STRING_BYTES + 1);
        let input = Value::Object([(key, Value::Null)].into_iter().collect());
        assert_eq!(
            validate_structure(&input).unwrap_err(),
            STRUCTURE_LIMIT_ERROR
        );
    }

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
