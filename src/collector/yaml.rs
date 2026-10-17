//! Literal OTel YAML with input and parser budgets; no includes or interpolation.
use serde_json::Value;

const MAX_NODES: usize = 4096;
const MAX_STRING_BYTES: usize = 131072;
const MAX_DEPTH: usize = 32;

pub(super) fn parse(text: &str) -> Result<Value, String> {
    if text.len() > 65536 {
        return Err("OTel configuration size limit exceeded".into());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_nodes: MAX_NODES,
            max_events: 8192,
            max_depth: MAX_DEPTH,
            max_total_scalar_bytes: MAX_STRING_BYTES,
            max_documents: 1,
            max_aliases: 0,
            max_anchors: 0,
            max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        reject_unsupported_tags: true,
    };
    // Errors may include snippets of private configuration; never propagate them.
    serde_saphyr::from_str_with_options(text, options)
        .map_err(|_| "Unsupported or over-budget OTel YAML".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_expansion_is_bounded_before_building_large_tree() {
        let scalar = "x".repeat(8192);
        let aliases = vec!["*a"; 32].join(",");
        let nested = vec!["*b"; 32].join(",");
        let yaml = format!("a: &a {scalar}\nb: &b [{aliases}]\nc: [{nested}]\n");
        assert!(yaml.len() < 65536);
        assert!(parse(&yaml).is_err());
    }

    #[test]
    fn structure_depth_tags_duplicates_and_multiple_documents_are_rejected() {
        assert!(parse(&format!("{}0{}", "[".repeat(34), "]".repeat(34))).is_err());
        assert!(parse(&format!("[{}]", vec!["0"; MAX_NODES + 1].join(","))).is_err());
        for text in ["a: !include secret.yml", "a: 1\na: 2", "a: 1\n---\nb: 2"] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn bounded_normal_configuration_is_preserved() {
        let root = parse("exporters:\n  prometheus:\n    endpoint: 127.0.0.1:8889\nservice:\n  pipelines:\n    metrics:\n      exporters: [prometheus]\n").unwrap();
        assert_eq!(
            root["exporters"]["prometheus"]["endpoint"].as_str(),
            Some("127.0.0.1:8889")
        );
    }
}
