//! The set of stored file paths one batch removal names (#10110).
//!
//! Why: removing an excluded repository from a large index went one file at a
//! time, and each call rehydrated the whole evicted corpus. A batch removal
//! names its files once, by prefix or by list, and every store — redb rows,
//! entity rows, content hashes, vectors — must agree on which keys that is.
//! What: [`PathSelection`] holds stored keys (already mapped from absolute
//! paths by the caller) and answers "is this file / chunk id / key selected"
//! with the segment-boundary rules of `core::store::path_match`, so
//! `src/foo` never selects `src/foobar/`.
//! Test: `prefix_selects_its_own_segment_only`, `files_select_exact_paths_and_their_chunk_ids`.

use std::collections::BTreeSet;

use crate::core::chunk_id::{is_valid_suffix, ChunkIdShapes};
use crate::core::store::path_match::matches_file;

/// Stored file keys one batch removal selects (#10110).
///
/// What: `Prefix` selects every file under each key at a path-segment
/// boundary; `Files` selects each key exactly. Both are non-empty: the
/// request layer refuses an empty selection, an empty key, and the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathSelection {
    /// Every file under one of these stored prefixes.
    Prefix(BTreeSet<String>),
    /// Exactly these stored file keys.
    Files(BTreeSet<String>),
}

impl PathSelection {
    /// The stored keys a range scan starts from. Every selected key begins
    /// with one of them, which is what lets redb answer by range, not by scan.
    pub(crate) fn scan_starts(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::Prefix(keys) | Self::Files(keys) => keys.iter().map(String::as_str),
        }
    }

    /// True when the stored file key `file` is selected.
    pub(crate) fn selects_file(&self, file: &str) -> bool {
        match self {
            Self::Prefix(prefixes) => prefixes.iter().any(|p| matches_file(file, Some(p), &[])),
            Self::Files(files) => files.contains(file),
        }
    }

    /// True when `id` is a chunk id of a selected file.
    ///
    /// What: a chunk id is `{file}{suffix}`; for `Files` each `:` is tried as
    /// the split point, so a file key holding `:` still matches its own ids.
    pub(crate) fn selects_chunk_id(&self, id: &str, shapes: ChunkIdShapes) -> bool {
        match self {
            Self::Prefix(prefixes) => prefixes.iter().any(|p| {
                crate::core::store::path_match::matches_chunk_id(id, Some(p), &[], shapes)
            }),
            Self::Files(files) => id
                .match_indices(':')
                .any(|(at, _)| files.contains(&id[..at]) && is_valid_suffix(&id[at..], shapes)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(p: &str) -> PathSelection {
        PathSelection::Prefix(BTreeSet::from([p.to_string()]))
    }

    /// `src/foo` selects its own subtree and never the sibling `src/foobar/`.
    #[test]
    fn prefix_selects_its_own_segment_only() {
        let sel = prefix("src/foo");
        assert!(sel.selects_file("src/foo/a.rs"));
        assert!(sel.selects_file("src/foo"));
        assert!(!sel.selects_file("src/foobar/a.rs"));
        assert!(!sel.selects_file("src/foo:evil/a.rs"));
        assert!(sel.selects_chunk_id("src/foo/a.rs:1:9", ChunkIdShapes::NewOnly));
        assert!(!sel.selects_chunk_id("src/foobar/a.rs:1:9", ChunkIdShapes::NewOnly));
    }

    /// A file key selects itself and its chunk ids, not a longer sibling key.
    #[test]
    fn files_select_exact_paths_and_their_chunk_ids() {
        let sel = PathSelection::Files(BTreeSet::from(["src/a.rs".to_string()]));
        assert!(sel.selects_file("src/a.rs"));
        assert!(!sel.selects_file("src/a.rs.bak"));
        assert!(sel.selects_chunk_id("src/a.rs:3:7", ChunkIdShapes::NewOnly));
        assert!(sel.selects_chunk_id("src/a.rs::Function::main::3::7", ChunkIdShapes::NewOnly));
        assert!(!sel.selects_chunk_id("src/a.rs.bak:3:7", ChunkIdShapes::NewOnly));
    }
}
