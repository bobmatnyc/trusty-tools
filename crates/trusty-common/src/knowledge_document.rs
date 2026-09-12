//! Shared source tombstone semantics for knowledge writers and search (#7379).
use serde_yaml::{Mapping, Value};

pub const SOURCE_STATUS: &str = "source_status";
pub const DELETED: &str = "deleted";

/// Why: retained deleted source claims must not reenter retrieval.
/// What: recognize only leading YAML provenance with a source identity and deleted status.
/// Test: `source_tombstone_is_not_a_body_keyword`.
pub fn is_tombstone(content: &str) -> bool {
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return false;
    }
    let mut yaml = String::new();
    for line in lines {
        if line == "---" {
            return serde_yaml::from_str::<Mapping>(&yaml)
                .ok()
                .is_some_and(|map| {
                    map.get(Value::String(SOURCE_STATUS.into()))
                        .and_then(Value::as_str)
                        == Some(DELETED)
                        && map
                            .get(Value::String("source_id".into()))
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.is_empty())
                });
        }
        if yaml.len().saturating_add(line.len()) > 64 * 1024 {
            return false;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_tombstone_is_not_a_body_keyword() {
        assert!(is_tombstone(
            "---\nsource_id: synthetic\nsource_status: deleted\n---\nRetained old claim"
        ));
        assert!(!is_tombstone(
            "# Guide\nsource_status: deleted\nsource_id: example"
        ));
        assert!(!is_tombstone(
            "---\nsource_status: deleted\n---\nOrdinary markdown"
        ));
    }
}
