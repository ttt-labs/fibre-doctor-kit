use crate::model::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) fn hash(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
pub(super) fn properties(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(k, v)| k.len() < 128 && v.len() <= 8192)
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}
pub(super) fn colon_fields(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().into(), v.trim().into()))
        .collect()
}
pub(super) fn counters(text: &str) -> BTreeMap<String, u64> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            Some((p.next()?.into(), p.next()?.parse().ok()?))
        })
        .collect()
}
pub(super) fn kib(text: Option<&String>) -> Option<u64> {
    text?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}
pub(super) fn limit(text: &str) -> Option<MemoryLimit> {
    match text.trim() {
        "max" | "infinity" | "unlimited" => Some(MemoryLimit::Unlimited),
        n => n.parse().ok().map(MemoryLimit::Finite),
    }
}
pub(super) fn starttime(stat: &str) -> Option<u64> {
    // comm may contain whitespace and ')'. The last ')' terminates this field.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}
pub(super) fn nul_arguments(data: &[u8]) -> Vec<String> {
    data.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}
pub(super) fn argument(args: &[String], name: &str) -> Option<String> {
    let mut result = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--" {
            break;
        }
        if args[i] == name {
            if let Some(v) = args.get(i + 1).filter(|v| !v.starts_with("--")) {
                result = Some(v.clone());
                i += 1;
            } else if name == "--unlimited-budget" {
                result = Some("true".into());
            }
        } else if let Some(v) = args[i].strip_prefix(&format!("{name}=")) {
            result = Some(v.into());
        }
        i += 1;
    }
    result
}
pub(super) fn safe_env(data: &[u8]) -> BTreeMap<String, String> {
    data.split(|b| *b == 0)
        .filter_map(|s| {
            let (k, v) = std::str::from_utf8(s).ok()?.split_once('=')?;
            if ["HOME", "FIBRE_HOME", "FIBRE_OTEL_ENDPOINT"].contains(&k) && v.len() <= 4096 {
                Some((k.into(), v.into()))
            } else {
                None
            }
        })
        .collect()
}
pub(super) fn fd_limits(text: &str) -> (Option<MemoryLimit>, Option<MemoryLimit>) {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Max open files") {
            let mut p = rest.split_whitespace();
            return (p.next().and_then(limit), p.next().and_then(limit));
        }
    }
    (None, None)
}
pub(super) fn cgroup_path(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .filter(|p| p.starts_with('/') && !p.split('/').any(|s| s == ".."))
        .map(str::to_string)
}
pub(super) fn fibre_config(config: &toml::Value, args: &[String]) -> BTreeMap<String, ConfigValue> {
    let mut out = BTreeMap::new();
    for key in [
        "max_connections",
        "max_concurrent_streams",
        "unlimited_budget",
        "min_upload_size",
        "upload_verify_workers",
    ] {
        // v10.4.0-mocha exposes only this field as a CLI flag. In particular,
        // --max-connections/--max-concurrent-streams are not supported flags.
        let cli = if key == "unlimited_budget" {
            argument(args, "--unlimited-budget")
        } else {
            None
        };
        let parsed = if let Some(c) = cli.as_ref() {
            if key == "unlimited_budget" {
                c.parse::<bool>().ok().map(serde_json::Value::Bool)
            } else {
                c.parse::<u64>()
                    .ok()
                    .filter(|v| *v > 0)
                    .map(serde_json::Value::from)
            }
        } else {
            config.get(key).and_then(|v| {
                if key == "unlimited_budget" {
                    v.as_bool().map(serde_json::Value::Bool)
                } else {
                    v.as_integer()
                        .filter(|v| *v > 0)
                        .map(serde_json::Value::from)
                }
            })
        };
        if let Some(value) = parsed {
            out.insert(
                format!("root.{key}"),
                ConfigValue {
                    value,
                    source: if cli.is_some() {
                        ConfigSource::ObservedRuntime
                    } else {
                        ConfigSource::ConfigFile
                    },
                    evidence: if cli.is_some() {
                        "process command line override".into()
                    } else {
                        "on-disk TOML; loaded runtime value not proven".into()
                    },
                },
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proc_stat_handles_spaces_and_parentheses() {
        let mut parts = vec!["S".to_owned()];
        parts.extend((4..=21).map(|_| "0".into()));
        parts.push("12345".into());
        assert_eq!(
            starttime(&format!("27 (foo ) bar) {}", parts.join(" "))),
            Some(12345)
        );
    }
    #[test]
    fn config_allowlist_and_override() {
        let cfg:toml::Value="max_connections=16\nunlimited_budget=false\nsecret_key='LEAK'\n[object_storage]\npassword='NO'".parse().unwrap();
        let got = fibre_config(
            &cfg,
            &["fibre".into(), "start".into(), "--max-connections=9".into()],
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got["root.max_connections"].value, 16);
        assert_eq!(got["root.max_connections"].source, ConfigSource::ConfigFile);
        assert!(!serde_json::to_string(&got).unwrap().contains("LEAK"));
    }
    #[test]
    fn proc_limits_do_not_confuse_fdsize() {
        assert_eq!(
            fd_limits("Max open files            1024                 unlimited            files"),
            (
                Some(MemoryLimit::Finite(1024)),
                Some(MemoryLimit::Unlimited)
            )
        );
        assert_eq!(cgroup_path("0::/safe/../bad"), None);
    }
    #[test]
    fn environment_does_not_retain_secrets() {
        let e = safe_env(b"HOME=/home/runner\0TOKEN=do-not-keep\0FIBRE_HOME=/data/f\0");
        assert_eq!(e.len(), 2);
        assert!(!e.contains_key("TOKEN"));
    }
}
