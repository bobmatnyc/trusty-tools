//! The tombstone detection rule trusty-search applies (SPEC-OKF-01 §4.8).
//!
//! Why: trusty-search removes a concept file from its index only when
//! trusty-common's `is_tombstone` accepts the bytes. A tombstone whose
//! frontmatter grew past 64 KiB reads as a live document, so the watcher
//! would index the deleted body again (reconciliation row 14). trusty-okf
//! must know, before the rename, whether the daemon will see the tombstone.
//! What: [`is_detectable_tombstone`] restates the rule with local constants:
//! line 1 is exactly `---`; the frontmatter, measured as trusty-common
//! measures it, stays within [`TOMBSTONE_FRONTMATTER_MAX`]; it carries a
//! top-level `source_status: deleted` and a non-empty top-level `source_id`.
//! #10746 S8: the core cannot link trusty-common (B8, owner decision D4b
//! open), so the rule is local; the `search-feed` test
//! `tombstone_rule_matches_trusty_common` pins it to the daemon's copy.
//! Test: `c141_overgrown_tombstone_lands_undetectable_and_never_withdrawn`,
//! `tombstone_rule_matches_trusty_common`.

use serde_yaml::{Mapping, Value};

/// The frontmatter bound of trusty-common's `is_tombstone`, in bytes.
pub const TOMBSTONE_FRONTMATTER_MAX: usize = 64 * 1024;

/// Whether trusty-search will treat `content` as a tombstone (§4.8).
///
/// What: true only when every condition in the module doc holds. Lines are
/// read as `str::lines` reads them, and the measured length counts each
/// line plus one newline, so the bound matches the daemon's byte for byte.
pub fn is_detectable_tombstone(content: &str) -> bool {
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return false;
    }
    let mut yaml = String::new();
    for line in lines {
        if line == "---" {
            return serde_yaml::from_str::<Mapping>(&yaml).is_ok_and(|map| {
                let text = |k: &str| map.get(Value::String(k.into())).and_then(Value::as_str);
                text("source_status") == Some("deleted")
                    && text("source_id").is_some_and(|s| !s.is_empty())
            });
        }
        if yaml.len().saturating_add(line.len()) > TOMBSTONE_FRONTMATTER_MAX {
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

    /// Why: each condition of §4.8 alone decides detection.
    /// What: a detectable tombstone, then one breach of each condition.
    /// Test: self-contained.
    #[test]
    fn each_tombstone_condition_decides() {
        let ok = "---\nsource_id: s\nsource_status: deleted\n---\nbody\n";
        assert!(is_detectable_tombstone(ok));
        assert!(!is_detectable_tombstone(&format!("\u{feff}{ok}")));
        assert!(!is_detectable_tombstone(
            "---\nsource_id: ''\nsource_status: deleted\n---\n"
        ));
        assert!(!is_detectable_tombstone("---\nsource_id: s\n---\n"));
        let big = format!(
            "---\nsource_id: s\nsource_status: deleted\nx: {}\n---\n",
            "a".repeat(TOMBSTONE_FRONTMATTER_MAX)
        );
        assert!(!is_detectable_tombstone(&big));
    }
}
