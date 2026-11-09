//! The document-tree ID rule (SPEC-OKF-01 §2.14 rule 3).
//!
//! Why: an import must map the same source path to the same concept on
//! every machine, keep the source hierarchy, and never let `a.md` and
//! `a.pdf` meet at one ID. A path that is already a valid ID maps to
//! itself, so ordinary trees import unchanged.
//! What: [`concept_id_for_source`] drops a `.md` extension (any case) and
//! keeps every other file name whole. Each segment §2.11 refuses is put in
//! NFC, its refused characters (`:`, `\`, control characters) and a leading
//! `_` or `.` of the first segment become `-`, and it gains the suffix
//! `~<first 8 hex of SHA-256 of the full source-relative path>`. Slice S8 of
//! #10746.
//! Test: `c73_ids_mirror_the_source_hierarchy`,
//! `c74_refused_names_normalize_with_a_stable_suffix`.

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::OkfError;
use crate::format::id::ConceptId;

/// The concept ID of a source-relative path (`notes/a.md`, `a.pdf`).
///
/// Errors: `InvalidId` when even the normalized form breaks §2.11; the item
/// then fails and nothing is written.
pub fn concept_id_for_source(rel: &str) -> Result<ConceptId, OkfError> {
    let stem = match rel.len().checked_sub(3).and_then(|i| rel.get(i..)) {
        Some(tail) if tail.eq_ignore_ascii_case(".md") => &rel[..rel.len() - 3],
        _ => rel,
    };
    if let Ok(id) = ConceptId::parse(stem) {
        return Ok(id);
    }
    let h8: String = Sha256::digest(rel.as_bytes())[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let segments: Vec<&str> = stem.split('/').collect();
    let last = segments.len().saturating_sub(1);
    let fixed: Vec<String> = segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            if refused(seg, i, last) {
                format!("{}~{h8}", normalize(seg, i))
            } else {
                seg.to_string()
            }
        })
        .collect();
    ConceptId::parse(&fixed.join("/"))
}

/// Whether §2.11 refuses `seg` at position `i` of a path whose last index
/// is `last`.
fn refused(seg: &str, i: usize, last: usize) -> bool {
    let tail_md = seg
        .len()
        .checked_sub(3)
        .and_then(|n| seg.get(n..))
        .is_some_and(|t| t.eq_ignore_ascii_case(".md"));
    seg.is_empty()
        || seg == "."
        || seg == ".."
        || seg.contains([':', '\\'])
        || seg.chars().any(char::is_control)
        || (i == 0 && (seg.starts_with('_') || seg.starts_with('.')))
        || (i == last
            && (seg.eq_ignore_ascii_case("index") || seg.eq_ignore_ascii_case("log") || tail_md))
}

/// NFC, refused characters to `-`, and a leading `_` or `.` of the first
/// segment to `-`.
fn normalize(seg: &str, i: usize) -> String {
    let mut out: String = seg
        .nfc()
        .map(|c| {
            if c == ':' || c == '\\' || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    if i == 0 && (out.starts_with('_') || out.starts_with('.')) {
        out.replace_range(..1, "-");
    }
    if out.is_empty() || out.chars().all(|c| c == '.') {
        out = "-".repeat(out.len().max(1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: §2.14 rule 3 — the identity for ordinary paths; `.md` dropped,
    /// any other name kept whole; refused segments suffixed.
    /// What: a table of source paths and expected IDs.
    /// Test: self-contained.
    #[test]
    fn id_rule_table() {
        let id = |s: &str| concept_id_for_source(s).unwrap().as_str().to_string();
        assert_eq!(id("notes/a.md"), "notes/a");
        assert_eq!(id("a.pdf"), "a.pdf");
        assert_eq!(id("A.MD"), "A");
        assert_eq!(id("a/_b/c.md"), "a/_b/c");
        let h = |s: &str| -> String {
            Sha256::digest(s.as_bytes())[..4]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        assert_eq!(id("index.md"), format!("index~{}", h("index.md")));
        assert_eq!(id("C:x.md"), format!("C-x~{}", h("C:x.md")));
        assert_eq!(id("_x/y.md"), format!("-x~{}/y", h("_x/y.md")));
        assert_eq!(id("n/LOG.md"), format!("n/LOG~{}", h("n/LOG.md")));
        assert_eq!(id("n/x.md.md"), format!("n/x.md~{}", h("n/x.md.md")));
        assert_eq!(id("a\\b.txt"), format!("a-b.txt~{}", h("a\\b.txt")));
    }
}
