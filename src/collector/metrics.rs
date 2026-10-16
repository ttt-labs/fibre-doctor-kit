use super::parsers;
use crate::model::*;
use std::collections::BTreeMap;

pub(super) fn parse_app_status(text: &str) -> AppStatus {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return AppStatus {
            status: CollectionStatus::Error,
            ..Default::default()
        };
    };
    let r = &v["result"];
    let height = r["sync_info"]["latest_block_height"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| r["sync_info"]["latest_block_height"].as_u64());
    AppStatus {
        status: if height.is_some() {
            CollectionStatus::Complete
        } else {
            CollectionStatus::Partial
        },
        chain_id: r["node_info"]["network"]
            .as_str()
            .filter(|s| s.len() <= 128)
            .map(str::to_string),
        block_height: height,
        block_time: r["sync_info"]["latest_block_time"]
            .as_str()
            .filter(|s| s.len() <= 128)
            .map(str::to_string),
        catching_up: r["sync_info"]["catching_up"].as_bool(),
    }
}
fn allowed(name: &str) -> bool {
    if name.len() > 192
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:".contains(&b))
    {
        return false;
    }
    if let Some((_, tail)) = name.split_once("privval_") {
        return [
            "signing_latency_seconds_bucket",
            "signing_latency_seconds_sum",
            "signing_latency_seconds_count",
            "signing_latency_median_seconds",
            "signing_failures_total",
        ]
        .contains(&tail);
    }
    let tail = if let Some((_, s)) = name.split_once("fibre_server_") {
        s
    } else if let Some((_, s)) = name.split_once("fibre_client_") {
        s
    } else {
        return false;
    };
    [
        "upload_shard_",
        "download_shard_",
        "store_put_",
        "store_get_",
        "backend_get_",
        "sign_",
        "prune_",
        "upload_",
        "upload_to_",
        "download_",
        "download_from_",
    ]
    .iter()
    .any(|p| tail.starts_with(p))
        && [
            "in_flight",
            "duration_seconds_bucket",
            "duration_seconds_count",
            "duration_seconds_sum",
            "bytes_total",
            "data_bytes_total",
            "network_bytes_total",
            "rejected_total",
            "dupe_hits_total",
            "occupancy_bytes",
            "budget_bytes",
            "last_success_timestamp_seconds",
            "entries_total",
            "rpc_latency_seconds_bucket",
            "rpc_latency_seconds_count",
            "rpc_latency_seconds_sum",
            "signatures_collected_bucket",
            "signatures_collected_count",
            "signatures_collected_sum",
        ]
        .iter()
        .any(|s| tail.ends_with(s))
}
fn parse_labels(text: &str) -> Option<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    let mut input = text.trim();
    let mut count = 0;
    while !input.is_empty() {
        count += 1;
        if count > 32 {
            return None;
        }
        let (key, rest) = input.split_once('=')?;
        let key = key.trim();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return None;
        }
        let mut chars = rest.trim_start().char_indices();
        if chars.next()?.1 != '"' {
            return None;
        }
        let mut value = String::new();
        let mut escape = false;
        let mut end = None;
        for (i, ch) in chars {
            if escape {
                value.push(match ch {
                    'n' => '\n',
                    '\\' => '\\',
                    '"' => '"',
                    _ => return None,
                });
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                end = Some(i + 1);
                break;
            } else {
                value.push(ch);
            }
            if value.len() > 512 {
                return None;
            }
        }
        let rest = rest.trim_start();
        input = rest.get(end?..)?.trim_start();
        if !input.is_empty() {
            input = input.strip_prefix(',')?.trim_start();
        }
        if [
            "chain_id",
            "message_type",
            "reason",
            "job",
            "instance",
            "service_name",
            "service_instance_id",
            "success",
            "outcome",
            "upload_size",
            "shard_size",
            "blob_size",
            "backend",
            "le",
            "stage",
            "validator_address",
        ]
        .contains(&key)
        {
            if ["instance", "service_instance_id", "validator_address"].contains(&key) {
                value = format!("sha256:{}", parsers::hash(value.as_bytes()));
            }
            if result.insert(key.into(), value).is_some() {
                return None;
            }
        }
    }
    Some(result)
}
#[cfg(any(target_os = "linux", test))]
pub(super) fn parse_metrics(text: &str, source: &str, verified: bool) -> MetricsSnapshot {
    parse_metrics_at(text, source, verified, Some(super::epoch()))
}
pub(super) fn parse_metrics_at(
    text: &str,
    source: &str,
    verified: bool,
    collected_epoch_seconds: Option<f64>,
) -> MetricsSnapshot {
    let mut out = MetricsSnapshot {
        status: CollectionStatus::Complete,
        source: source.into(),
        collected_epoch_seconds,
        target_verified: verified,
        freshness_verified: false,
        ..Default::default()
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.len() > 8192 {
            out.status = CollectionStatus::Partial;
            continue;
        }
        let name_end = line
            .find(|c: char| c == '{' || c.is_ascii_whitespace())
            .unwrap_or(line.len());
        let name = &line[..name_end];
        if !allowed(name) {
            continue;
        }
        let mut tail = &line[name_end..];
        let labels = if tail.starts_with('{') {
            let mut quoted = false;
            let mut escaped = false;
            let mut end = None;
            for (i, c) in tail.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if c == '\\' && quoted {
                    escaped = true;
                } else if c == '"' {
                    quoted = !quoted;
                } else if c == '}' && !quoted {
                    end = Some(i);
                    break;
                }
            }
            let Some(end) = end else {
                out.status = CollectionStatus::Partial;
                continue;
            };
            let labels = parse_labels(&tail[1..end]);
            tail = &tail[end + 1..];
            let Some(labels) = labels else {
                out.status = CollectionStatus::Partial;
                continue;
            };
            labels
        } else {
            BTreeMap::new()
        };
        let mut pieces = tail.split_whitespace();
        let Some(raw) = pieces.next() else {
            out.status = CollectionStatus::Partial;
            continue;
        };
        let Some(value) = raw.parse::<f64>().ok().filter(|n| n.is_finite()) else {
            out.status = CollectionStatus::Partial;
            continue;
        };
        let stamp = pieces
            .next()
            .filter(|s| *s != "#")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s >= 0.)
            .map(|ms| ms / 1000.);
        if out.samples.len() >= 4096 {
            out.status = CollectionStatus::Partial;
            break;
        }
        out.samples.push(MetricSample {
            name: name.into(),
            labels,
            value,
            exact_integer: raw.parse().ok(),
            timestamp_epoch_seconds: stamp,
        });
    }
    if out.samples.is_empty() && out.status == CollectionStatus::Complete {
        out.status = CollectionStatus::Missing;
    }
    out
}

#[derive(Debug, Clone)]
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) struct HttpTarget {
    pub addr: std::net::SocketAddr,
    pub path: String,
}
#[cfg(any(target_os = "linux", test))]
pub(super) fn loopback_target(url: &str) -> Result<HttpTarget, String> {
    let tail = url
        .strip_prefix("http://")
        .ok_or("Only existing plaintext HTTP loopback endpoints are supported")?;
    if tail.len() > 4096
        || tail.bytes().any(|b| b <= 32 || b >= 127)
        || tail.contains('@')
        || tail.contains('?')
        || tail.contains('#')
    {
        return Err(
            "Endpoint must not contain credentials, query parameters, or control characters".into(),
        );
    }
    let (authority, path) = tail
        .split_once('/')
        .map(|(a, p)| (a, format!("/{p}")))
        .unwrap_or((tail, "/".into()));
    let addr: std::net::SocketAddr = authority
        .parse()
        .map_err(|_| "Endpoint requires a numeric loopback IP and explicit port")?;
    if !addr.ip().is_loopback() || addr.port() == 0 {
        return Err("Endpoint is not numeric loopback".into());
    }
    Ok(HttpTarget { addr, path })
}

/// Accept only a literal local Prometheus exporter enabled in a metrics pipeline.
/// No YAML includes, environment expansion, remote configs, or extension execution.
#[cfg(test)]
pub(super) fn otel_exporter_url(text: &str) -> Result<String, String> {
    otel_exporter_for_receiver(text, None)
}

/// When a Fibre OTLP endpoint is known, require the HTTP receiver and exporter
/// to participate in the same enabled metrics pipeline. Literal YAML only.
#[cfg(any(target_os = "linux", test))]
pub(super) fn otel_exporter_for_receiver(
    text: &str,
    receiver: Option<std::net::SocketAddr>,
) -> Result<String, String> {
    let root: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|_| "Unsupported OTel Collector YAML")?;
    let pipelines = root["service"]["pipelines"]
        .as_mapping()
        .ok_or("No metrics pipeline")?;
    let mut names = Vec::new();
    for (name, pipeline) in pipelines {
        if name
            .as_str()
            .is_some_and(|s| s == "metrics" || s.starts_with("metrics/"))
        {
            if let Some(expected) = receiver {
                let receiver_matches = pipeline["receivers"].as_sequence().is_some_and(|names| {
                    names
                        .iter()
                        .filter_map(serde_yaml::Value::as_str)
                        .any(|name| {
                            if name != "otlp" && !name.starts_with("otlp/") {
                                return false;
                            }
                            let http = &root["receivers"][name]["protocols"]["http"];
                            if !http["tls"].is_null() {
                                return false;
                            }
                            http["endpoint"]
                                .as_str()
                                .and_then(|s| s.parse::<std::net::SocketAddr>().ok())
                                .is_some_and(|actual| {
                                    actual.port() == expected.port()
                                        && (actual.ip() == expected.ip()
                                            || (actual.ip().is_unspecified()
                                                && actual.is_ipv4() == expected.is_ipv4()))
                                })
                        })
                });
                if !receiver_matches {
                    continue;
                }
            }
            if let Some(exporters) = pipeline["exporters"].as_sequence() {
                for exporter in exporters {
                    if let Some(name) = exporter
                        .as_str()
                        .filter(|s| *s == "prometheus" || s.starts_with("prometheus/"))
                    {
                        names.push(name);
                    }
                }
            }
        }
    }
    let mut urls = Vec::new();
    for name in names {
        let exporter = &root["exporters"][name];
        if !exporter["tls"].is_null() {
            continue;
        }
        if let Some(endpoint) = exporter["endpoint"].as_str() {
            let url = format!("http://{endpoint}/metrics");
            if loopback_target(&url).is_ok() {
                urls.push(url);
            }
        }
    }
    urls.sort();
    urls.dedup();
    if urls.len() != 1 {
        return Err("Expected one unambiguous literal loopback Prometheus exporter in an active metrics pipeline".into());
    }
    Ok(urls.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prom_parser_preserves_signing_counts_but_not_unknown_secrets() {
        let text="cometbft_privval_signing_latency_seconds_count{chain_id=\"mocha-5\",message_type=\"raw_bytes\",password=\"secret\",instance=\"127.0.0.1:99\"} 51\ncometbft_privval_signing_latency_median_seconds{chain_id=\"mocha-5\"} 0.003\nprivate_key 123\n";
        let m = parse_metrics(text, "test", true);
        assert_eq!(m.samples.len(), 2);
        assert!(!m.freshness_verified);
        let out = serde_json::to_string(&m).unwrap();
        assert!(!out.contains("secret"));
        assert!(!out.contains("127.0.0.1:99"));
    }
    #[test]
    fn endpoints_do_not_allow_proxy_or_hostname() {
        for bad in [
            "http://localhost:99/metrics",
            "http://127.0.0.1:99/metrics?token=secret",
            "http://user@127.0.0.1:99/metrics",
            "http://8.8.8.8:80/",
        ] {
            assert!(loopback_target(bad).is_err());
        }
        assert!(loopback_target("http://[::1]:99/metrics").is_ok());
    }
    #[test]
    fn otel_discovery_requires_pipeline_and_does_not_use_internal_port() {
        let yaml="exporters:\n  prometheus:\n    endpoint: 127.0.0.1:8889\nservice:\n  telemetry:\n    metrics:\n      address: 127.0.0.1:8888\n  pipelines:\n    metrics:\n      exporters: [prometheus]\n";
        assert_eq!(
            otel_exporter_url(yaml).unwrap(),
            "http://127.0.0.1:8889/metrics"
        );
        assert!(
            otel_exporter_url("service: {telemetry: {metrics: {address: '127.0.0.1:8888'}}}")
                .is_err()
        );
    }
    #[test]
    fn otel_receiver_and_exporter_must_share_metrics_pipeline() {
        let yaml="receivers:\n  otlp:\n    protocols:\n      http:\n        endpoint: 0.0.0.0:4318\nexporters:\n  prometheus:\n    endpoint: 127.0.0.1:8889\nservice:\n  pipelines:\n    metrics:\n      receivers: [otlp]\n      exporters: [prometheus]\n";
        assert!(otel_exporter_for_receiver(yaml, Some("127.0.0.1:4318".parse().unwrap())).is_ok());
        assert!(otel_exporter_for_receiver(yaml, Some("127.0.0.1:4317".parse().unwrap())).is_err());
        assert!(otel_exporter_for_receiver(
            &yaml.replace("receivers: [otlp]", "receivers: [other]"),
            Some("127.0.0.1:4318".parse().unwrap())
        )
        .is_err());
    }
    #[test]
    fn pinned_fibre_occupancy_budget_and_prune_are_retained() {
        let text="fibre_server_upload_shard_occupancy_bytes 123\nfibre_server_upload_shard_budget_bytes 999\nfibre_server_prune_entries_total 10\nfibre_server_upload_shard_last_success_timestamp_seconds 1700000000\n";
        assert_eq!(parse_metrics(text, "test", false).samples.len(), 4);
    }
    #[test]
    fn integer_counters_preserve_bits_beyond_f64_precision() {
        let m = parse_metrics(
            "fibre_server_prune_entries_total 9007199254740993\n",
            "test",
            false,
        );
        assert_eq!(m.samples[0].exact_integer, Some(9007199254740993));
    }
}
