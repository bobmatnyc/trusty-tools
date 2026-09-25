//! Containment classification for project / index roots (#4289).
//!
//! Why: Two write paths accept an operator-chosen directory — `POST
//! /api/projects` (register a project) and `POST /api/project-tools/index`
//! (create a trusty-search index over one). Both previously guarded only
//! EXACT-match collisions, so a subdirectory of an already-covered tree, or
//! an ancestor that swallows one, was accepted from a button click.
//! Overlapping index roots are not cosmetic: #402 / #2178 recorded a reindex
//! hijacking an existing index and pruning its corpus, and duplicate coverage
//! doubles embedding compute and `fseventsd` load.
//! What: [`classify_root_overlap`] answers "how does this candidate relate to
//! a root I already have?" for all three cases — same tree, candidate inside,
//! candidate encloses — and both callers refuse on any of them.
//! [`find_project_overlap`] applies it to registry entries;
//! [`registry_overlaps`] reports the pairs a pre-guard registry already
//! contains, without rejecting them.
//! Test: `same_tree_is_detected_through_a_symlink_and_trailing_slash`,
//! `candidate_inside_a_registered_root_is_flagged`,
//! `candidate_that_encloses_a_registered_root_is_flagged`,
//! `siblings_do_not_overlap`, `registry_overlaps_reports_each_pair_once`.

use std::path::{Path, PathBuf};

use trusty_common::index_id::identifies_same_path;

use super::{ProjectEntry, ProjectStatus};

/// How a candidate directory relates to a root that is already known (#4289).
///
/// Why: The three cases need distinct operator-facing wording — "you already
/// have this", "you are inside one", "you would swallow one" — and the
/// ancestor case is the one a user picking a folder is least likely to
/// notice.
/// What: The relation OF THE CANDIDATE TO the known root.
/// Test: `candidate_inside_a_registered_root_is_flagged`,
/// `candidate_that_encloses_a_registered_root_is_flagged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootOverlap {
    /// Both paths name one directory tree, however each was spelled.
    SameTree,
    /// The candidate lives below the known root.
    InsideKnownRoot,
    /// The candidate is an ancestor of the known root and would enclose it.
    EnclosesKnownRoot,
}

/// Does `candidate` overlap the already-known root `known`?
///
/// Why: `trusty_common::index_id::identifies_same_path` already answers the
/// exact-match half (#2336/#2519: `(dev, ino)` comparison, so a symlink
/// alias, a bind mount, and a macOS case variant all resolve to one tree).
/// The containment half has no shared primitive, and a naive string prefix
/// would miss exactly the aliases that check exists to catch.
/// What: Same tree wins first; otherwise each of the candidate's ancestors is
/// compared against `known` with the same `(dev, ino)` primitive, then the
/// reverse direction. Returns None for unrelated trees. A trailing slash is
/// absorbed by `Path` component equality on the deleted-directory fallback
/// path and by the inode comparison otherwise.
/// Test: `same_tree_is_detected_through_a_symlink_and_trailing_slash`,
/// `siblings_do_not_overlap`.
pub fn classify_root_overlap(candidate: &Path, known: &Path) -> Option<RootOverlap> {
    if identifies_same_path(candidate, known) {
        return Some(RootOverlap::SameTree);
    }
    if is_below(candidate, known) {
        return Some(RootOverlap::InsideKnownRoot);
    }
    if is_below(known, candidate) {
        return Some(RootOverlap::EnclosesKnownRoot);
    }
    None
}

/// Is `inner` a strict descendant of `outer`?
///
/// Why: Comparing canonical strings with `starts_with` is wrong twice over —
/// it misses an aliased spelling of `outer`, and it matches a sibling whose
/// name merely shares a prefix (`/srv/app2` under `/srv/app`).
/// What: Walks `inner`'s ancestors excluding itself and asks the shared
/// same-tree primitive about each, so path-segment boundaries are structural
/// rather than textual.
/// Test: `siblings_do_not_overlap`.
fn is_below(inner: &Path, outer: &Path) -> bool {
    inner
        .ancestors()
        .skip(1)
        .any(|ancestor| identifies_same_path(ancestor, outer))
}

/// A registered project the candidate directory collides with (#4289).
///
/// Why: A refusal that does not name the existing project leaves the operator
/// guessing which of their folders is in the way.
/// What: The relation plus the registered entry's name and path.
/// Test: `candidate_inside_a_registered_root_is_flagged`.
#[derive(Debug, Clone)]
pub struct ProjectOverlap {
    /// Relation of the candidate directory to [`Self::path`].
    pub overlap: RootOverlap,
    /// Registered project name.
    pub name: String,
    /// Registered project root.
    pub path: PathBuf,
}

impl ProjectOverlap {
    /// Operator-facing refusal sentence naming the registered project.
    ///
    /// Why: The HTTP refusal and the startup report want the same wording, and
    /// a route should not hand-assemble it per call site.
    /// What: One sentence per [`RootOverlap`] arm, always naming the existing
    /// project and its path.
    /// Test: `candidate_inside_a_registered_root_is_flagged`.
    pub fn refusal(&self) -> String {
        let (name, path) = (&self.name, self.path.display());
        match self.overlap {
            RootOverlap::SameTree => {
                format!("This folder is already registered as \"{name}\" ({path})")
            }
            RootOverlap::InsideKnownRoot => {
                format!("This folder is inside the registered project \"{name}\" ({path})")
            }
            RootOverlap::EnclosesKnownRoot => {
                format!("This folder contains the registered project \"{name}\" ({path})")
            }
        }
    }
}

/// Find the registered project that `candidate` would duplicate or straddle.
///
/// Why: `POST /api/projects` canonicalizes its argument and then upserts, so
/// without this check a second registration of one tree under another
/// spelling — or of a subdirectory — silently produced a second entry, and
/// every downstream index/import decision inherited the ambiguity.
/// What: Re-registering a path the registry already stores verbatim is the
/// idempotent refresh `om connect` relies on and yields None. `Removed`
/// entries (directory gone) never block a fresh registration. Otherwise the
/// first overlapping entry in path order is returned, so the answer does not
/// depend on `HashMap` iteration order.
/// Test: `re_registering_a_known_path_is_not_an_overlap`,
/// `removed_projects_do_not_block_registration`,
/// `candidate_inside_a_registered_root_is_flagged`.
pub fn find_project_overlap(candidate: &Path, entries: &[ProjectEntry]) -> Option<ProjectOverlap> {
    if entries.iter().any(|entry| entry.path == candidate) {
        return None;
    }
    let mut live: Vec<&ProjectEntry> = entries
        .iter()
        .filter(|entry| entry.status != ProjectStatus::Removed)
        .collect();
    live.sort_by(|a, b| a.path.cmp(&b.path));
    live.iter().find_map(|entry| {
        classify_root_overlap(candidate, &entry.path).map(|overlap| ProjectOverlap {
            overlap,
            name: entry.name.clone(),
            path: entry.path.clone(),
        })
    })
}

/// A pair of registered projects whose roots already overlap (#4289).
///
/// Why: Registries written before this guard can hold overlapping entries.
/// Rejecting them at load would lock the operator out of every project at
/// once, so the loader reports instead.
/// What: The later entry in path order, the entry it overlaps, and the
/// relation between them.
/// Test: `registry_overlaps_reports_each_pair_once`.
#[derive(Debug, Clone)]
pub struct RegistryOverlap {
    /// Name of the entry whose relation is described.
    pub name: String,
    /// Root of the entry whose relation is described.
    pub path: PathBuf,
    /// Relation of [`Self::path`] to [`Self::against_path`].
    pub overlap: RootOverlap,
    /// Name of the entry it overlaps.
    pub against_name: String,
    /// Root of the entry it overlaps.
    pub against_path: PathBuf,
}

/// Report every overlapping pair already present in a registry.
///
/// Why: The guard only covers registrations made through it; a registry
/// predating it needs the overlaps surfaced so the operator can clean them up
/// deliberately.
/// What: Sorts by path for a deterministic report, then compares each pair
/// once. Never errors and never filters an entry out of the registry.
/// Test: `registry_overlaps_reports_each_pair_once`.
pub fn registry_overlaps(entries: &[ProjectEntry]) -> Vec<RegistryOverlap> {
    let mut live: Vec<&ProjectEntry> = entries
        .iter()
        .filter(|entry| entry.status != ProjectStatus::Removed)
        .collect();
    live.sort_by(|a, b| a.path.cmp(&b.path));
    let mut found = Vec::new();
    for (index, entry) in live.iter().enumerate() {
        for other in live.iter().take(index) {
            if let Some(overlap) = classify_root_overlap(&entry.path, &other.path) {
                found.push(RegistryOverlap {
                    name: entry.name.clone(),
                    path: entry.path.clone(),
                    overlap,
                    against_name: other.name.clone(),
                    against_path: other.path.clone(),
                });
            }
        }
    }
    found
}
