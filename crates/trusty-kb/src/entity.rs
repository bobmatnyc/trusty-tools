//! Entity model — slugify, deep-merge, and `[[wiki-link]]` extraction.
//!
//! Why: entities are markdown files with a YAML frontmatter mapping and a body.
//! The store's determinism and OKF's "never strip unknown keys" rule both live
//! or die here: [`deep_merge`] must union without loss, and slugging must be
//! stable so the same title always maps to the same filename.
//!
//! What: [`Entity`] wraps a frontmatter [`Value`] mapping + body string;
//! [`slugify`] produces a stable filesystem-safe slug; [`deep_merge`] deep-merges
//! two frontmatter mappings (recursive map merge, order-independent set union of
//! sequences, existing keys never dropped); [`wiki_links`] scans text for
//! `[[Target]]` references; [`link_values`] pulls link targets out of a
//! frontmatter field value (scalar or list).
//!
//! Test: `slugify_is_stable_and_safe`, `deep_merge_preserves_and_unions`,
//! `wiki_links_extracts_targets`, `entity_content_roundtrip`.

use serde_yaml::{Mapping, Value};

use crate::frontmatter;

/// A parsed KB entity: frontmatter mapping + markdown body.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    /// The frontmatter as a [`Value::Mapping`]. Unknown keys are retained.
    pub frontmatter: Value,
    /// The markdown body after the frontmatter fence.
    pub body: String,
}

impl Entity {
    /// An empty entity (empty mapping, empty body).
    pub fn empty() -> Self {
        Self {
            frontmatter: Value::Mapping(Mapping::new()),
            body: String::new(),
        }
    }

    /// Parse a full file's content into an [`Entity`].
    ///
    /// Why: the store reads raw file text and needs the split + parse in one
    /// step, tolerant of a frontmatter-less file.
    /// What: splits on the `---` fence, parses the YAML to a mapping, and keeps
    /// the body. A file with no frontmatter yields an empty mapping.
    /// Test: `entity_content_roundtrip`.
    pub fn from_content(content: &str) -> anyhow::Result<Self> {
        let split = frontmatter::split(content);
        let frontmatter = match split.frontmatter {
            Some(yaml) => frontmatter::parse(&yaml)?,
            None => Value::Mapping(Mapping::new()),
        };
        Ok(Self {
            frontmatter,
            body: split.body,
        })
    }

    /// Render this entity to canonical, key-sorted document text.
    ///
    /// Test: `entity_content_roundtrip`.
    pub fn to_content(&self) -> String {
        frontmatter::render(&self.frontmatter, &self.body)
    }

    /// The frontmatter mapping, borrowed mutably (creating it if somehow absent).
    pub fn map_mut(&mut self) -> &mut Mapping {
        if !matches!(self.frontmatter, Value::Mapping(_)) {
            self.frontmatter = Value::Mapping(Mapping::new());
        }
        match &mut self.frontmatter {
            Value::Mapping(m) => m,
            _ => unreachable!("just ensured a mapping"),
        }
    }

    /// A string-valued frontmatter field, if present and scalar.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.frontmatter.get(key).and_then(Value::as_str)
    }
}

/// Produce a stable, filesystem-safe slug from a display name.
///
/// Why: the filename is derived from the title, so slugging must be
/// deterministic and injective enough to avoid collisions — and must never emit
/// a path separator or `..`, which would be a traversal vector.
/// What: lowercases, replaces every run of non-`[a-z0-9]` characters with a
/// single `-`, and trims leading/trailing `-`. An empty result becomes
/// `untitled`.
/// Test: `slugify_is_stable_and_safe`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Deep-merge `overlay` into `base`, never dropping an existing key.
///
/// Why: OKF requires unknown keys to survive a merge, and the store's merge mode
/// must union arrays and recurse into nested maps rather than clobber.
/// What: for each key in `overlay`: nested mappings merge recursively; sequences
/// union (existing order preserved, new items appended if not already present by
/// value); any other value type replaces the base scalar (last-writer-wins for
/// scalars). Keys present only in `base` are always retained.
/// Test: `deep_merge_preserves_and_unions`.
pub fn deep_merge(base: &mut Value, overlay: Value) {
    let (base_map, overlay_map) = match (base, overlay) {
        (Value::Mapping(b), Value::Mapping(o)) => (b, o),
        // Non-mapping overlay replaces base wholesale (used for leaf scalars).
        (base_slot, overlay) => {
            *base_slot = overlay;
            return;
        }
    };
    for (key, over_val) in overlay_map {
        match base_map.get_mut(&key) {
            Some(existing) => merge_slot(existing, over_val),
            None => {
                base_map.insert(key, over_val);
            }
        }
    }
}

/// Merge one overlay value into an existing base slot.
fn merge_slot(existing: &mut Value, over_val: Value) {
    match (&mut *existing, over_val) {
        (Value::Mapping(_), over @ Value::Mapping(_)) => deep_merge(existing, over),
        (Value::Sequence(base_seq), Value::Sequence(over_seq)) => {
            for item in over_seq {
                if !base_seq.contains(&item) {
                    base_seq.push(item);
                }
            }
        }
        (slot, over) => *slot = over,
    }
}

/// Extract every `[[Target]]` wiki-link target from a text blob.
///
/// Why: the reconciler and the dangling-link lint both need the set of link
/// targets in a field value or body.
/// What: scans for `[[` … `]]` spans and returns the trimmed inner text of each.
/// An unterminated `[[` is ignored. A link with a `|` alias (`[[Target|Label]]`)
/// yields just the `Target` portion.
/// Test: `wiki_links_extracts_targets`.
pub fn wiki_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(end) = text[i + 2..].find("]]") {
                let inner = &text[i + 2..i + 2 + end];
                let target = inner.split('|').next().unwrap_or(inner).trim();
                if !target.is_empty() {
                    out.push(target.to_string());
                }
                i = i + 2 + end + 2;
                continue;
            } else {
                break;
            }
        }
        i += 1;
    }
    out
}

/// Extract link targets held in a frontmatter field value (scalar or sequence).
///
/// Why: relationship edges are stored either as a single quoted `"[[X]]"` scalar
/// or a list of them; the reconciler must treat both uniformly.
/// What: collects [`wiki_links`] from a scalar string, or from every scalar in a
/// sequence. Non-string values contribute nothing.
/// Test: `wiki_links_extracts_targets`.
pub fn link_values(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => wiki_links(s),
        Value::Sequence(seq) => seq
            .iter()
            .filter_map(Value::as_str)
            .flat_map(wiki_links)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: slugging must be deterministic and never emit a traversal-capable
    /// string.
    /// What: asserts casing/space/punctuation normalisation, `..`/slash
    /// stripping, and the empty fallback.
    /// Test: self-contained.
    #[test]
    fn slugify_is_stable_and_safe() {
        assert_eq!(slugify("Ada Lovelace"), "ada-lovelace");
        assert_eq!(slugify("  Multiple   Spaces! "), "multiple-spaces");
        assert_eq!(slugify("../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("a/b\\c"), "a-b-c");
        assert_eq!(slugify("***"), "untitled");
        assert_eq!(slugify("Ada Lovelace"), slugify("ada  lovelace"));
    }

    /// Why: the merge contract — never lose a key, union arrays, recurse maps —
    /// is the core of merge-mode determinism.
    /// What: merges an overlay that adds a key, extends a list (with a
    /// duplicate that must not double), and nests a map; asserts the pre-existing
    /// key survives and the union is order-stable.
    /// Test: self-contained.
    #[test]
    fn deep_merge_preserves_and_unions() {
        let mut base: Value = serde_yaml::from_str(
            "type: Person\naliases: [Ada]\nnested:\n  a: 1\ncustom_key: keepme\n",
        )
        .unwrap();
        let overlay: Value = serde_yaml::from_str(
            "aliases: [Ada, Countess]\nnested:\n  b: 2\ntitle: Ada Lovelace\n",
        )
        .unwrap();
        deep_merge(&mut base, overlay);

        // Pre-existing unknown key retained.
        assert_eq!(base.get("custom_key").unwrap().as_str(), Some("keepme"));
        // Array unioned without duplicating "Ada".
        let aliases = base.get("aliases").unwrap().as_sequence().unwrap();
        assert_eq!(aliases.len(), 2);
        // Nested map deep-merged (both a and b present).
        let nested = base.get("nested").unwrap();
        assert!(nested.get("a").is_some() && nested.get("b").is_some());
        // New key added.
        assert_eq!(base.get("title").unwrap().as_str(), Some("Ada Lovelace"));
    }

    /// Why: link extraction backs both the reconciler and the dangling lint.
    /// What: asserts targets are pulled from body text and from scalar + list
    /// field values, aliases stripped, unterminated links ignored.
    /// Test: self-contained.
    #[test]
    fn wiki_links_extracts_targets() {
        assert_eq!(
            wiki_links("see [[Ada Lovelace]] and [[Babbage|Charles]]"),
            vec!["Ada Lovelace".to_string(), "Babbage".to_string()]
        );
        assert!(wiki_links("no links here [[unterminated").is_empty());

        let scalar = Value::String("[[Analytical Engine]]".into());
        assert_eq!(link_values(&scalar), vec!["Analytical Engine".to_string()]);
        let list: Value = serde_yaml::from_str("- \"[[A]]\"\n- \"[[B]]\"\n").unwrap();
        assert_eq!(link_values(&list), vec!["A".to_string(), "B".to_string()]);
    }

    /// Why: parse→render must round-trip a real entity.
    /// What: asserts content parses to the expected fields and re-renders with
    /// sorted keys.
    /// Test: self-contained.
    #[test]
    fn entity_content_roundtrip() {
        let e = Entity::from_content("---\ntype: Person\ntitle: Ada\n---\n\nBody.\n").unwrap();
        assert_eq!(e.get_str("type"), Some("Person"));
        assert_eq!(e.body, "Body.\n");
        let rendered = e.to_content();
        assert!(rendered.starts_with("---\ntitle: Ada\ntype: Person\n---\n\nBody.\n"));
    }
}
