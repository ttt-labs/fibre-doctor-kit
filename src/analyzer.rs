//! Offline analysis. Observed values, missing evidence and assumptions stay distinct.
use crate::model::*;
use std::collections::BTreeSet;

pub const CAPACITY_PROFILE: &str = "v10.4.0-mocha";
pub const RECEIVE_BUFFER_BYTES: u64 = 132 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub now_epoch_seconds: Option<f64>,
    pub max_metrics_age_seconds: f64,
    pub min_signing_samples: u64,
    pub signing_threshold_seconds: f64,
    pub profile: Option<String>,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            // Offline findings describe the capture; reopening a report must not
            // silently change a historical verdict as wall-clock time advances.
            now_epoch_seconds: None,
            max_metrics_age_seconds: 120.0,
            min_signing_samples: 50,
            signing_threshold_seconds: 0.010,
            profile: None,
        }
    }
}

fn finding(
    id: &str,
    status: CheckStatus,
    title: &str,
    evidence: Vec<String>,
    impact: &str,
    recommendation: &str,
    verification: &str,
) -> Finding {
    Finding {
        id: id.into(),
        status,
        title: title.into(),
        evidence,
        impact: impact.into(),
        recommendation: recommendation.into(),
        verification: verification.into(),
    }
}

fn role<'a>(name: &str, service: &'a ServiceSnapshot) -> &'a str {
    if service.role == "app" || service.role == "fibre" {
        &service.role
    } else if name == "celestia-appd.service" {
        "app"
    } else if name == "fibre.service" {
        "fibre"
    } else {
        "unknown"
    }
}

fn has_observed_version(snapshot: &SnapshotV1, expected: &str) -> bool {
    snapshot.services.iter().any(|(name, service)| {
        role(name, service) == "fibre"
            && service
                .version
                .as_ref()
                .is_some_and(|v| v.source == ConfigSource::ObservedRuntime && v.version == expected)
    })
}

fn config_positive(snapshot: &SnapshotV1, key: &str) -> Option<u64> {
    snapshot
        .fibre_config
        .get(key)?
        .value
        .as_u64()
        .filter(|v| *v > 0)
}

/// Version-scoped receive-buffer arithmetic, never a total RAM or readiness claim.
pub fn capacity(
    snapshot: &SnapshotV1,
    profile: Option<&str>,
    target: Option<u64>,
) -> CapacityModel {
    let mut out = CapacityModel {
        status: CheckStatus::Inconclusive, profile: None, profile_source: ConfigSource::Unknown,
        connections: target.or_else(|| config_positive(snapshot, "root.max_connections")),
        streams: config_positive(snapshot, "root.max_concurrent_streams"),
        receive_buffer_bytes: None, scenarios: vec![], evidence: vec![],
        limitations: vec![
            "Receive buffers are a concurrency scenario, not measured memory, total process memory or a RAM limit.".into(),
            "On-disk configuration does not prove effective runtime configuration or CLI overrides.".into(),
            "Do not add RSS, cgroup memory and file cache together; these observations overlap.".into(),
            "Other services, ancestor limits, runtime overhead and changing reclaimable cache require separate evidence.".into(),
        ],
    };
    push_opt(
        &mut out.evidence,
        "host_total_bytes",
        snapshot.host.memory_total_bytes,
    );
    push_opt(
        &mut out.evidence,
        "host_available_bytes",
        snapshot.host.memory_available_bytes,
    );
    if let Some((_, fibre)) = snapshot
        .services
        .iter()
        .find(|(n, s)| role(n, s) == "fibre")
    {
        push_opt(&mut out.evidence, "rss_bytes", fibre.rss_bytes);
        push_opt(
            &mut out.evidence,
            "memory_current_bytes",
            fibre.memory_current_bytes,
        );
        push_opt(
            &mut out.evidence,
            "memory_anon_bytes",
            fibre.memory_anon_bytes,
        );
        push_opt(
            &mut out.evidence,
            "memory_file_bytes",
            fibre.memory_file_bytes,
        );
        push_opt(&mut out.evidence, "fd_count", fibre.fd_count);
        if let Some(MemoryLimit::Finite(limit)) = fibre.fd_soft_limit {
            push_opt(&mut out.evidence, "fd_soft_limit", Some(limit));
        }
        out.evidence.push(format!(
            "fd_evidence_complete={}",
            fibre.fd_inspection_complete
        ));
        out.evidence.push(format!(
            "ancestor_limits_complete={}",
            fibre.ancestor_limits_complete
        ));
    }
    match profile {
        Some(value) if value != CAPACITY_PROFILE => {
            out.status = CheckStatus::Unsupported;
            out.evidence.push("profile_supported=false".into());
            return out;
        }
        Some(_) => {
            out.profile = Some(CAPACITY_PROFILE.into());
            out.profile_source = ConfigSource::DefaultAssumed;
            out.evidence.push("profile_explicit_assumption=true".into());
        }
        None if has_observed_version(snapshot, CAPACITY_PROFILE) => {
            out.profile = Some(CAPACITY_PROFILE.into());
            out.profile_source = ConfigSource::ObservedRuntime;
            out.evidence.push("profile_observed=true".into());
        }
        None => {
            out.evidence.push("supported_version_observed=false".into());
            return out;
        }
    }
    if let (Some(connections), Some(streams)) = (out.connections, out.streams) {
        if connections == 0 {
            out.evidence.push("connections_valid=false".into());
            return out;
        }
        out.receive_buffer_bytes = connections
            .checked_mul(streams)
            .and_then(|n| n.checked_mul(RECEIVE_BUFFER_BYTES));
        if out.receive_buffer_bytes.is_none() {
            out.evidence.push("arithmetic_overflow=true".into());
            return out;
        }
        out.evidence.extend([
            format!("connections={connections}"),
            format!("streams={streams}"),
            format!("buffer_per_stream_bytes={RECEIVE_BUFFER_BYTES}"),
        ]);
        for connections in [16u64, 24, 32] {
            if let Some(bytes) = connections
                .checked_mul(streams)
                .and_then(|n| n.checked_mul(RECEIVE_BUFFER_BYTES))
            {
                out.scenarios.push(CapacityScenario {
                    connections,
                    streams,
                    receive_buffer_bytes: bytes,
                });
            }
        }
        let runtime = ["root.max_connections", "root.max_concurrent_streams"]
            .iter()
            .all(|key| {
                snapshot
                    .fibre_config
                    .get(*key)
                    .is_some_and(|v| v.source == ConfigSource::ObservedRuntime)
            });
        out.status =
            if runtime && out.profile_source == ConfigSource::ObservedRuntime && target.is_none() {
                CheckStatus::Pass
            } else {
                CheckStatus::Warn
            };
    } else {
        out.evidence.push("configuration_complete=false".into());
    }
    out
}

fn push_opt(evidence: &mut Vec<String>, name: &str, value: Option<u64>) {
    if let Some(value) = value {
        evidence.push(format!("{name}={value}"));
    }
}

/// Findings concern the observation scope. Incomplete checks never become PASS.
pub fn doctor(snapshot: &SnapshotV1, options: &DoctorOptions) -> Vec<Finding> {
    let mut out = Vec::new();
    if !snapshot.issues.is_empty() {
        out.push(finding(
            "collection_issues",
            CheckStatus::Inconclusive,
            "Collection has evidence gaps",
            vec![format!("issue_count={}", snapshot.issues.len())],
            "Some checks cannot be established from this capture.",
            "Review collection status and recollect only missing evidence.",
            "Verify each required section has a complete, correctly scoped observation.",
        ));
    }
    for expected in ["app", "fibre"] {
        if !snapshot
            .services
            .iter()
            .any(|(n, s)| role(n, s) == expected)
        {
            out.push(finding(
                "service_state",
                CheckStatus::Inconclusive,
                "Service state unavailable",
                vec![format!("service={expected}")],
                "Missing evidence does not mean the service is stopped.",
                "Collect the intended service's current state.",
                "Verify the unit and its current main process.",
            ));
        }
    }
    let mut host_evidence = Vec::new();
    push_opt(
        &mut host_evidence,
        "total_bytes",
        snapshot.host.memory_total_bytes,
    );
    push_opt(
        &mut host_evidence,
        "available_bytes",
        snapshot.host.memory_available_bytes,
    );
    let host_status = match (
        snapshot.host.memory_total_bytes,
        snapshot.host.memory_available_bytes,
    ) {
        (Some(total), Some(avail)) if total > 0 && avail <= total => {
            if avail == 0 {
                CheckStatus::Fail
            } else if (avail as f64) / (total as f64) < 0.10 {
                CheckStatus::Warn
            } else {
                CheckStatus::Pass
            }
        }
        _ => CheckStatus::Inconclusive,
    };
    out.push(finding(
        "host_memory",
        host_status,
        "Host available memory at capture",
        host_evidence,
        "Available memory includes reclaimable memory and is not a workload capacity guarantee.",
        "Observe representative workload and other services before changing limits.",
        "Confirm host memory observations and pressure across a representative interval.",
    ));

    for (name, service) in &snapshot.services {
        let service_role = role(name, service);
        let base = || vec![format!("service={service_role}")];
        let mut evidence = base();
        if let Some(state) = &service.active_state {
            if matches!(
                state.as_str(),
                "active" | "inactive" | "failed" | "activating" | "deactivating" | "reloading"
            ) {
                evidence.push(format!("status={state}"));
            }
        }
        let status = match (
            service.active_state.as_deref(),
            service.sub_state.as_deref(),
        ) {
            (Some("failed"), _) | (Some("inactive"), _) => CheckStatus::Fail,
            (Some("active"), Some("running")) if service.status == CollectionStatus::Complete => {
                CheckStatus::Pass
            }
            (Some("activating" | "deactivating" | "reloading"), _) => CheckStatus::Warn,
            _ => CheckStatus::Inconclusive,
        };
        out.push(finding(
            "service_state",
            status,
            "Observed service state",
            evidence,
            "Running state alone does not establish synchronization or workload readiness.",
            "Inspect the intended unit and application observations.",
            "Verify state and process identity during capture.",
        ));
        out.push(finding(
            "process_stability",
            match service.identity.stable_during_capture {
                Some(true) => CheckStatus::Pass,
                Some(false) => CheckStatus::Warn,
                None => CheckStatus::Inconclusive,
            },
            "Process identity during collection",
            base(),
            "A changing process can mix observations from different lifetimes.",
            "Repeat collection when identity remains stable.",
            "Compare PID and process start ticks at capture boundaries.",
        ));

        let mut evidence = base();
        push_opt(&mut evidence, "rss_bytes", service.rss_bytes);
        push_opt(
            &mut evidence,
            "memory_current_bytes",
            service.memory_current_bytes,
        );
        push_opt(
            &mut evidence,
            "memory_anon_bytes",
            service.memory_anon_bytes,
        );
        push_opt(
            &mut evidence,
            "memory_file_bytes",
            service.memory_file_bytes,
        );
        out.push(finding(
            "service_memory",
            if service.rss_bytes.is_some()
                && service.memory_current_bytes.is_some()
                && service.memory_file_bytes.is_some()
            {
                CheckStatus::Pass
            } else {
                CheckStatus::Inconclusive
            },
            "Distinct process and cgroup memory observations",
            evidence,
            "RSS, cgroup totals and file-backed cache overlap; their sum is not memory demand.",
            "Keep measurements separate and observe changes under representative load.",
            "Verify process scope, cgroup scope and units before interpreting a delta.",
        ));

        let mut evidence = base();
        let mut limits = service
            .ancestor_limits
            .iter()
            .filter(|(name, _)| name.ends_with(":memory.max"))
            .map(|(_, limit)| limit)
            .collect::<Vec<_>>();
        if let Some(limit) = &service.memory_max {
            limits.push(limit);
        }
        let finite = limits
            .iter()
            .filter_map(|v| match v {
                MemoryLimit::Finite(n) => Some(*n),
                _ => None,
            })
            .min();
        push_opt(&mut evidence, "effective_limit_bytes", finite);
        let pressure = service
            .ancestor_limits
            .iter()
            .filter(|(name, _)| name.ends_with(":memory.high"))
            .map(|(_, limit)| limit)
            .chain(service.memory_high.iter())
            .filter_map(|v| match v {
                MemoryLimit::Finite(n) => Some(*n),
                MemoryLimit::Unlimited => None,
            })
            .min();
        push_opt(&mut evidence, "pressure_threshold_bytes", pressure);
        evidence.push(format!("ancestor_count={}", service.ancestor_limits.len()));
        evidence.push(format!(
            "ancestor_limits_complete={}",
            service.ancestor_limits_complete
        ));
        let status = if finite
            .zip(service.memory_current_bytes)
            .is_some_and(|(lim, usage)| usage >= lim)
            || pressure
                .zip(service.memory_current_bytes)
                .is_some_and(|(lim, usage)| usage >= lim)
        {
            CheckStatus::Warn
        } else if service.memory_max.is_none() || !service.ancestor_limits_complete {
            CheckStatus::Inconclusive
        } else {
            CheckStatus::Pass
        };
        out.push(finding(
            "cgroup_limits",
            status,
            "Service and ancestor memory limits",
            evidence,
            "An unlimited service setting does not exclude a finite ancestor limit.",
            "Collect the memory limits of all ancestors before evaluating effective headroom.",
            "Verify the service limit and its complete parent hierarchy.",
        ));

        let mut evidence = base();
        for key in ["high", "max", "oom", "oom_kill"] {
            push_opt(&mut evidence, key, service.memory_events.get(key).copied());
        }
        let events_known = ["high", "max", "oom", "oom_kill"]
            .iter()
            .all(|k| service.memory_events.contains_key(*k));
        let event_positive = ["high", "max", "oom", "oom_kill"]
            .iter()
            .any(|k| service.memory_events.get(*k).is_some_and(|v| *v > 0));
        out.push(finding(
            "memory_events",
            if event_positive {
                CheckStatus::Warn
            } else if events_known {
                CheckStatus::Pass
            } else {
                CheckStatus::Inconclusive
            },
            "Recorded cgroup memory events",
            evidence,
            "Counters describe their retained cgroup lifetime, not complete operational history.",
            "Correlate positive counters with an eligible temporal comparison.",
            "Verify cgroup continuity before calculating counter increases.",
        ));

        let mut evidence = base();
        push_opt(&mut evidence, "fd_count", service.fd_count);
        let fd_limit = match service.fd_soft_limit {
            Some(MemoryLimit::Finite(n)) => Some(n),
            _ => None,
        };
        push_opt(&mut evidence, "fd_soft_limit", fd_limit);
        let fd_status = if !service.fd_inspection_complete {
            CheckStatus::Inconclusive
        } else {
            match (service.fd_count, &service.fd_soft_limit) {
                (Some(count), Some(MemoryLimit::Finite(limit))) if *limit > 0 => {
                    if count >= *limit {
                        CheckStatus::Fail
                    } else if count as f64 / *limit as f64 >= 0.90 {
                        CheckStatus::Warn
                    } else {
                        CheckStatus::Pass
                    }
                }
                (Some(_), Some(MemoryLimit::Unlimited)) => CheckStatus::Pass,
                _ => CheckStatus::Inconclusive,
            }
        };
        out.push(finding("file_descriptors", fd_status, "Observed file descriptor use", evidence,
            "Incomplete inspection does not establish descriptor headroom or absence of open files.",
            "Collect descriptor count and process limits with complete inspection.",
            "Verify the same process lifetime and available descriptor headroom."));

        let mut evidence = base();
        let disk_status = if let Some(fs) = &service.data_filesystem {
            push_opt(&mut evidence, "total_bytes", fs.total_bytes);
            push_opt(&mut evidence, "available_bytes", fs.available_bytes);
            push_opt(&mut evidence, "available_inodes", fs.available_inodes);
            match (
                fs.total_bytes,
                fs.available_bytes,
                fs.total_inodes,
                fs.available_inodes,
            ) {
                (Some(total), Some(avail), Some(inodes), Some(free))
                    if total > 0 && avail <= total && inodes > 0 && free <= inodes =>
                {
                    if avail == 0 || free == 0 {
                        CheckStatus::Fail
                    } else if avail as f64 / (total as f64) < 0.05
                        || free as f64 / (inodes as f64) < 0.05
                    {
                        CheckStatus::Warn
                    } else {
                        CheckStatus::Pass
                    }
                }
                _ => CheckStatus::Inconclusive,
            }
        } else {
            CheckStatus::Inconclusive
        };
        out.push(finding("data_filesystem", disk_status, "Filesystem containing service data", evidence,
            "Root filesystem free space does not establish space or inode headroom on a data mount.",
            "Resolve the actual data directory to its filesystem and collect bytes and inodes.",
            "Verify data-directory mount identity and available space."));
    }

    if snapshot.scans.is_empty() {
        out.push(finding(
            "storage_scan",
            CheckStatus::Inconclusive,
            "Storage scan unavailable",
            vec![],
            "Directory usage and aged files have not been measured.",
            "Request a bounded, explicit storage scan if needed.",
            "Verify completeness and resource usage of the scan.",
        ));
    }
    for scan in &snapshot.scans {
        let complete = scan.complete
            && scan.status == CollectionStatus::Complete
            && scan.errors == 0
            && !scan.cleanup_pending
            && scan.observed_apparent_bytes.is_some()
            && scan.observed_allocated_bytes.is_some();
        let mut evidence = vec![
            format!("files_counted={}", scan.files_counted),
            format!("entries_visited={}", scan.entries_visited),
            format!("incomplete={}", !complete),
            format!("errors={}", scan.errors),
        ];
        push_opt(
            &mut evidence,
            "apparent_bytes",
            scan.observed_apparent_bytes,
        );
        push_opt(
            &mut evidence,
            "allocated_bytes",
            scan.observed_allocated_bytes,
        );
        out.push(finding("storage_scan", if complete { CheckStatus::Pass } else { CheckStatus::Inconclusive },
            "Bounded storage observations", evidence,
            "Partial scans report only bytes observed while scanning; concurrent writes or pruning prevent an instantaneous total or lower-bound claim.",
            "Retain partial observations with their stop reason and collection cost.",
            "Verify scan completion, errors, mount boundaries and hard-link deduplication."));
        let open_known = !scan.aged_details_truncated
            && scan.aged_files.iter().all(|f| f.has_open_fd.is_some())
            && scan.aged_files.len() as u64 == scan.aged_files_count;
        let open_count = scan
            .aged_files
            .iter()
            .filter(|f| f.has_open_fd == Some(true))
            .count();
        out.push(finding("aged_files", if !complete || !open_known { CheckStatus::Inconclusive }
            else if scan.aged_files_count > 0 { CheckStatus::Warn } else { CheckStatus::Pass },
            "Observed aged files", vec![format!("aged_files_count={}", scan.aged_files_count),
                format!("apparent_bytes={}", scan.aged_apparent_bytes), format!("allocated_bytes={}", scan.aged_allocated_bytes),
                format!("open_file_count={open_count}"), format!("fd_evidence_complete={open_known}")],
            "File age and an observed lack of open descriptors do not establish safe deletion.",
            "Review application retention and recovery requirements; do not delete based on age alone.",
            "Verify age policy, file identity and complete descriptor inspection separately."));
    }

    let app_stable = snapshot
        .services
        .values()
        .find(|s| s.role == "app")
        .is_some_and(|s| s.identity.stable_during_capture == Some(true));
    out.push(finding("app_sync", match (snapshot.app_status.status, snapshot.app_status.catching_up) {
        (CollectionStatus::Complete, Some(false)) if app_stable && snapshot.app_status.chain_id.is_some() && snapshot.app_status.block_height.is_some() => CheckStatus::Pass,
        (_, Some(true)) => CheckStatus::Warn,
        _ => CheckStatus::Inconclusive,
    }, "Observed application synchronization state", snapshot.app_status.block_height.map(|n| vec![format!("block_height={n}")]).unwrap_or_default(),
        "One status response does not establish progressing height or successful validator signing.",
        "Collect verified loopback status and compare eligible observations.",
        "Verify chain identity, catching-up state and height progress."));
    let fresh = metrics_fresh(snapshot, options);
    out.push(finding(
        "metrics_evidence",
        if snapshot.metrics.status == CollectionStatus::Complete
            && snapshot.metrics.target_verified
            && fresh
        {
            CheckStatus::Pass
        } else {
            CheckStatus::Inconclusive
        },
        "Metrics target and freshness evidence",
        vec![
            format!("target_verified={}", snapshot.metrics.target_verified),
            format!("freshness_verified={fresh}"),
            format!("sample_count={}", snapshot.metrics.samples.len()),
        ],
        "A fresh HTTP scrape alone does not establish recent successful signing activity.",
        "Collect scoped metrics and independently establish relevant activity freshness.",
        "Verify endpoint ownership, chain scope and recent successful observation evidence.",
    ));
    out.push(signing_finding(snapshot, options));
    out.push(storage_budget_finding(snapshot, options));
    out.push(shared_filesystem_finding(snapshot));
    out
}

fn metrics_fresh(snapshot: &SnapshotV1, options: &DoctorOptions) -> bool {
    if !snapshot.metrics.freshness_verified
        || !options.max_metrics_age_seconds.is_finite()
        || options.max_metrics_age_seconds <= 0.0
    {
        return false;
    }
    let reference = options
        .now_epoch_seconds
        .or(snapshot.capture.finished_epoch_seconds)
        .or(snapshot.capture.started_epoch_seconds);
    match (reference, snapshot.metrics.collected_epoch_seconds) {
        (Some(now), Some(collected)) if now.is_finite() && collected.is_finite() => {
            let age = now - collected;
            age >= 0.0 && age <= options.max_metrics_age_seconds
        }
        _ => false,
    }
}

fn sample_integer(sample: &MetricSample) -> Option<u64> {
    if let Some(exact) = sample.exact_integer {
        return (sample.value == exact as f64).then_some(exact);
    }
    // Beyond the safe integer boundary the lexical value is required; a
    // rounded f64 cannot establish an exact successful-call count or byte size.
    (sample.value.is_finite()
        && sample.value >= 0.0
        && sample.value <= (1u64 << 53) as f64
        && sample.value.fract() == 0.0)
        .then_some(sample.value as u64)
}

fn signing_finding(snapshot: &SnapshotV1, options: &DoctorOptions) -> Finding {
    let finish = |status, evidence| {
        finding("signing_latency", status, "Version-scoped signing latency median", evidence,
        "The median covers the last 50 successful signing calls and does not report failure rate or tail latency.",
        "Confirm the exact metric semantics, matching successful count and freshness before applying the 10 ms target.",
        "Verify core v0.42.3, the same target and chain, at least 50 successful calls and recent activity.")
    };
    let observed_versions: Vec<_> = snapshot
        .services
        .iter()
        .filter(|(n, s)| role(n, s) == "app")
        .filter_map(|(_, s)| s.version.as_ref())
        .filter(|v| v.source == ConfigSource::ObservedRuntime)
        .collect();
    let known_wrong_core = observed_versions.iter().any(|v| {
        v.core_version
            .as_deref()
            .is_some_and(|c| c != "v0.42.3" && c != "0.42.3")
    });
    let exact_core = observed_versions
        .iter()
        .any(|v| matches!(v.core_version.as_deref(), Some("v0.42.3" | "0.42.3")));
    let pinned_inference = observed_versions
        .iter()
        .any(|v| v.version == CAPACITY_PROFILE && v.core_version.is_none());
    if known_wrong_core {
        return finish(
            CheckStatus::Unsupported,
            vec!["core_supported=false".into()],
        );
    }
    if !exact_core && !pinned_inference {
        return finish(
            CheckStatus::Inconclusive,
            vec!["core_version_observed=false".into()],
        );
    }
    let mut evidence = vec![
        "core_version=0.42.3".into(),
        format!("core_version_inferred={}", !exact_core),
    ];
    let suffix = "_signing_latency_median_seconds";
    let medians: Vec<_> = snapshot
        .metrics
        .samples
        .iter()
        .filter(|m| {
            m.name.ends_with(suffix)
                && (m.name == "privval_signing_latency_median_seconds"
                    || m.name.ends_with("_privval_signing_latency_median_seconds"))
                && !m.labels.contains_key("message_type")
                && !m
                    .labels
                    .get("chain_id")
                    .zip(snapshot.app_status.chain_id.as_ref())
                    .is_some_and(|(a, b)| a != b)
        })
        .collect();
    if medians.len() != 1 {
        evidence.push(format!("median_series_count={}", medians.len()));
        return finish(CheckStatus::Inconclusive, evidence);
    }
    let median = medians[0];
    if !median.labels.get("chain_id").is_some_and(|v| !v.is_empty()) {
        evidence.push("chain_scope_known=false".into());
        return finish(CheckStatus::Inconclusive, evidence);
    }
    // An untouched gauge is zero during warm-up. A counter alone cannot prove
    // that the median observation has been published or refreshed.
    if !median.value.is_finite() || median.value <= 0.0 {
        evidence.push("median_valid=false".into());
        return finish(CheckStatus::Inconclusive, evidence);
    }
    let prefix = median.name.strip_suffix(suffix).unwrap();
    let count_name = format!("{prefix}_signing_latency_seconds_count");
    let mut seen = BTreeSet::new();
    let mut successful_count = 0u64;
    let mut count_series = 0usize;
    for sample in snapshot
        .metrics
        .samples
        .iter()
        .filter(|m| m.name == count_name)
    {
        let mut labels = sample.labels.clone();
        let message_type = labels.remove("message_type");
        if labels != median.labels {
            continue;
        }
        if message_type.is_none() || !seen.insert(message_type) {
            evidence.push("successful_count_valid=false".into());
            return finish(CheckStatus::Inconclusive, evidence);
        }
        let Some(total) =
            sample_integer(sample).and_then(|count| successful_count.checked_add(count))
        else {
            evidence.push("successful_count_valid=false".into());
            return finish(CheckStatus::Inconclusive, evidence);
        };
        successful_count = total;
        count_series += 1;
    }
    evidence.extend([
        format!("median_seconds={}", median.value),
        format!("successful_count={successful_count}"),
        format!("count_series={count_series}"),
        format!("threshold_seconds={}", options.signing_threshold_seconds),
    ]);
    if count_series == 0
        || successful_count < options.min_signing_samples.max(50)
        || snapshot.metrics.status != CollectionStatus::Complete
        || !snapshot.metrics.target_verified
        || !metrics_fresh(snapshot, options)
        || !options.signing_threshold_seconds.is_finite()
        || options.signing_threshold_seconds != 0.010
    {
        return finish(CheckStatus::Inconclusive, evidence);
    }
    finish(
        if median.value <= options.signing_threshold_seconds {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence,
    )
}

fn storage_budget_finding(snapshot: &SnapshotV1, options: &DoctorOptions) -> Finding {
    // v10.4.0-mocha/fibre/server_metrics.go: logical limiter occupancy includes
    // on-disk reservations and in-flight bytes. It is not filesystem use.
    let finish = |status, evidence| {
        finding("storage_budget", status, "Logical occupancy and storage budget", evidence,
        "Limiter occupancy includes in-flight reservations and differs from staging, shard bytes and filesystem usage.",
        "Verify the target, freshness and effective budget setting before interpreting logical headroom.",
        "Compare one matching occupancy/budget series with the independently measured data filesystem.")
    };
    let samples = &snapshot.metrics.samples;
    let metric = |name: &str| {
        samples
            .iter()
            .filter(|m| m.name == name)
            .collect::<Vec<_>>()
    };
    let occupied = metric("fibre_server_upload_shard_occupancy_bytes");
    let budgets = metric("fibre_server_upload_shard_budget_bytes");
    let mut evidence = vec![
        format!("occupancy_series_count={}", occupied.len()),
        format!("budget_series_count={}", budgets.len()),
    ];
    let mut status = CheckStatus::Inconclusive;
    if let ([occupancy], [budget]) = (occupied.as_slice(), budgets.as_slice()) {
        if let (Some(occupancy_bytes), Some(budget_bytes)) =
            (sample_integer(occupancy), sample_integer(budget))
        {
            if occupancy.labels != budget.labels {
                return finish(CheckStatus::Inconclusive, evidence);
            }
            evidence.push(format!("occupancy_bytes={occupancy_bytes}"));
            evidence.push(format!("budget_bytes={budget_bytes}"));
            let runtime_budget_enabled = snapshot
                .fibre_config
                .get("root.unlimited_budget")
                .is_some_and(|v| {
                    v.source == ConfigSource::ObservedRuntime && v.value.as_bool() == Some(false)
                });
            evidence.push(format!("budget_enforced_observed={runtime_budget_enabled}"));
            if has_observed_version(snapshot, CAPACITY_PROFILE)
                && snapshot.metrics.status == CollectionStatus::Complete
                && snapshot.metrics.target_verified
                && metrics_fresh(snapshot, options)
                && runtime_budget_enabled
                && budget_bytes > 0
            {
                status = if (occupancy_bytes as u128) * 10 >= (budget_bytes as u128) * 9 {
                    CheckStatus::Warn
                } else {
                    CheckStatus::Pass
                };
            }
        }
    }
    finish(status, evidence)
}

fn shared_filesystem_finding(snapshot: &SnapshotV1) -> Finding {
    let device = |wanted: &str| {
        snapshot
            .services
            .iter()
            .filter(|(n, s)| role(n, s) == wanted)
            .filter_map(|(_, s)| s.data_filesystem.as_ref().and_then(|fs| fs.device_id))
            .collect::<Vec<_>>()
    };
    let app = device("app");
    let fibre = device("fibre");
    let mut evidence = Vec::new();
    let status = match (app.as_slice(), fibre.as_slice()) {
        ([a], [b]) => {
            evidence.push(format!("same_filesystem={}", a == b));
            if a == b {
                CheckStatus::Warn
            } else {
                CheckStatus::Inconclusive
            }
        }
        _ => CheckStatus::Inconclusive,
    };
    finding("shared_data_filesystem", status, "Application and Fibre data filesystem relationship", evidence,
        "Sharing a filesystem can couple free-space pressure; different device IDs do not prove separate physical disks.",
        "Observe space and I/O contention for both data paths before making placement decisions.",
        "Verify both paths, mount identities and underlying devices independently.")
}

fn is_legacy(snapshot: &SnapshotV1) -> bool {
    snapshot.capture.input_format == "legacy_v0"
}

fn present_equal<T: PartialEq>(a: Option<&T>, b: Option<&T>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a == b)
}
fn present_diff<T: PartialEq>(a: Option<&T>, b: Option<&T>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a != b)
}

fn identity(
    a: &SnapshotV1,
    b: &SnapshotV1,
    sa: &ServiceSnapshot,
    sb: &ServiceSnapshot,
) -> (ProcessIdentityMatch, Vec<String>) {
    let ia = &sa.identity;
    let ib = &sb.identity;
    let mut reasons = Vec::new();
    let pairs = [
        (
            "host identity differs",
            present_diff(a.capture.host_hash.as_ref(), b.capture.host_hash.as_ref()),
        ),
        (
            "boot identity differs",
            present_diff(a.capture.boot_hash.as_ref(), b.capture.boot_hash.as_ref()),
        ),
        (
            "main PID differs",
            present_diff(ia.pid.as_ref(), ib.pid.as_ref()),
        ),
        (
            "process start ticks differ",
            present_diff(ia.starttime_ticks.as_ref(), ib.starttime_ticks.as_ref()),
        ),
        (
            "service start timestamp differs",
            present_diff(
                ia.service_start_monotonic_us.as_ref(),
                ib.service_start_monotonic_us.as_ref(),
            ),
        ),
        (
            "recorded restart counter differs",
            present_diff(ia.restart_count.as_ref(), ib.restart_count.as_ref()),
        ),
    ];
    for (reason, different) in pairs {
        if different {
            reasons.push(reason.into());
        }
    }
    if !reasons.is_empty() {
        return (ProcessIdentityMatch::Mismatch, reasons);
    }
    if ia.stable_during_capture == Some(false) || ib.stable_during_capture == Some(false) {
        return (
            ProcessIdentityMatch::Unknown,
            vec!["process changed during collection".into()],
        );
    }
    let host = a.capture.host_hash.as_ref().is_some_and(|v| !v.is_empty())
        && present_equal(a.capture.host_hash.as_ref(), b.capture.host_hash.as_ref());
    let boot = a.capture.boot_hash.as_ref().is_some_and(|v| !v.is_empty())
        && present_equal(a.capture.boot_hash.as_ref(), b.capture.boot_hash.as_ref());
    let pid = ia.pid.is_some_and(|p| p > 0) && present_equal(ia.pid.as_ref(), ib.pid.as_ref());
    let ticks = ia.starttime_ticks.is_some_and(|t| t > 0)
        && present_equal(ia.starttime_ticks.as_ref(), ib.starttime_ticks.as_ref());
    if host
        && boot
        && pid
        && ticks
        && ia.stable_during_capture == Some(true)
        && ib.stable_during_capture == Some(true)
    {
        return (
            ProcessIdentityMatch::Verified,
            vec!["host, boot, main PID and process start ticks verified".into()],
        );
    }
    let start = ia.service_start_monotonic_us.is_some_and(|t| t > 0)
        && present_equal(
            ia.service_start_monotonic_us.as_ref(),
            ib.service_start_monotonic_us.as_ref(),
        );
    let restarts = present_equal(ia.restart_count.as_ref(), ib.restart_count.as_ref());
    let cgroup = ia.cgroup_path.as_ref().is_some_and(|p| !p.is_empty())
        && present_equal(ia.cgroup_path.as_ref(), ib.cgroup_path.as_ref());
    if is_legacy(a) && is_legacy(b) && pid && start && restarts && cgroup {
        return (
            ProcessIdentityMatch::HeuristicLegacy,
            vec![
                "legacy PID, service start timestamp, restart counter and cgroup path match".into(),
                "host and boot identity and cgroup generation remain unverified".into(),
            ],
        );
    }
    (
        ProcessIdentityMatch::Unknown,
        vec!["required identity evidence is missing; absence is not a mismatch".into()],
    )
}

fn gauge(name: &str, before: Option<u64>, after: Option<u64>, reason: &str) -> MetricDelta {
    MetricDelta {
        metric: name.into(),
        before,
        after,
        delta: before.zip(after).map(|(a, b)| b as i128 - a as i128),
        rate_per_second: None,
        reason: if before.is_some() && after.is_some() {
            reason.into()
        } else {
            "observation missing".into()
        },
    }
}

fn counter(
    name: &str,
    before: Option<u64>,
    after: Option<u64>,
    eligible: bool,
    elapsed: Option<f64>,
    basis: &str,
    blocked_reason: &str,
) -> MetricDelta {
    let mut out = MetricDelta {
        metric: name.into(),
        before,
        after,
        delta: None,
        rate_per_second: None,
        reason: blocked_reason.into(),
    };
    if !eligible {
        return out;
    }
    let Some(dt) = elapsed.filter(|v| v.is_finite() && *v > 0.0) else {
        out.reason =
            "positive compatible duration unavailable; counter delta and rate withheld".into();
        return out;
    };
    match (before, after) {
        (Some(a), Some(b)) if b >= a => {
            out.delta = Some((b - a) as i128);
            out.rate_per_second = Some((b - a) as f64 / dt);
            out.reason = basis.into();
        }
        (Some(_), Some(_)) => {
            out.reason = "counter decreased or reset; delta and rate unavailable".into()
        }
        _ => out.reason = "counter observation missing".into(),
    }
    out
}

fn scoped_gauge(
    name: &str,
    before: Option<u64>,
    after: Option<u64>,
    eligible: bool,
    reason: &str,
) -> MetricDelta {
    let mut result = gauge(name, before, after, reason);
    if !eligible {
        result.delta = None;
        result.reason = "temporal gauge continuity or positive duration unavailable; observations shown separately".into();
    }
    result
}

/// Cross-environment comparison retains gauges but never yields temporal counter rates.
pub fn compare(a: &SnapshotV1, b: &SnapshotV1, temporal: bool, allow_legacy: bool) -> Comparison {
    let mut out = Comparison {
        mode: if temporal { "temporal" } else { "environments" }.into(),
        elapsed_seconds: None,
        clock_basis: "unavailable".into(),
        services: vec![],
        host_metrics: vec![],
        warnings: vec![],
    };
    let same_boot = a.capture.boot_hash.as_ref().is_some_and(|v| !v.is_empty())
        && present_equal(a.capture.boot_hash.as_ref(), b.capture.boot_hash.as_ref());
    let same_host = a.capture.host_hash.as_ref().is_some_and(|v| !v.is_empty())
        && present_equal(a.capture.host_hash.as_ref(), b.capture.host_hash.as_ref());
    if temporal {
        if same_host && same_boot {
            if let (Some(start), Some(end)) = (a.capture.monotonic_ns, b.capture.monotonic_ns) {
                if end > start {
                    out.elapsed_seconds = Some((end - start) as f64 / 1e9);
                    out.clock_basis = "monotonic".into();
                }
            }
        }
        if out.elapsed_seconds.is_none()
            && is_legacy(a)
            && is_legacy(b)
            && !present_diff(a.capture.host_hash.as_ref(), b.capture.host_hash.as_ref())
            && !present_diff(a.capture.boot_hash.as_ref(), b.capture.boot_hash.as_ref())
        {
            if let (Some(start), Some(end)) = (
                a.capture.started_epoch_seconds,
                b.capture.started_epoch_seconds,
            ) {
                if start.is_finite() && end.is_finite() && end > start {
                    out.elapsed_seconds = Some(end - start);
                    out.clock_basis = "estimated_wall_clock".into();
                    out.warnings.push("Legacy interval uses wall-clock capture starts; clocks may change and sections were read sequentially.".into());
                }
            }
        }
        if out.elapsed_seconds.is_none() {
            out.warnings
                .push("No positive compatible capture duration; rates are unavailable.".into());
        }
    }
    let gauge_reason = if temporal {
        "difference between observed gauges; does not prove continuous growth"
    } else {
        "difference between environments; not a temporal change"
    };
    let legacy_host_assumed = allow_legacy
        && is_legacy(a)
        && is_legacy(b)
        && !a.services.is_empty()
        && a.services.len() == b.services.len()
        && a.services.iter().all(|(name, sa)| {
            b.services
                .get(name)
                .is_some_and(|sb| identity(a, b, sa, sb).0 == ProcessIdentityMatch::HeuristicLegacy)
        });
    let positive_duration = out
        .elapsed_seconds
        .is_some_and(|dt| dt.is_finite() && dt > 0.0);
    let host_gauge_eligible =
        !temporal || positive_duration && (same_host && same_boot || legacy_host_assumed);
    out.host_metrics.push(scoped_gauge(
        "memory_available_bytes",
        a.host.memory_available_bytes,
        b.host.memory_available_bytes,
        host_gauge_eligible,
        gauge_reason,
    ));
    out.host_metrics.push(scoped_gauge(
        "memory_total_bytes",
        a.host.memory_total_bytes,
        b.host.memory_total_bytes,
        host_gauge_eligible,
        gauge_reason,
    ));
    let root_identity = a
        .host
        .root_filesystem
        .as_ref()
        .zip(b.host.root_filesystem.as_ref())
        .is_some_and(|(fa, fb)| {
            present_equal(fa.device_id.as_ref(), fb.device_id.as_ref())
                && present_equal(fa.mount_id.as_ref(), fb.mount_id.as_ref())
        });
    let root_eligible = !temporal || host_gauge_eligible && (root_identity || legacy_host_assumed);
    out.host_metrics.push(scoped_gauge(
        "root_available_bytes",
        a.host
            .root_filesystem
            .as_ref()
            .and_then(|f| f.available_bytes),
        b.host
            .root_filesystem
            .as_ref()
            .and_then(|f| f.available_bytes),
        root_eligible,
        gauge_reason,
    ));
    let units: BTreeSet<_> = a.services.keys().chain(b.services.keys()).collect();
    for unit in units {
        let (Some(sa), Some(sb)) = (a.services.get(unit), b.services.get(unit)) else {
            out.services.push(ServiceComparison {
                service: unit.clone(),
                identity_match: ProcessIdentityMatch::Unknown,
                reasons: vec!["service observation missing on one side".into()],
                metrics: vec![],
            });
            continue;
        };
        let (identity_match, mut reasons) = identity(a, b, sa, sb);
        let cgroup_path = sa
            .identity
            .cgroup_path
            .as_ref()
            .is_some_and(|v| !v.is_empty())
            && present_equal(
                sa.identity.cgroup_path.as_ref(),
                sb.identity.cgroup_path.as_ref(),
            );
        let cgroup_inode = sa.identity.cgroup_inode.is_some_and(|i| i > 0)
            && present_equal(
                sa.identity.cgroup_inode.as_ref(),
                sb.identity.cgroup_inode.as_ref(),
            );
        let cgroup_mismatch = present_diff(
            sa.identity.cgroup_path.as_ref(),
            sb.identity.cgroup_path.as_ref(),
        ) || present_diff(
            sa.identity.cgroup_inode.as_ref(),
            sb.identity.cgroup_inode.as_ref(),
        );
        let legacy_allowed = identity_match == ProcessIdentityMatch::HeuristicLegacy
            && allow_legacy
            && !cgroup_mismatch;
        let scope_verified =
            identity_match == ProcessIdentityMatch::Verified && cgroup_path && cgroup_inode;
        let eligible = temporal && (scope_verified || legacy_allowed);
        let process_gauge_eligible = !temporal
            || positive_duration
                && (identity_match == ProcessIdentityMatch::Verified || legacy_allowed);
        let cgroup_gauge_eligible =
            !temporal || positive_duration && (scope_verified || legacy_allowed);
        if identity_match == ProcessIdentityMatch::HeuristicLegacy {
            reasons.push(
                if allow_legacy {
                    "legacy continuity assumption explicitly enabled"
                } else {
                    "legacy counter comparison requires explicit opt-in"
                }
                .into(),
            );
        }
        if cgroup_mismatch {
            reasons.push("cgroup scope differs; cgroup counters cannot be compared".into());
        } else if !scope_verified && !legacy_allowed {
            reasons.push("cgroup lifetime continuity is unverified".into());
        }
        let blocked = if !temporal {
            "cross-environment counters are not temporal deltas"
        } else {
            "counter continuity is not verified or explicitly accepted"
        };
        let mut metrics = vec![
            scoped_gauge(
                "rss_bytes",
                sa.rss_bytes,
                sb.rss_bytes,
                process_gauge_eligible,
                gauge_reason,
            ),
            scoped_gauge(
                "memory_current_bytes",
                sa.memory_current_bytes,
                sb.memory_current_bytes,
                cgroup_gauge_eligible,
                gauge_reason,
            ),
            scoped_gauge(
                "memory_peak_bytes",
                sa.memory_peak_bytes,
                sb.memory_peak_bytes,
                cgroup_gauge_eligible,
                gauge_reason,
            ),
            scoped_gauge(
                "memory_anon_bytes",
                sa.memory_anon_bytes,
                sb.memory_anon_bytes,
                cgroup_gauge_eligible,
                gauge_reason,
            ),
            scoped_gauge(
                "memory_file_bytes",
                sa.memory_file_bytes,
                sb.memory_file_bytes,
                cgroup_gauge_eligible,
                gauge_reason,
            ),
            scoped_gauge(
                "memory_kernel_bytes",
                sa.memory_kernel_bytes,
                sb.memory_kernel_bytes,
                cgroup_gauge_eligible,
                gauge_reason,
            ),
            scoped_gauge(
                "fd_count",
                sa.fd_count,
                sb.fd_count,
                process_gauge_eligible
                    && (!temporal || sa.fd_inspection_complete && sb.fd_inspection_complete),
                gauge_reason,
            ),
            counter(
                "cpu_usage_ns",
                sa.cpu_usage_ns,
                sb.cpu_usage_ns,
                eligible,
                out.elapsed_seconds,
                &out.clock_basis,
                blocked,
            ),
        ];
        let events: BTreeSet<_> = sa
            .memory_events
            .keys()
            .chain(sb.memory_events.keys())
            .collect();
        for key in events {
            metrics.push(counter(
                &format!("memory_events.{key}"),
                sa.memory_events.get(key).copied(),
                sb.memory_events.get(key).copied(),
                eligible,
                out.elapsed_seconds,
                &out.clock_basis,
                blocked,
            ));
        }
        out.services.push(ServiceComparison {
            service: unit.clone(),
            identity_match,
            reasons,
            metrics,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_sync_pass_requires_capture_wide_stable_process() {
        let mut snapshot = SnapshotV1 {
            app_status: AppStatus {
                status: CollectionStatus::Complete,
                catching_up: Some(false),
                chain_id: Some("lab".into()),
                block_height: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        snapshot.services.insert(
            "app.service".into(),
            ServiceSnapshot {
                role: "app".into(),
                ..Default::default()
            },
        );
        for (stable, expected) in [
            (None, CheckStatus::Inconclusive),
            (Some(false), CheckStatus::Inconclusive),
            (Some(true), CheckStatus::Pass),
        ] {
            snapshot
                .services
                .get_mut("app.service")
                .unwrap()
                .identity
                .stable_during_capture = stable;
            let findings = doctor(&snapshot, &DoctorOptions::default());
            assert_eq!(
                findings.iter().find(|f| f.id == "app_sync").unwrap().status,
                expected
            );
        }
    }
    use std::collections::BTreeMap;

    fn pair(legacy: bool) -> (SnapshotV1, SnapshotV1) {
        let mut a = SnapshotV1 {
            capture: Capture {
                started_epoch_seconds: Some(1000.0),
                monotonic_ns: Some(1_000_000_000),
                host_hash: if legacy { None } else { Some("host".into()) },
                boot_hash: if legacy { None } else { Some("boot".into()) },
                input_format: if legacy { "legacy_v0" } else { "v1" }.into(),
                ..Capture::default()
            },
            ..SnapshotV1::default()
        };
        a.services.insert(
            "fibre.service".into(),
            ServiceSnapshot {
                role: "fibre".into(),
                cpu_usage_ns: Some(1_000_000_000),
                rss_bytes: Some(100),
                identity: ProcessIdentity {
                    pid: Some(50),
                    starttime_ticks: if legacy { None } else { Some(20) },
                    service_start_monotonic_us: Some(10),
                    restart_count: Some(0),
                    stable_during_capture: if legacy { None } else { Some(true) },
                    cgroup_path: Some("/system.slice/fibre.service".into()),
                    cgroup_inode: if legacy { None } else { Some(99) },
                },
                ..ServiceSnapshot::default()
            },
        );
        let mut b = a.clone();
        b.capture.started_epoch_seconds = Some(1010.0);
        b.capture.monotonic_ns = Some(11_000_000_000);
        b.services.get_mut("fibre.service").unwrap().cpu_usage_ns = Some(2_000_000_000);
        b.services.get_mut("fibre.service").unwrap().rss_bytes = Some(120);
        (a, b)
    }
    fn cpu(result: &Comparison) -> &MetricDelta {
        result.services[0]
            .metrics
            .iter()
            .find(|m| m.metric == "cpu_usage_ns")
            .unwrap()
    }

    #[test]
    fn verified_counter_uses_monotonic_time() {
        let (a, mut b) = pair(false);
        b.capture.started_epoch_seconds = Some(900.0);
        let r = compare(&a, &b, true, false);
        assert_eq!(r.services[0].identity_match, ProcessIdentityMatch::Verified);
        assert_eq!(r.clock_basis, "monotonic");
        assert_eq!(r.elapsed_seconds, Some(10.0));
        assert_eq!(cpu(&r).delta, Some(1_000_000_000));
        assert_eq!(cpu(&r).rate_per_second, Some(100_000_000.0));
    }
    #[test]
    fn legacy_requires_opt_in_and_marks_estimated_clock() {
        let (a, b) = pair(true);
        let strict = compare(&a, &b, true, false);
        assert_eq!(cpu(&strict).delta, None);
        let r = compare(&a, &b, true, true);
        assert_eq!(
            r.services[0].identity_match,
            ProcessIdentityMatch::HeuristicLegacy
        );
        assert_eq!(r.clock_basis, "estimated_wall_clock");
        assert!(cpu(&r).rate_per_second.is_some());
        let mut missing = b.clone();
        missing
            .services
            .get_mut("fibre.service")
            .unwrap()
            .identity
            .service_start_monotonic_us = None;
        assert_eq!(
            compare(&a, &missing, true, true).services[0].identity_match,
            ProcessIdentityMatch::Unknown
        );
    }
    #[test]
    fn a_b_gauges_never_have_temporal_rates() {
        let (a, mut b) = pair(false);
        b.capture.host_hash = Some("other".into());
        let r = compare(&a, &b, false, true);
        assert_eq!(r.services[0].identity_match, ProcessIdentityMatch::Mismatch);
        assert_eq!(r.elapsed_seconds, None);
        assert_eq!(cpu(&r).delta, None);
        assert!(r.services[0]
            .metrics
            .iter()
            .all(|m| m.rate_per_second.is_none()));
        assert_eq!(
            r.services[0]
                .metrics
                .iter()
                .find(|m| m.metric == "rss_bytes")
                .unwrap()
                .delta,
            Some(20)
        );
    }
    #[test]
    fn counter_reset_and_changed_cgroup_block_rates() {
        let (a, mut b) = pair(false);
        b.services.get_mut("fibre.service").unwrap().cpu_usage_ns = Some(1);
        let reset = compare(&a, &b, true, false);
        assert_eq!(cpu(&reset).delta, None);
        b.services.get_mut("fibre.service").unwrap().cpu_usage_ns = Some(2_000_000_000);
        b.services
            .get_mut("fibre.service")
            .unwrap()
            .identity
            .cgroup_inode = Some(100);
        let changed = compare(&a, &b, true, false);
        assert_eq!(
            changed.services[0].identity_match,
            ProcessIdentityMatch::Verified
        );
        assert_eq!(cpu(&changed).delta, None);
    }
    #[test]
    fn nonpositive_duration_and_missing_v1_monotonic_never_rate() {
        let (a, mut b) = pair(false);
        b.capture.monotonic_ns = a.capture.monotonic_ns;
        assert_eq!(cpu(&compare(&a, &b, true, false)).rate_per_second, None);
        assert_eq!(cpu(&compare(&a, &b, true, false)).delta, None);
        b.capture.monotonic_ns = None;
        assert_eq!(cpu(&compare(&a, &b, true, false)).rate_per_second, None);
        let (a, mut b) = pair(true);
        b.capture.started_epoch_seconds = a.capture.started_epoch_seconds;
        assert_eq!(cpu(&compare(&a, &b, true, true)).rate_per_second, None);
        assert!(compare(&a, &b, true, true).services[0]
            .metrics
            .iter()
            .all(|m| m.delta.is_none()));
    }
    #[test]
    fn temporal_gauges_require_their_own_scope_and_legacy_opt_in() {
        let (a, mut b) = pair(false);
        b.services
            .get_mut("fibre.service")
            .unwrap()
            .identity
            .cgroup_inode = Some(101);
        let result = compare(&a, &b, true, false);
        let metrics = &result.services[0].metrics;
        assert_eq!(
            metrics
                .iter()
                .find(|m| m.metric == "rss_bytes")
                .unwrap()
                .delta,
            Some(20)
        );
        assert_eq!(
            metrics
                .iter()
                .find(|m| m.metric == "memory_current_bytes")
                .unwrap()
                .delta,
            None
        );
        b.capture.host_hash = Some("different".into());
        assert!(compare(&a, &b, true, false).services[0]
            .metrics
            .iter()
            .all(|m| m.delta.is_none()));
        let (a, b) = pair(true);
        assert!(compare(&a, &b, true, false).services[0]
            .metrics
            .iter()
            .all(|m| m.delta.is_none()));
        assert_eq!(
            compare(&a, &b, true, true).services[0]
                .metrics
                .iter()
                .find(|m| m.metric == "rss_bytes")
                .unwrap()
                .delta,
            Some(20)
        );
    }
    #[test]
    fn missing_identity_is_unknown_not_mismatch() {
        let (a, mut b) = pair(false);
        b.capture.host_hash = None;
        let r = compare(&a, &b, true, false);
        assert_eq!(r.services[0].identity_match, ProcessIdentityMatch::Unknown);
        assert_eq!(cpu(&r).delta, None);
    }
    #[test]
    fn unstable_or_restarted_process_blocks_counters() {
        let (a, mut b) = pair(false);
        b.services
            .get_mut("fibre.service")
            .unwrap()
            .identity
            .stable_during_capture = Some(false);
        assert_eq!(cpu(&compare(&a, &b, true, false)).delta, None);
        b.services
            .get_mut("fibre.service")
            .unwrap()
            .identity
            .stable_during_capture = Some(true);
        b.services
            .get_mut("fibre.service")
            .unwrap()
            .identity
            .restart_count = Some(1);
        assert_eq!(
            compare(&a, &b, true, false).services[0].identity_match,
            ProcessIdentityMatch::Mismatch
        );
    }

    fn config_snapshot() -> SnapshotV1 {
        let mut s = SnapshotV1::default();
        for (key, value) in [
            ("root.max_connections", 16),
            ("root.max_concurrent_streams", 13),
        ] {
            s.fibre_config.insert(
                key.into(),
                ConfigValue {
                    value: value.into(),
                    source: ConfigSource::ConfigFile,
                    evidence: String::new(),
                },
            );
        }
        s
    }
    #[test]
    fn capacity_requires_exact_version_or_explicit_profile() {
        let mut s = config_snapshot();
        assert_eq!(capacity(&s, None, None).status, CheckStatus::Inconclusive);
        assert_eq!(
            capacity(&s, Some("v10.4"), None).status,
            CheckStatus::Unsupported
        );
        let r = capacity(&s, Some(CAPACITY_PROFILE), None);
        assert_eq!(r.receive_buffer_bytes, Some(16 * 13 * 132 * 1024 * 1024));
        assert_eq!(
            r.scenarios
                .iter()
                .map(|v| v.connections)
                .collect::<Vec<_>>(),
            vec![16, 24, 32]
        );
        assert_eq!(
            r.scenarios
                .iter()
                .map(|v| v.receive_buffer_bytes as f64 / 1024f64.powi(3))
                .collect::<Vec<_>>(),
            vec![26.8125, 40.21875, 53.625]
        );
        assert_eq!(r.profile_source, ConfigSource::DefaultAssumed);
        assert_eq!(r.status, CheckStatus::Warn);
        s.services.insert(
            "fibre.service".into(),
            ServiceSnapshot {
                version: Some(VersionEvidence {
                    version: CAPACITY_PROFILE.into(),
                    source: ConfigSource::ObservedRuntime,
                    ..VersionEvidence::default()
                }),
                ..ServiceSnapshot::default()
            },
        );
        assert_eq!(
            capacity(&s, None, None).profile_source,
            ConfigSource::ObservedRuntime
        );
    }
    #[test]
    fn application_version_does_not_prove_fibre_version() {
        let mut s = config_snapshot();
        s.services.insert(
            "celestia-appd.service".into(),
            ServiceSnapshot {
                role: "app".into(),
                version: Some(VersionEvidence {
                    version: CAPACITY_PROFILE.into(),
                    source: ConfigSource::ObservedRuntime,
                    ..VersionEvidence::default()
                }),
                ..ServiceSnapshot::default()
            },
        );
        assert_eq!(
            capacity(&s, None, None).profile_source,
            ConfigSource::Unknown
        );
        assert_eq!(capacity(&s, None, None).receive_buffer_bytes, None);
    }
    #[test]
    fn capacity_does_not_invent_config_or_overflow() {
        let mut s = config_snapshot();
        assert_eq!(
            capacity(&s, Some(CAPACITY_PROFILE), Some(u64::MAX)).receive_buffer_bytes,
            None
        );
        assert_eq!(
            capacity(&s, Some(CAPACITY_PROFILE), Some(0)).receive_buffer_bytes,
            None
        );
        s.fibre_config.remove("root.max_concurrent_streams");
        assert_eq!(
            capacity(&s, Some(CAPACITY_PROFILE), None).receive_buffer_bytes,
            None
        );
    }
    #[test]
    fn absent_evidence_never_passes_and_root_mount_is_not_data_mount() {
        let mut s = SnapshotV1::default();
        s.host.root_filesystem = Some(FilesystemSnapshot {
            total_bytes: Some(100),
            available_bytes: Some(99),
            ..FilesystemSnapshot::default()
        });
        s.services.insert(
            "fibre.service".into(),
            ServiceSnapshot {
                memory_max: Some(MemoryLimit::Unlimited),
                ..ServiceSnapshot::default()
            },
        );
        let checks = doctor(&s, &DoctorOptions::default());
        assert!(checks.iter().all(|f| f.status != CheckStatus::Pass));
        for id in [
            "data_filesystem",
            "cgroup_limits",
            "file_descriptors",
            "app_sync",
            "signing_latency",
        ] {
            assert!(checks
                .iter()
                .any(|f| f.id == id && f.status == CheckStatus::Inconclusive));
        }
    }
    #[test]
    fn partial_scan_and_unknown_open_fd_never_certify_cleanup() {
        let mut s = SnapshotV1::default();
        s.scans.push(ScanResult {
            complete: false,
            status: CollectionStatus::Timeout,
            aged_files_count: 1,
            aged_files: vec![AgedFile {
                has_open_fd: None,
                ..AgedFile::default()
            }],
            ..ScanResult::default()
        });
        let checks = doctor(&s, &DoctorOptions::default());
        assert_eq!(
            checks
                .iter()
                .find(|f| f.id == "storage_scan")
                .unwrap()
                .status,
            CheckStatus::Inconclusive
        );
        assert_eq!(
            checks.iter().find(|f| f.id == "aged_files").unwrap().status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn partial_ancestor_hierarchy_never_passes_and_high_is_not_hard_limit() {
        let mut s = SnapshotV1::default();
        let mut service = ServiceSnapshot {
            memory_max: Some(MemoryLimit::Unlimited),
            memory_current_bytes: Some(100),
            ancestor_limits: BTreeMap::from([
                ("/system.slice:memory.max".into(), MemoryLimit::Unlimited),
                ("/system.slice:memory.high".into(), MemoryLimit::Finite(50)),
            ]),
            ..ServiceSnapshot::default()
        };
        s.services.insert("fibre.service".into(), service.clone());
        service.memory_current_bytes = Some(10);
        s.services.insert("fibre.service".into(), service.clone());
        assert_eq!(
            doctor(&s, &DoctorOptions::default())
                .iter()
                .find(|f| f.id == "cgroup_limits")
                .unwrap()
                .status,
            CheckStatus::Inconclusive
        );
        service.ancestor_limits_complete = true;
        s.services.insert("fibre.service".into(), service);
        let findings = doctor(&s, &DoctorOptions::default());
        let limits = findings.iter().find(|f| f.id == "cgroup_limits").unwrap();
        assert_eq!(limits.status, CheckStatus::Pass);
        assert!(!limits
            .evidence
            .iter()
            .any(|e| e.starts_with("effective_limit_bytes=")));
        assert!(limits
            .evidence
            .iter()
            .any(|e| e == "pressure_threshold_bytes=50"));
        s.services
            .get_mut("fibre.service")
            .unwrap()
            .memory_current_bytes = Some(100);
        assert_eq!(
            doctor(&s, &DoctorOptions::default())
                .iter()
                .find(|f| f.id == "cgroup_limits")
                .unwrap()
                .status,
            CheckStatus::Warn
        );
    }

    fn signing_snapshot() -> (SnapshotV1, DoctorOptions) {
        let mut s = SnapshotV1::default();
        s.services.insert(
            "celestia-appd.service".into(),
            ServiceSnapshot {
                role: "app".into(),
                version: Some(VersionEvidence {
                    version: CAPACITY_PROFILE.into(),
                    core_version: Some("v0.42.3".into()),
                    source: ConfigSource::ObservedRuntime,
                    ..VersionEvidence::default()
                }),
                ..ServiceSnapshot::default()
            },
        );
        s.app_status.chain_id = Some("test-chain".into());
        let labels = BTreeMap::from([
            ("chain_id".into(), "test-chain".into()),
            ("instance".into(), "verified-target".into()),
        ]);
        s.metrics = MetricsSnapshot {
            status: CollectionStatus::Complete,
            target_verified: true,
            freshness_verified: true,
            collected_epoch_seconds: Some(1000.0),
            samples: vec![MetricSample {
                name: "custom_privval_signing_latency_median_seconds".into(),
                labels: labels.clone(),
                value: 0.009,
                ..MetricSample::default()
            }],
            ..MetricsSnapshot::default()
        };
        for (message_type, count) in [("vote", 20.0), ("proposal", 20.0), ("raw_bytes", 10.0)] {
            let mut labels = labels.clone();
            labels.insert("message_type".into(), message_type.into());
            s.metrics.samples.push(MetricSample {
                name: "custom_privval_signing_latency_seconds_count".into(),
                labels,
                value: count,
                ..MetricSample::default()
            });
        }
        (
            s,
            DoctorOptions {
                now_epoch_seconds: Some(1010.0),
                ..DoctorOptions::default()
            },
        )
    }
    #[test]
    fn signing_aggregates_successes_across_message_types_and_namespace() {
        let (s, options) = signing_snapshot();
        assert_eq!(signing_finding(&s, &options).status, CheckStatus::Pass);
        let mut slow = s.clone();
        slow.metrics.samples[0].value = 0.011;
        assert_eq!(signing_finding(&slow, &options).status, CheckStatus::Warn);
        let mut underfilled = s;
        underfilled.metrics.samples[3].value = 9.0;
        assert_eq!(
            signing_finding(&underfilled, &options).status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn zero_median_remains_inconclusive_even_after_fifty_counted_calls() {
        let (mut s, options) = signing_snapshot();
        s.metrics.samples[0].value = 0.0;
        assert_eq!(
            signing_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn signing_count_retains_exact_large_integers_and_rejects_overflow() {
        let (mut s, options) = signing_snapshot();
        let exact = (1u64 << 53) + 1;
        s.metrics.samples[1].exact_integer = Some(exact);
        s.metrics.samples[1].value = exact as f64;
        let finding = signing_finding(&s, &options);
        assert_eq!(finding.status, CheckStatus::Pass);
        assert!(finding
            .evidence
            .contains(&format!("successful_count={}", exact + 30)));
        s.metrics.samples[1].exact_integer = Some(u64::MAX);
        s.metrics.samples[1].value = u64::MAX as f64;
        assert_eq!(
            signing_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
        s.metrics.samples[1].exact_integer = None;
        assert_eq!(
            signing_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn historical_signing_is_evaluated_at_capture_not_report_open_time() {
        let (mut s, _) = signing_snapshot();
        s.capture.started_epoch_seconds = Some(1000.0);
        s.capture.finished_epoch_seconds = Some(1010.0);
        assert_eq!(
            signing_finding(&s, &DoctorOptions::default()).status,
            CheckStatus::Pass
        );
        s.capture.finished_epoch_seconds = Some(1201.0);
        assert_eq!(
            signing_finding(&s, &DoctorOptions::default()).status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn storage_budget_is_logical_scoped_and_requires_runtime_limit_evidence() {
        let (mut s, options) = signing_snapshot();
        s.services.insert(
            "fibre.service".into(),
            ServiceSnapshot {
                role: "fibre".into(),
                version: Some(VersionEvidence {
                    version: CAPACITY_PROFILE.into(),
                    source: ConfigSource::ObservedRuntime,
                    ..VersionEvidence::default()
                }),
                ..ServiceSnapshot::default()
            },
        );
        s.fibre_config.insert(
            "root.unlimited_budget".into(),
            ConfigValue {
                value: false.into(),
                source: ConfigSource::ObservedRuntime,
                evidence: String::new(),
            },
        );
        s.metrics.samples = vec![
            MetricSample {
                name: "fibre_server_upload_shard_occupancy_bytes".into(),
                value: 90.0,
                ..Default::default()
            },
            MetricSample {
                name: "fibre_server_upload_shard_budget_bytes".into(),
                value: 100.0,
                ..Default::default()
            },
        ];
        assert_eq!(
            storage_budget_finding(&s, &options).status,
            CheckStatus::Warn
        );
        s.metrics.samples[0].value = 10.0;
        assert_eq!(
            storage_budget_finding(&s, &options).status,
            CheckStatus::Pass
        );
        s.fibre_config
            .get_mut("root.unlimited_budget")
            .unwrap()
            .source = ConfigSource::ConfigFile;
        assert_eq!(
            storage_budget_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
        s.fibre_config
            .get_mut("root.unlimited_budget")
            .unwrap()
            .source = ConfigSource::ObservedRuntime;
        s.metrics.samples[1]
            .labels
            .insert("instance".into(), "other".into());
        assert_eq!(
            storage_budget_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn shared_device_warns_but_different_devices_do_not_prove_isolation() {
        let mut s = SnapshotV1::default();
        for name in ["celestia-appd.service", "fibre.service"] {
            s.services.insert(
                name.into(),
                ServiceSnapshot {
                    data_filesystem: Some(FilesystemSnapshot {
                        device_id: Some(123),
                        ..FilesystemSnapshot::default()
                    }),
                    ..ServiceSnapshot::default()
                },
            );
        }
        assert_eq!(shared_filesystem_finding(&s).status, CheckStatus::Warn);
        s.services
            .get_mut("fibre.service")
            .unwrap()
            .data_filesystem
            .as_mut()
            .unwrap()
            .device_id = Some(456);
        assert_eq!(
            shared_filesystem_finding(&s).status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn stale_unverified_and_wrong_target_metrics_never_pass() {
        let (s, options) = signing_snapshot();
        for field in ["freshness", "target", "stale", "chain", "version"] {
            let mut copy = s.clone();
            match field {
                "freshness" => copy.metrics.freshness_verified = false,
                "target" => copy.metrics.target_verified = false,
                "stale" => copy.metrics.collected_epoch_seconds = Some(1.0),
                "chain" => copy.metrics.samples[3]
                    .labels
                    .insert("chain_id".into(), "other-chain".into())
                    .map(|_| ())
                    .unwrap_or(()),
                "version" => {
                    copy.services
                        .get_mut("celestia-appd.service")
                        .unwrap()
                        .version
                        .as_mut()
                        .unwrap()
                        .core_version = Some("v0.42.4".into())
                }
                _ => unreachable!(),
            }
            assert_ne!(
                signing_finding(&copy, &options).status,
                CheckStatus::Pass,
                "{field}"
            );
        }
    }
    #[test]
    fn duplicate_or_message_typed_median_is_not_median_of_last_fifty() {
        let (mut s, options) = signing_snapshot();
        s.metrics.samples.push(s.metrics.samples[1].clone());
        assert_eq!(
            signing_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
        s.metrics.samples.pop();
        s.metrics.samples[0]
            .labels
            .insert("message_type".into(), "vote".into());
        assert_eq!(
            signing_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn signing_requires_chain_scope_and_privval_subsystem() {
        let (mut s, options) = signing_snapshot();
        for sample in &mut s.metrics.samples {
            sample.labels.remove("chain_id");
        }
        assert_eq!(
            signing_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
        let (mut s, options) = signing_snapshot();
        for sample in &mut s.metrics.samples {
            sample.name = sample.name.replace("privval_", "unrelated_");
        }
        assert_eq!(
            signing_finding(&s, &options).status,
            CheckStatus::Inconclusive
        );
    }
    #[test]
    fn synthetic_legacy_pair_has_heuristic_identity_and_exact_counter_deltas() {
        let a = crate::parser::parse_snapshot(include_bytes!(
            "../tests/fixtures/legacy/snapshot-a.json"
        ))
        .unwrap();
        let b = crate::parser::parse_snapshot(include_bytes!(
            "../tests/fixtures/legacy/snapshot-b.json"
        ))
        .unwrap();
        let comparison = compare(&a, &b, true, true);
        assert!((comparison.elapsed_seconds.unwrap() - 83.4120102).abs() < 0.000001);
        assert!(comparison
            .services
            .iter()
            .all(|s| s.identity_match == ProcessIdentityMatch::HeuristicLegacy));
        assert_eq!(comparison.clock_basis, "estimated_wall_clock");
        let app = comparison
            .services
            .iter()
            .find(|s| s.service == "celestia-appd.service")
            .unwrap();
        let cpu = app
            .metrics
            .iter()
            .find(|m| m.metric == "cpu_usage_ns")
            .unwrap();
        assert_eq!(cpu.delta, Some(8_000_000_000));
        assert!((cpu.rate_per_second.unwrap() / 1e9 - 8.0 / 83.4120102).abs() < 0.000001);
        assert_eq!(
            app.metrics
                .iter()
                .find(|m| m.metric == "memory_current_bytes")
                .unwrap()
                .delta,
            Some(67_108_864)
        );
        assert_eq!(capacity(&a, None, None).status, CheckStatus::Inconclusive);
    }
}
