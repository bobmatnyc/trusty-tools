//! Per-root watch identity and state for a multi-root index (#7434).
//!
//! Why: `WatcherManager` keyed one `WatcherTask` and one degraded string per
//! `IndexId`, and `spawn_watch_loop` watched exactly `handle.root_path`. An
//! index that covers several trees ([`crate::core::index_roots`]) therefore
//! got incremental indexing for its PRIMARY root only — every additional root
//! went stale between reindexes, silently, with the index still reporting a
//! live watcher. Re-keying to N watches per index needs two things this module
//! owns: a value that says WHICH root a running watch covers (so an event's
//! corpus key carries that root's `@root<n>/` prefix instead of resolving
//! against the primary), and a per-root state so one root's failure is a
//! recorded fact rather than a `warn!` line.
//!
//! What: [`WatchedRoot`] — one root's canonical/raw path pair plus its slot in
//! the index's root table — and [`RootWatchState`] / [`RootWatchReport`], the
//! three states a root's watch can be in and the shape
//! `GET /indexes/:id/status` serialises them as.
//!
//! The canonical/raw pair is carried, not collapsed, for the reason
//! [`crate::service::watch_loop::watcher_relative_path`] documents: a DELETED
//! file cannot be canonicalised, so the strip has to be attempted against both
//! forms or a macOS `/var` ↔ `/private/var` event yields an absolute key that
//! matches nothing `handle_modified` recorded.
//!
//! Test: `mod tests` below covers the slot prefixing, the out-of-root
//! fallback, and table construction ordering.

use std::path::{Path, PathBuf};

use crate::core::index_roots::{resolve_absolute, stored_path_for_slot};
use crate::service::watch_loop::watcher_relative_path;

/// One root a watcher covers, and where it sits in its index's root table.
///
/// Why: an event delivered by the watch on additional root 2 must be stored as
/// `@root2/<rel>`, and nothing in the event itself says so — the watch's own
/// identity is the only carrier. Bundling the slot with the two root spellings
/// the strip needs is what keeps every call site from re-deriving it.
/// What: `canonical` is the symlink-resolved root (what a live file's
/// canonicalised event path strips against), `raw` is the configured spelling
/// (the deleted-file fallback), and `slot` is `None` for the primary root or
/// `Some(n)` for 0-based additional slot `n`.
/// Test: `mod tests` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedRoot {
    canonical: PathBuf,
    raw: PathBuf,
    slot: Option<usize>,
}

impl WatchedRoot {
    /// Build a watched root for `root`, canonicalising as the reindex walker
    /// does (#402) and falling back to the raw path when that fails.
    ///
    /// Why: `std::fs::canonicalize` is the same resolution `spawn_watch_loop`
    /// already performed inline; doing it here means every construction site
    /// gets it rather than the one that remembered.
    /// What: `slot` is `None` for the primary root, `Some(n)` for additional
    /// slot `n`. A canonicalize failure (unmounted, permission) keeps the raw
    /// path in both fields, matching `reindex::validate`'s fallback.
    /// Test: `table_numbers_additional_roots_from_one`.
    pub fn new(root: &Path, slot: Option<usize>) -> Self {
        Self {
            canonical: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            raw: root.to_path_buf(),
            slot,
        }
    }

    /// Build a watched PRIMARY root from an already-resolved path pair.
    ///
    /// Why: the pre-#7434 `handle_modified` / `handle_removed` signatures take
    /// `(canonical_root, raw_root)` and are called directly by
    /// `tests/watcher_chunk_cap_orphans_100.rs`. Keeping those signatures and
    /// adapting here is what lets the scoped handlers be the single
    /// implementation without changing a public API an integration test uses.
    /// What: no filesystem access — both spellings are taken as given.
    /// Test: exercised by every `watch_loop` unit test that calls the legacy
    /// handlers.
    pub fn from_pair(canonical: &Path, raw: &Path) -> Self {
        Self {
            canonical: canonical.to_path_buf(),
            raw: raw.to_path_buf(),
            slot: None,
        }
    }

    /// Every root of an index, primary first, numbered as the corpus encoding
    /// numbers them.
    ///
    /// Why: the manager, the rescan and the status endpoint all need "the root
    /// table as watch scopes", and three inline loops are three chances to
    /// number an additional root from 1 instead of 0.
    /// What: slot `None` for `primary`, then `Some(0..)` in `additional` order
    /// — the same ordering [`crate::core::index_roots::IndexRoots::all`] pins.
    /// Test: `table_numbers_additional_roots_from_one`.
    pub fn table(primary: &Path, additional: &[PathBuf]) -> Vec<Self> {
        let mut out = Vec::with_capacity(1 + additional.len());
        out.push(Self::new(primary, None));
        for (n, root) in additional.iter().enumerate() {
            out.push(Self::new(root, Some(n)));
        }
        out
    }

    /// The configured root spelling — this watch's identity in the table.
    pub fn raw(&self) -> &Path {
        &self.raw
    }

    /// The symlink-resolved root.
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }

    /// `None` for the primary root, `Some(n)` for 0-based additional slot `n`.
    pub fn slot(&self) -> Option<usize> {
        self.slot
    }

    /// The corpus key for a filesystem event path under this root.
    ///
    /// Why: this is the whole reason a watch has to know its slot. Storing an
    /// additional root's file under its bare relative name would collide with a
    /// same-named file in the primary tree and would decode back to the wrong
    /// absolute path; storing it absolute would give up the #402 relocation
    /// resilience for exactly the files multi-root adds.
    /// What: [`watcher_relative_path`] against this root's two spellings, then
    /// [`stored_path_for_slot`] — the same encoder the reindex walk uses, so
    /// the watcher and the walk cannot disagree about what a file is called
    /// (the property #848's prune depends on).
    /// Test: `corpus_path_prefixes_an_additional_root`,
    /// `corpus_path_leaves_the_primary_root_bare`,
    /// `corpus_path_keeps_an_out_of_root_fallback_absolute`.
    pub fn corpus_path(&self, event_path: &Path) -> String {
        let rel = watcher_relative_path(&self.canonical, &self.raw, event_path);
        stored_path_for_slot(self.slot, &rel)
    }
}

/// Resolve a corpus key produced by [`WatchedRoot::corpus_path`] back to an
/// absolute path, using `table` as the root table.
///
/// Why: the dropped-event rescan's deletion sweep asks "does this tracked file
/// still exist on disk". Joining an additional root's key against the PRIMARY
/// root answers `false` for every one of them, so a single rescan would sweep
/// every additional root's chunks out of the corpus — the worst failure this
/// slice could ship.
/// What: delegates to [`resolve_absolute`], rebuilding the `(primary,
/// additional)` shape it takes from `table`'s slots. A table with no primary
/// entry (impossible via [`WatchedRoot::table`]) resolves against the first
/// entry, which is the closest thing to the pre-#7434 behaviour.
/// Test: `resolves_a_key_against_its_own_root`.
pub fn absolute_for_key(table: &[WatchedRoot], key: &Path) -> PathBuf {
    let primary = table
        .iter()
        .find(|r| r.slot.is_none())
        .or_else(|| table.first())
        .map(|r| r.canonical.clone())
        .unwrap_or_default();
    let slots = table
        .iter()
        .filter_map(|r| r.slot)
        .max()
        .map_or(0, |m| m + 1);
    let mut additional = vec![PathBuf::new(); slots];
    for root in table {
        if let Some(n) = root.slot {
            additional[n] = root.canonical.clone();
        }
    }
    resolve_absolute(&primary, &additional, &key.to_string_lossy())
}

/// What one root's file watch is currently doing.
///
/// Why: before #7434 a spawn failure was a `warn!` and a return — the index
/// then reported `watcher.active: true` on the strength of its OTHER roots
/// while one tree silently stopped updating. A recorded state per root is what
/// makes that answerable from `GET /indexes/:id/status`.
/// What: exactly three outcomes. `Degraded` is the #3408 network-mount refusal
/// (a deliberate decision not to watch); `Failed` is a `spawn_watch_loop` error
/// (the root vanished, an inotify limit). Both carry the operator-facing
/// reason.
/// Test: `watcher_manager::tests::one_root_spawn_failure_leaves_other_roots_watched_and_is_reported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootWatchState {
    /// A live watch is installed on this root.
    Watching,
    /// Deliberately not watched — the #3408 network-mount refusal.
    Degraded {
        /// The actionable message naming the supported alternative.
        reason: String,
    },
    /// `spawn_watch_loop` returned an error for this root.
    Failed {
        /// The spawn error, rendered.
        reason: String,
    },
}

impl RootWatchState {
    /// The stable string `GET /indexes/:id/status` reports.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::Degraded { .. } => "degraded",
            Self::Failed { .. } => "failed",
        }
    }

    /// The operator-facing reason, absent for a healthy watch.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Watching => None,
            Self::Degraded { reason } | Self::Failed { reason } => Some(reason),
        }
    }

    /// `true` only for a live watch.
    pub fn is_watching(&self) -> bool {
        matches!(self, Self::Watching)
    }

    /// `true` only for the #3408 network-mount refusal.
    pub fn is_network_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }
}

/// One row of `GET /indexes/:id/status`'s `watcher.roots` array (#7434).
///
/// Why: the pre-#7434 `watcher` object answered `active` / `degraded_reason`
/// for the index as a whole, which cannot express "two of three roots are
/// watched". Those fields stay and keep their meaning; this array is additive.
/// What: a serialisable projection of [`RootWatchState`] for one root.
/// Test: `server::tests_7434_watch::status_reports_every_root_watch_state`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RootWatchReport {
    /// The configured root path.
    pub root: String,
    /// `null` for the primary root; the 0-based additional slot otherwise.
    pub slot: Option<usize>,
    /// `true` for the index's primary root.
    pub primary: bool,
    /// `watching` | `degraded` | `failed`.
    pub state: &'static str,
    /// Absent when `state` is `watching`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the primary root's key must stay byte-identical to the pre-#7434
    /// form — every existing corpus holds that spelling.
    /// Test: this test.
    #[test]
    fn corpus_path_leaves_the_primary_root_bare() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let file = root.join("lib.rs");
        std::fs::write(&file, "").expect("write");

        let watched = WatchedRoot::new(&root, None);
        assert_eq!(watched.corpus_path(&file), "lib.rs");
    }

    /// Why: this is the behaviour the whole slice exists for — a save under an
    /// additional root must land on that root's `@root<n>/` chunks, not on a
    /// bare key the decoder would resolve against the primary tree.
    /// Test: this test.
    #[test]
    fn corpus_path_prefixes_an_additional_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        let file = root.join("src/main.rs");
        std::fs::write(&file, "").expect("write");

        assert_eq!(
            WatchedRoot::new(&root, Some(0)).corpus_path(&file),
            "@root1/src/main.rs"
        );
        assert_eq!(
            WatchedRoot::new(&root, Some(2)).corpus_path(&file),
            "@root3/src/main.rs"
        );
    }

    /// Why: a symlink whose target escapes every root already produced an
    /// ABSOLUTE key on both the walk and the watcher side. Prefixing that would
    /// make it undecodable, so the fallback has to survive the slot.
    /// Test: this test.
    #[test]
    fn corpus_path_keeps_an_out_of_root_fallback_absolute() {
        let inside = tempfile::tempdir().expect("tempdir a");
        let outside = tempfile::tempdir().expect("tempdir b");
        let root = std::fs::canonicalize(inside.path()).expect("canonicalize");
        let stray = std::fs::canonicalize(outside.path())
            .expect("canonicalize")
            .join("stray.rs");
        std::fs::write(&stray, "").expect("write");

        let key = WatchedRoot::new(&root, Some(0)).corpus_path(&stray);
        assert!(
            Path::new(&key).is_absolute(),
            "an out-of-root path must stay absolute, got {key:?}"
        );
        assert!(!key.starts_with("@root"), "and must not gain a slot prefix");
    }

    /// Why: the table's numbering IS the corpus encoding's numbering; an
    /// off-by-one here would decode every additional-root chunk to the wrong
    /// tree.
    /// Test: this test.
    #[test]
    fn table_numbers_additional_roots_from_one() {
        let table = WatchedRoot::table(
            Path::new("/a/primary"),
            &[PathBuf::from("/b/extra"), PathBuf::from("/c/third")],
        );

        assert_eq!(table.len(), 3);
        assert_eq!(table[0].slot(), None);
        assert_eq!(table[0].raw(), Path::new("/a/primary"));
        assert_eq!(table[1].slot(), Some(0));
        assert_eq!(table[2].slot(), Some(1));
    }

    /// Why: the rescan sweep's existence check runs through this. Resolving an
    /// additional root's key against the primary would report every one of its
    /// files as deleted.
    /// Test: this test.
    #[test]
    fn resolves_a_key_against_its_own_root() {
        let table = WatchedRoot::table(
            Path::new("/a/primary"),
            &[PathBuf::from("/b/extra"), PathBuf::from("/c/third")],
        );

        assert_eq!(
            absolute_for_key(&table, Path::new("src/lib.rs")),
            PathBuf::from("/a/primary/src/lib.rs")
        );
        assert_eq!(
            absolute_for_key(&table, Path::new("@root1/src/lib.rs")),
            PathBuf::from("/b/extra/src/lib.rs")
        );
        assert_eq!(
            absolute_for_key(&table, Path::new("@root2/x.rs")),
            PathBuf::from("/c/third/x.rs")
        );
    }
}
