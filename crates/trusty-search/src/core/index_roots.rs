//! Multi-root index primitive: the root table and the corpus-path encoding
//! that lets one index span several directory trees (#7434).
//!
//! Why: `IndexHandle` carried exactly one `root_path`, so an assistant that
//! needs one index over an OKG tree plus one tree per project had to create one
//! index per tree and fan out at query time (#7429, epic #7425). Widening the
//! walk alone is not enough: every file the reindex stores is written to the
//! corpus RELATIVE to the index root (#402, so a moved tree keeps its index),
//! and a naive multi-root walk stores additional-root files ABSOLUTE — the
//! `strip_prefix(root).unwrap_or(path)` fallback — which silently gives up
//! relocation resilience for exactly the files the feature added.
//!
//! What: [`IndexRoots`], an ordered table whose slot 0 is the PRIMARY root and
//! whose remaining slots are the additional roots, plus the encode/decode pair
//! that maps an absolute path to its corpus-relative string and back. The
//! encoding is:
//!
//! | Root | Stored form |
//! |---|---|
//! | primary (slot 0) | `<rel>` — bare, byte-identical to the single-root form |
//! | additional slot `n` (1-based) | `@root<n>/<rel>` |
//!
//! The primary root's form is unchanged on purpose: every existing corpus was
//! written that way, so a single-root index round-trips through this module
//! without a migration and `additional_roots` deserialises as an empty list.
//!
//! The `@root<n>/` sentinel is ambiguous against a real top-level directory
//! literally named `@root1`. Decoding is bounded — the ordinal must parse and
//! must be within `additional.len()` — so the ambiguity needs a primary root
//! that genuinely contains such a directory AND an index that has at least
//! that many additional roots; the cost is one wrongly-resolved display path,
//! never a corrupt corpus. No cheaper escape exists: on Unix every byte except
//! `/` and NUL is a legal component name, so no prefix is collision-proof.
//!
//! "Index roots" is this concept's name everywhere. It is UNRELATED to
//! [`crate::service::roots_registry`] ("tracked roots"), which records the
//! directories the startup scanner should look at — a discovery surface, not a
//! per-index corpus surface.
//!
//! Test: `mod tests` below covers encode, decode, round-trip, longest-match
//! nesting, containment, and the back-compat guarantee for a single-root table.

use std::path::{Path, PathBuf};

/// Sentinel that prefixes the corpus path of every file under an ADDITIONAL
/// index root. Followed immediately by the 1-based ordinal and a `/`.
///
/// Why: named once so the encoder, the decoder, and the tests cannot drift.
const ADDITIONAL_ROOT_PREFIX: &str = "@root";

/// Encode `path` relative to whichever of `primary` / `additional` contains it.
///
/// Why: the batch loop and the prune pass must produce IDENTICAL strings for
/// the same file or the prune's set-difference falsely classifies live files as
/// deleted and wipes them from the staging corpus (#848). One canonical
/// function called from both sites is what keeps that true once more than one
/// root is in play.
/// What: picks the LONGEST root that is a prefix of `path` (so a nested
/// additional root wins over the primary that contains it, keeping the stored
/// path short and the decode unique), with the primary winning an exact tie.
/// A primary-root file is stored bare; an additional-root file is stored as
/// `@root<n>/<rel>`. A path under NO root falls back to its absolute form —
/// the same fallback the single-root code had, so a symlink whose target
/// escapes every root still produces matching strings on both sides.
/// Test: `encodes_primary_root_files_bare`, `encodes_additional_root_files`,
/// `nested_additional_root_wins_over_primary`, `path_outside_every_root_stays_absolute`.
pub fn relative_path(primary: &Path, additional: &[PathBuf], path: &Path) -> String {
    let mut best: Option<(usize, Option<usize>, &Path)> = None;
    let mut consider = |root: &Path, slot: Option<usize>| {
        if let Ok(rel) = path.strip_prefix(root) {
            let depth = root.components().count();
            let better = match &best {
                None => true,
                Some((best_depth, _, _)) => depth > *best_depth,
            };
            if better {
                best = Some((depth, slot, rel));
            }
        }
    };
    consider(primary, None);
    for (i, root) in additional.iter().enumerate() {
        consider(root, Some(i));
    }

    match best {
        None => path.display().to_string(),
        Some((_, slot, rel)) => stored_path_for_slot(slot, &rel.display().to_string()),
    }
}

/// Apply the `@root<n>/` sentinel to a root-relative path for `slot`.
///
/// Why: the reindex walk is no longer the only producer of corpus paths — the
/// file watcher writes them too, once per watched root (#7434). Two encoders
/// would be two places for the sentinel's spelling to drift, and the #848
/// prune property depends on every producer agreeing byte-for-byte. This is
/// the one place the sentinel is written.
/// What: `None` (the primary root) returns `rel` unchanged, which is what keeps
/// a single-root corpus byte-identical to its pre-#7434 form. `Some(n)` returns
/// `@root<n+1>/<rel>`. An ALREADY-ABSOLUTE `rel` is returned unchanged for
/// every slot: that string is the out-of-root fallback both producers share,
/// and prefixing it would make it undecodable.
/// Test: `stored_path_for_slot_matches_relative_path`,
/// `stored_path_for_slot_leaves_an_absolute_fallback_alone`.
pub fn stored_path_for_slot(slot: Option<usize>, rel: &str) -> String {
    match slot {
        None => rel.to_string(),
        Some(_) if Path::new(rel).is_absolute() => rel.to_string(),
        Some(n) => format!("{ADDITIONAL_ROOT_PREFIX}{}/{rel}", n + 1),
    }
}

/// Decode a stored corpus path back to an absolute path.
///
/// Why: search results, the KG, and `list_chunks` all hand the caller a real
/// on-disk path, and joining an additional root's stored path against the
/// PRIMARY root produces a path that does not exist — a result the caller
/// cannot open. This is the only place that reverses [`relative_path`].
/// What: an already-absolute `stored` passes through (legacy pre-#402 chunks).
/// Otherwise, a leading `@root<n>/` whose ordinal is within `additional` joins
/// against `additional[n - 1]`; everything else joins against `primary`, which
/// is exactly the pre-#7434 behaviour for every single-root index.
/// Test: `round_trips_every_root`, `decodes_unknown_ordinal_against_primary`.
pub fn resolve_absolute(primary: &Path, additional: &[PathBuf], stored: &str) -> PathBuf {
    if Path::new(stored).is_absolute() {
        return PathBuf::from(stored);
    }
    if let Some((head, rest)) = stored.split_once('/') {
        if let Some(slot) = additional_root_slot(head, additional.len()) {
            return additional[slot].join(rest);
        }
    }
    primary.join(stored)
}

/// Parse an `@root<n>` path component into a 0-based slot in `additional`.
///
/// Why: the bound (`n >= 1 && n <= len`) is what keeps a real directory named
/// `@root9` in a two-root index from being decoded as a root reference.
/// What: `Some(n - 1)` for a well-formed, in-range ordinal; `None` otherwise.
/// Test: `decodes_unknown_ordinal_against_primary`.
fn additional_root_slot(component: &str, len: usize) -> Option<usize> {
    let ordinal: usize = component
        .strip_prefix(ADDITIONAL_ROOT_PREFIX)?
        .parse()
        .ok()?;
    (ordinal >= 1 && ordinal <= len).then_some(ordinal - 1)
}

/// `true` when `path` lies under the primary root or any additional root.
///
/// Why: the search post-filter (`file_is_within_root`) drops any result whose
/// stored path is not inside the index's root, which is the guard against
/// cross-index bleed (#64). With additional roots that guard has to ask
/// any-of-N or it silently drops every additional-root hit.
/// What: a purely lexical prefix test over the whole table — no syscalls, so
/// callers that need symlink-alias tolerance keep their own canonicalize
/// fallback on top.
/// Test: `containment_is_any_of_n`.
pub fn is_within_any(primary: &Path, additional: &[PathBuf], path: &Path) -> bool {
    path.starts_with(primary) || additional.iter().any(|r| path.starts_with(r))
}

/// One index's ordered root table: the primary root plus its additional roots.
///
/// Why: the reindex pipeline threads the root set through several structs
/// (`BatchCtx`, `FinishCtx`) and a bare pair of arguments kept growing new
/// call sites that could pass them in the wrong order. Owning them together
/// also makes "primary is slot 0" a property of the type rather than a comment.
/// What: a cheap, cloneable value built once per reindex. Every method
/// delegates to the free functions above so the hot per-chunk resolution path
/// can keep borrowing `(&Path, &[PathBuf])` without constructing one of these.
/// Test: `mod tests` below.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexRoots {
    primary: PathBuf,
    additional: Vec<PathBuf>,
}

impl IndexRoots {
    /// Build a table from a primary root and its additional roots.
    pub fn new(primary: PathBuf, additional: Vec<PathBuf>) -> Self {
        Self {
            primary,
            additional,
        }
    }

    /// The primary root — the index's identity anchor.
    ///
    /// Why: index-id derivation, colocated storage, and the #402/#2178 hijack
    /// gate are defined against this root ALONE and must never see an
    /// additional one.
    pub fn primary(&self) -> &Path {
        &self.primary
    }

    /// The additional roots, in table order.
    pub fn additional(&self) -> &[PathBuf] {
        &self.additional
    }

    /// Every root, primary first. Allocates; use for walking, not per chunk.
    pub fn all(&self) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(1 + self.additional.len());
        out.push(self.primary.clone());
        out.extend(self.additional.iter().cloned());
        out
    }

    /// The root that contains `path`, by the same longest-match rule
    /// [`relative_path`] encodes with.
    ///
    /// Why: filters defined against "the index root" — `path_filter`'s
    /// immediate-subdirectory globs (#111) — have to be evaluated against the
    /// root the file actually came from, or every additional-root file fails
    /// the `strip_prefix` inside them and is dropped from the walk.
    /// What: `None` when `path` is under no root, which callers treat as
    /// "not ours".
    /// Test: `owning_root_uses_longest_match`.
    pub fn owning_root(&self, path: &Path) -> Option<&Path> {
        let mut best: Option<(usize, &Path)> = None;
        for root in std::iter::once(&self.primary).chain(self.additional.iter()) {
            if path.starts_with(root) {
                let depth = root.components().count();
                if best.is_none_or(|(d, _)| depth > d) {
                    best = Some((depth, root.as_path()));
                }
            }
        }
        best.map(|(_, r)| r)
    }

    /// See [`relative_path`].
    pub fn relative_path(&self, path: &Path) -> String {
        relative_path(&self.primary, &self.additional, path)
    }

    /// See [`resolve_absolute`].
    pub fn resolve_absolute(&self, stored: &str) -> PathBuf {
        resolve_absolute(&self.primary, &self.additional, stored)
    }

    /// See [`is_within_any`].
    pub fn is_within_any(&self, path: &Path) -> bool {
        is_within_any(&self.primary, &self.additional, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> IndexRoots {
        IndexRoots::new(
            PathBuf::from("/a/primary"),
            vec![PathBuf::from("/b/extra"), PathBuf::from("/c/third")],
        )
    }

    /// Why: the primary root's stored form is what every pre-#7434 corpus
    /// already holds; changing it would invalidate every existing index.
    #[test]
    fn encodes_primary_root_files_bare() {
        assert_eq!(
            roots().relative_path(Path::new("/a/primary/src/lib.rs")),
            "src/lib.rs"
        );
    }

    /// Why: the whole point of #7434 — an additional-root file must be stored
    /// relative to ITS OWN root, not absolute (the naive-walk failure) and not
    /// relative to the primary (which would resolve to a path that does not
    /// exist).
    #[test]
    fn encodes_additional_root_files() {
        let r = roots();
        assert_eq!(
            r.relative_path(Path::new("/b/extra/main.rs")),
            "@root1/main.rs"
        );
        assert_eq!(
            r.relative_path(Path::new("/c/third/x/y.md")),
            "@root2/x/y.md"
        );
    }

    /// Why: an additional root nested inside the primary is a legitimate
    /// configuration; longest-match keeps the encoding unique so decode is
    /// deterministic.
    #[test]
    fn nested_additional_root_wins_over_primary() {
        let r = IndexRoots::new(PathBuf::from("/a"), vec![PathBuf::from("/a/vendor/dep")]);
        assert_eq!(
            r.relative_path(Path::new("/a/vendor/dep/f.rs")),
            "@root1/f.rs"
        );
        assert_eq!(r.relative_path(Path::new("/a/src/f.rs")), "src/f.rs");
    }

    /// Why: the single-root code returned the absolute path when
    /// `strip_prefix` failed and the batch loop and prune pass agreed on it;
    /// preserving that fallback keeps the set-difference sound.
    #[test]
    fn path_outside_every_root_stays_absolute() {
        assert_eq!(
            roots().relative_path(Path::new("/elsewhere/f.rs")),
            "/elsewhere/f.rs"
        );
    }

    /// Why: a stored path that cannot be turned back into the file it came
    /// from is unusable to a search client.
    #[test]
    fn round_trips_every_root() {
        let r = roots();
        for abs in [
            "/a/primary/src/lib.rs",
            "/b/extra/main.rs",
            "/c/third/x/y.md",
        ] {
            let stored = r.relative_path(Path::new(abs));
            assert_eq!(r.resolve_absolute(&stored), PathBuf::from(abs), "{stored}");
        }
    }

    /// Why: an out-of-range or malformed ordinal must not be read as a root
    /// reference — it is a real directory name and belongs under the primary.
    #[test]
    fn decodes_unknown_ordinal_against_primary() {
        let r = roots();
        assert_eq!(
            r.resolve_absolute("@root9/f.rs"),
            PathBuf::from("/a/primary/@root9/f.rs")
        );
        assert_eq!(
            r.resolve_absolute("@rootx/f.rs"),
            PathBuf::from("/a/primary/@rootx/f.rs")
        );
    }

    /// Why: a single-root table must behave exactly as the pre-#7434 code did,
    /// which is what makes the change a no-migration one.
    #[test]
    fn single_root_table_is_back_compatible() {
        let r = IndexRoots::new(PathBuf::from("/a/primary"), Vec::new());
        assert_eq!(r.relative_path(Path::new("/a/primary/s.rs")), "s.rs");
        assert_eq!(
            r.resolve_absolute("@root1/s.rs"),
            PathBuf::from("/a/primary/@root1/s.rs"),
            "with no additional roots the sentinel is just a directory name"
        );
    }

    /// Why: an absolute stored path is a legacy pre-#402 chunk and must pass
    /// through untouched.
    #[test]
    fn absolute_stored_path_passes_through() {
        assert_eq!(
            roots().resolve_absolute("/somewhere/f.rs"),
            PathBuf::from("/somewhere/f.rs")
        );
    }

    /// Why: the search post-filter must accept a hit from any root, or every
    /// additional-root result is dropped before the caller sees it.
    #[test]
    fn containment_is_any_of_n() {
        let r = roots();
        assert!(r.is_within_any(Path::new("/a/primary/f.rs")));
        assert!(r.is_within_any(Path::new("/b/extra/f.rs")));
        assert!(r.is_within_any(Path::new("/c/third/f.rs")));
        assert!(!r.is_within_any(Path::new("/d/other/f.rs")));
    }

    /// Why: a per-root filter evaluated against the wrong root drops every
    /// file under the other roots.
    #[test]
    fn owning_root_uses_longest_match() {
        let r = IndexRoots::new(PathBuf::from("/a"), vec![PathBuf::from("/a/vendor")]);
        assert_eq!(
            r.owning_root(Path::new("/a/src/f.rs")),
            Some(Path::new("/a"))
        );
        assert_eq!(
            r.owning_root(Path::new("/a/vendor/f.rs")),
            Some(Path::new("/a/vendor"))
        );
        assert_eq!(r.owning_root(Path::new("/z/f.rs")), None);
    }

    /// Why: `all()` is what the walker iterates, and the primary must lead so
    /// a caller that takes `all()[0]` gets the identity anchor.
    #[test]
    fn all_lists_primary_first() {
        assert_eq!(
            roots().all(),
            vec![
                PathBuf::from("/a/primary"),
                PathBuf::from("/b/extra"),
                PathBuf::from("/c/third"),
            ]
        );
    }

    /// Why: the file watcher encodes its corpus keys through
    /// [`stored_path_for_slot`] while the reindex walk reaches it through
    /// [`relative_path`]. The #848 prune property needs the two to produce the
    /// same string for the same file, byte for byte.
    /// Test: this test (#7434).
    #[test]
    fn stored_path_for_slot_matches_relative_path() {
        let table = roots();
        assert_eq!(
            stored_path_for_slot(None, "src/lib.rs"),
            table.relative_path(Path::new("/a/primary/src/lib.rs"))
        );
        assert_eq!(
            stored_path_for_slot(Some(0), "src/lib.rs"),
            table.relative_path(Path::new("/b/extra/src/lib.rs"))
        );
        assert_eq!(
            stored_path_for_slot(Some(1), "x.rs"),
            table.relative_path(Path::new("/c/third/x.rs"))
        );
    }

    /// Why: a path under no root falls back to its absolute form on both the
    /// walk and the watcher side. Prefixing that with a slot would produce a
    /// string neither [`resolve_absolute`] nor any consumer can decode.
    /// Test: this test (#7434).
    #[test]
    fn stored_path_for_slot_leaves_an_absolute_fallback_alone() {
        assert_eq!(
            stored_path_for_slot(Some(0), "/elsewhere/stray.rs"),
            "/elsewhere/stray.rs"
        );
        assert_eq!(
            stored_path_for_slot(None, "/elsewhere/stray.rs"),
            "/elsewhere/stray.rs"
        );
    }
}
