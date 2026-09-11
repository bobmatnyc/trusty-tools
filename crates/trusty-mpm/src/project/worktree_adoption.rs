//! Adopting a project's PRE-EXISTING worktrees into the daemon's registry
//! (#7357).
//!
//! Why: until #7357 the daemon's whole worktree view came from
//! [`crate::session_manager::worktree_registry::scan_registered_worktrees`],
//! which walks exactly `<repos_root>/<owner>/<repo>`. A project registered from
//! a checkout that does not sit at that depth under the repos root is never
//! interrogated, so every worktree beneath it is invisible to
//! `tm session reconcile-worktrees`, `prune-worktrees --merged-prs`, `tm doctor`
//! and the Disk survey. Registering the project changed nothing, because
//! registration wrote a [`crate::project::Project`] record and nothing else —
//! the guarded reclaim path stayed permanently unavailable for any repo that had
//! agent activity before it was registered, and the operator's only remaining
//! option was `git worktree remove --force` by hand.
//!
//! What: an ADOPTION record per pre-existing worktree, written by
//! [`AdoptionStore::record`] — the one record-writing function — and a set of
//! project ANCHORS derived from those records ([`adopted_anchors`]) that the
//! scan interrogates alongside its repos-root walk. The records carry
//! ATTRIBUTION only (which project a path belongs to, when it was adopted);
//! every FACT about a worktree still comes from `git worktree list --porcelain`
//! at scan time. That split is deliberate: two registries disagreeing about one
//! worktree is a failure this repository has already had twice (see
//! `worktree_registry`'s module docs), so nothing here caches a branch name, a
//! lock, or a liveness answer as truth.
//!
//! FAIL-OPEN, in the one direction that is safe here: an unreadable store, an
//! unreadable worktree, or a checkout git cannot answer for yields FEWER
//! records and FEWER anchors, never more. A missing anchor means the sweep sees
//! a worktree it saw before; a fabricated one would point a reclaim gate at a
//! directory nothing vouched for.
//!
//! FAIL-OPEN IS NOT SILENT (#7357). Every candidate this pass declines to
//! record lands in [`BackfillReport::unrecorded`] with the reason, and both
//! registration transports return that report to their caller, so a skipped or
//! foreign-claimed worktree is visible rather than a `tracing` line nobody
//! reads.
//! Test: `worktree_adoption_tests`.
//!
//! # Concurrency
//!
//! `worktrees.json` is written by the daemon and by any `tm` process that
//! registers a project, so the load → mutate → save cycle needs the same
//! cross-process critical section `projects.json` got in
//! [`crate::project::store`]: two registrations interleaving read/read/write/
//! write drop one project's records while both callers see success.
//! [`AdoptionStore::update`] is the only write path and runs the whole cycle
//! inside [`trusty_common::json_rmw::update`], which takes a file lock,
//! re-reads under it, and publishes by atomic rename. There is deliberately no
//! unlocked `save`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;
use trusty_common::json_rmw::{self, JsonRmwError};

use crate::session_manager::worktree_registry::{list_registered_worktrees, registry_root_for};

/// File name of the adoption store, inside the project-registry data directory.
///
/// Why: naming it once keeps the daemon writer and the out-of-process `tm`
/// reader from pointing at different files — the same discipline #4300
/// established for `projects.json`.
/// What: `"worktrees.json"`, beside `projects.json`.
/// Test: `adoption_store_path_is_beside_the_project_registry`.
pub const ADOPTED_WORKTREES_FILE: &str = "worktrees.json";

/// Directory names a project parks its worktrees under, checked in addition to
/// git's own registry (#7357).
///
/// Why: git answers for every worktree IT registered, which is the enumeration
/// that matters. These two names catch the remainder — a directory that looks
/// like a worktree but whose gitdir pointer is missing or unreadable, which is
/// exactly the case the backfill must skip loudly rather than silently.
/// What: `.claude/worktrees` (the Claude Code harness's own store) and
/// `.worktrees` (trusty-mpm's).
/// Test: `backfill_skips_a_worktree_with_no_gitdir_pointer_and_still_succeeds`.
const WORKTREE_PARENTS: [&str; 2] = [".claude/worktrees", ".worktrees"];

/// One adopted worktree: which project owns a path, and when that was decided.
///
/// Why: attribution is the only thing the daemon could not derive. Everything
/// else about a worktree — its branch, whether it is locked, whether its
/// directory still exists — is git's answer at scan time, and storing a second
/// copy is how two registries start to disagree.
/// What: `path` is the canonical worktree directory; `project` is the
/// registered project name; `checkout` is the project checkout whose registry
/// named it (this is what becomes a scan anchor); `branch` is recorded for
/// operator-facing reporting only and is never read back as truth.
/// Test: `record_writes_one_entry_per_worktree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptedWorktree {
    /// Canonical path of the worktree directory.
    pub path: PathBuf,
    /// Registered project name this worktree is attributed to.
    pub project: String,
    /// The project checkout whose git registry named this worktree.
    pub checkout: PathBuf,
    /// Short branch name at adoption time, `None` when detached or unknown.
    pub branch: Option<String>,
    /// When the adoption record was written.
    pub adopted_at: DateTime<Utc>,
}

/// What [`AdoptionStore::record`] did with one candidate.
///
/// Why: the caller has to be able to tell "already ours" from "someone else's"
/// — the second is the case #7357 requires the backfill to leave strictly
/// alone, and collapsing both into a silent no-op would hide a real conflict.
/// What: three outcomes, exactly one of which writes.
/// Test: `record_is_idempotent_for_the_same_project`,
/// `record_never_reattributes_another_projects_worktree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// A new record was added.
    Recorded,
    /// This project already holds a record for that path; nothing changed.
    AlreadyRecorded,
    /// Another project holds the record; it was left untouched. Carries that
    /// project's name.
    ClaimedByAnother(String),
}

/// The adoption records, loaded from and published to one JSON document.
///
/// Why: a flat list keyed by path is enough — the store answers exactly two
/// questions ("who owns this path?" and "which checkouts should the scan
/// interrogate?") and carries no state a reader could act on destructively.
/// What: just the entries — the FILE is named by the directory each entry
/// point passes, never cached on the value, so no caller can hold a store that
/// remembers a path it is no longer allowed to write. Reads go through
/// [`load`](Self::load) and take no lock; every WRITE goes through
/// [`update`](Self::update), which holds the file lock across the whole
/// read-modify-write cycle (see this module's Concurrency section).
/// Test: every test in `worktree_adoption_tests`.
#[derive(Debug, Default)]
pub struct AdoptionStore {
    entries: Vec<AdoptedWorktree>,
}

impl AdoptionStore {
    /// Load the store from `dir`, treating an absent or unreadable file as
    /// empty.
    ///
    /// Why: fail-open is the safe direction (see the module docs) and it also
    /// makes first run work with no init step. A parse failure is warned about
    /// rather than swallowed, because a corrupt store is a real fault even
    /// though it must not stop registration.
    /// What: reads `<dir>/worktrees.json`; any I/O or parse failure yields an
    /// empty store. A corrupt document is never republished over by this path
    /// — see [`update`](Self::update), which refuses instead.
    /// Test: `load_of_a_missing_file_is_empty`,
    /// `load_of_a_corrupt_file_is_empty`.
    pub fn load(dir: &Path) -> Self {
        let file_path = dir.join(ADOPTED_WORKTREES_FILE);
        let entries = match std::fs::read_to_string(&file_path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                warn!(
                    store = %file_path.display(),
                    "worktree adoption store is unreadable ({e}); continuing with no adopted \
                     worktrees"
                );
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Self { entries }
    }

    /// Every record currently held.
    pub fn entries(&self) -> &[AdoptedWorktree] {
        &self.entries
    }

    /// Write ONE adoption record, refusing to re-attribute an existing one.
    ///
    /// Why: this is the single record-writing function — the backfill has no
    /// other way in, so "registering twice produces one record" and "another
    /// project's worktree is never taken" are properties of this function
    /// rather than of each caller remembering to check. Re-attribution is the
    /// dangerous direction: the recorded project is what a later sweep uses to
    /// bound a candidate, so moving a path from one project to another would
    /// silently widen what that project's sweep may propose.
    /// What: keyed on `path`. An existing record for the same project is left
    /// as it is ([`RecordOutcome::AlreadyRecorded`] — the adoption timestamp is
    /// not refreshed, so the record keeps naming when the path was FIRST seen);
    /// one naming a different project is left as it is too
    /// ([`RecordOutcome::ClaimedByAnother`]).
    /// Test: `record_writes_one_entry_per_worktree`,
    /// `record_is_idempotent_for_the_same_project`,
    /// `record_never_reattributes_another_projects_worktree`.
    pub fn record(&mut self, entry: AdoptedWorktree) -> RecordOutcome {
        if let Some(existing) = self.entries.iter().find(|e| e.path == entry.path) {
            if existing.project == entry.project {
                return RecordOutcome::AlreadyRecorded;
            }
            return RecordOutcome::ClaimedByAnother(existing.project.clone());
        }
        self.entries.push(entry);
        RecordOutcome::Recorded
    }

    /// Run one read-modify-write against `<dir>/worktrees.json` under a lock
    /// (#7357).
    ///
    /// Why: this is the ONLY write path, and the fix for the lost-update race
    /// the sibling `projects.json` store already closed (#4300). The previous
    /// shape — [`load`](Self::load), mutate in memory, `fs::write` the whole
    /// file back — left an unguarded window in which a second registering
    /// process could publish its own document, which this one then clobbered;
    /// nothing reported an error and one project's records simply vanished.
    /// What: delegates the whole cycle to [`trusty_common::json_rmw::update`],
    /// which takes an advisory exclusive lock on a `.lock` sidecar, re-reads
    /// the entries from disk under it (a caller's in-memory copy is never
    /// trusted), applies `f`, and publishes by atomic rename. The directory is
    /// created first, because the lock sidecar lives in it.
    ///
    /// NEVER FAIL-OPEN, unlike the readers: a store that is present but will
    /// not parse returns `Err` and is left byte-for-byte alone, rather than
    /// being replaced with the caller's view of it. The caller's remedy is to
    /// report the failure and record nothing.
    ///
    /// Not reentrant: `f` must not call [`update`](Self::update) again on the
    /// same directory — the second acquisition uses a fresh descriptor and
    /// self-deadlocks. Lock acquisition blocks, so an async caller must run
    /// this on a blocking-safe thread.
    ///
    /// # Errors
    ///
    /// [`JsonRmwError`] when the lock cannot be taken, the document cannot be
    /// read or parsed, or the publish fails.
    ///
    /// Test: `record_writes_one_entry_per_worktree`,
    /// `update_refuses_to_clobber_a_corrupt_store`,
    /// `concurrent_backfills_never_lose_a_projects_records`.
    pub fn update<R, F>(dir: &Path, f: F) -> Result<R, JsonRmwError>
    where
        F: FnOnce(&mut Self) -> R,
    {
        let file_path = dir.join(ADOPTED_WORKTREES_FILE);
        std::fs::create_dir_all(dir).map_err(|source| JsonRmwError::Io {
            path: file_path.clone(),
            source,
        })?;
        json_rmw::update::<Vec<AdoptedWorktree>, R, JsonRmwError, _>(&file_path, move |entries| {
            let mut store = Self {
                entries: std::mem::take(entries),
            };
            let out = f(&mut store);
            *entries = store.entries;
            Ok(out)
        })
    }
}

/// Why one candidate was NOT recorded (#7357).
///
/// Why: "skipped" alone tells an operator a worktree is missing from the
/// registry but not what to do about it, and the three causes need three
/// different actions — fix the checkout path, repair the worktree, or accept
/// that another project already owns it.
/// What: one variant per refusal in [`backfill_checkout`], serialized
/// internally-tagged so the reason and its detail are one flat JSON object
/// beside the path.
/// Test: `backfill_names_the_skipped_worktree_and_why`,
/// `backfill_leaves_another_projects_worktree_alone`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SkipReason {
    /// The checkout is not a git repository root, so nothing under it was
    /// interrogated at all.
    NotARepositoryRoot,
    /// A directory shaped like a worktree that git does not name and whose own
    /// gitdir pointer is absent or unreadable.
    NoGitdirPointer,
    /// Another project already holds the record; it was left untouched.
    ClaimedByAnother {
        /// The project whose record stands.
        project: String,
    },
}

/// One candidate a backfill pass declined to record, and why (#7357).
///
/// Test: `backfill_names_the_skipped_worktree_and_why`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnrecordedWorktree {
    /// The candidate directory (or, for [`SkipReason::NotARepositoryRoot`],
    /// the checkout itself).
    pub path: PathBuf,
    /// Why it was left out.
    #[serde(flatten)]
    pub reason: SkipReason,
}

/// What one backfill pass did (#7357).
///
/// Why: before this was returned to callers it reached only `tracing`, so a
/// registration that adopted nothing looked exactly like one that adopted
/// everything. Both registration transports now serialize this into their
/// response, which is the whole signal: registration still SUCCEEDS whatever
/// is in here.
/// What: the four counts, plus one [`UnrecordedWorktree`] per candidate that
/// did not become a record — so `unrecorded.len() == skipped +
/// claimed_by_another` — plus the persistence failure, if any.
/// Test: `backfill_records_every_pre_existing_worktree`,
/// `backfill_is_idempotent`, `backfill_names_the_skipped_worktree_and_why`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillReport {
    /// Records newly written.
    pub recorded: usize,
    /// Paths this project already held a record for.
    pub already_recorded: usize,
    /// Paths another project holds, left untouched.
    pub claimed_by_another: usize,
    /// Candidate directories that could not be read, skipped with a warning.
    pub skipped: usize,
    /// Every candidate that did not become a record, with its reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unrecorded: Vec<UnrecordedWorktree>,
    /// Set when the store could not be published, so nothing was written even
    /// though candidates were found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_error: Option<String>,
}

/// Adopt every worktree that already exists under `checkout` into `project`
/// (#7357).
///
/// Why: this is the whole fix. A project registered after its worktrees exist
/// must end up with the same registry entries it would have had if the
/// worktrees had been provisioned afterwards, and the only missing ingredient
/// is attribution — so the backfill writes attribution and nothing else.
/// What: enumerates `git worktree list --porcelain` at `checkout` (dropping the
/// main checkout and bare records — neither is a worktree to reclaim), then
/// sweeps `.claude/worktrees/*` and `.worktrees/*` for directories git did not
/// name. Each candidate goes through [`AdoptionStore::record`]. The store is
/// saved once, at the end, only when something changed.
///
/// FAIL-OPEN, and the caller must not treat this as a gate: a `checkout` that
/// is not a git repository root, a git invocation that will not answer, and a
/// candidate directory with no readable gitdir pointer are each SKIPPED with a
/// warning. Registration is a bookkeeping call and must succeed whatever the
/// filesystem says; the cost of a skip is that one worktree stays as invisible
/// as it was before this function existed.
/// Test: `backfill_records_every_pre_existing_worktree`,
/// `backfill_is_idempotent`,
/// `backfill_leaves_another_projects_worktree_alone`,
/// `backfill_skips_a_worktree_with_no_gitdir_pointer_and_still_succeeds`,
/// `backfill_names_the_skipped_worktree_and_why`,
/// `concurrent_backfills_never_lose_a_projects_records`.
pub fn backfill_checkout(
    store_dir: &Path,
    project: &str,
    checkout: &Path,
    now: DateTime<Utc>,
) -> BackfillReport {
    let mut report = BackfillReport::default();
    let Some(canonical_checkout) = canonical_repo_root(checkout) else {
        warn!(
            checkout = %checkout.display(),
            project,
            "#7357: not a git repository root, or git would not answer for it — no pre-existing \
             worktrees adopted"
        );
        // #7357: reported, not merely logged — a caller that gets an empty
        // report otherwise cannot tell this apart from a checkout with no
        // worktrees at all.
        report.skipped = 1;
        report.unrecorded.push(UnrecordedWorktree {
            path: checkout.to_path_buf(),
            reason: SkipReason::NotARepositoryRoot,
        });
        return report;
    };
    let (candidates, unreadable) = discover_worktrees(&canonical_checkout);
    report.skipped = unreadable.len();
    report.unrecorded = unreadable;
    if candidates.is_empty() {
        return report;
    }

    // #7357: one locked read-modify-write, so two concurrent registrations
    // serialise instead of clobbering each other's records.
    let written = AdoptionStore::update(store_dir, |store| {
        let mut tally = BackfillReport::default();
        for (path, branch) in candidates {
            let outcome = store.record(AdoptedWorktree {
                path: path.clone(),
                project: project.to_string(),
                checkout: canonical_checkout.clone(),
                branch,
                adopted_at: now,
            });
            match outcome {
                RecordOutcome::Recorded => tally.recorded += 1,
                RecordOutcome::AlreadyRecorded => tally.already_recorded += 1,
                RecordOutcome::ClaimedByAnother(other) => {
                    tally.claimed_by_another += 1;
                    warn!(
                        worktree = %path.display(),
                        project,
                        claimed_by = %other,
                        "#7357: worktree is already attributed to another project — left untouched"
                    );
                    tally.unrecorded.push(UnrecordedWorktree {
                        path,
                        reason: SkipReason::ClaimedByAnother { project: other },
                    });
                }
            }
        }
        tally
    });
    match written {
        Ok(tally) => {
            report.recorded = tally.recorded;
            report.already_recorded = tally.already_recorded;
            report.claimed_by_another = tally.claimed_by_another;
            report.unrecorded.extend(tally.unrecorded);
        }
        Err(e) => {
            warn!(
                store = %store_dir.display(),
                "#7357: could not persist worktree adoption records ({e}); registration still \
                 succeeded"
            );
            report.persist_error = Some(e.to_string());
        }
    }
    report
}

/// The distinct project checkouts the adoption records name (#7357).
///
/// Why: the scan needs ANCHORS, not worktrees — it re-derives every worktree
/// fact from git at each pass, so what it is missing is only "which checkouts
/// should I ask?". Returning checkouts rather than worktree paths is what keeps
/// this store from becoming a second, drifting copy of git's registry.
/// What: the `checkout` field of every record, de-duplicated and sorted.
/// Test: `anchors_are_the_distinct_checkouts`.
pub fn adopted_anchors(store_dir: &Path) -> Vec<PathBuf> {
    let store = AdoptionStore::load(store_dir);
    let set: BTreeSet<PathBuf> = store.entries.iter().map(|e| e.checkout.clone()).collect();
    set.into_iter().collect()
}

/// The adopted anchors under a framework root the caller already knows (#7357).
///
/// Why: the daemon WRITES its records under `state.framework_root()`, so a
/// reader that resolves `$HOME` instead reads a different file whenever the two
/// disagree — which is exactly what an isolated-managed daemon and every
/// daemon test do. Every entry point holding a `DaemonState` resolves anchors
/// through here rather than [`default_adopted_anchors`], which keeps the reader
/// and the writer on one file and keeps those tests hermetic.
/// What: [`adopted_anchors`] over
/// [`crate::project::registry_data_dir_under`].
/// Test: `parity_register_adopts_the_same_worktrees_across_transports`.
pub fn adopted_anchors_under(framework_root: &Path) -> Vec<PathBuf> {
    adopted_anchors(&super::registry_data_dir_under(framework_root))
}

/// The adopted anchors for a process with no `DaemonState` (#7357).
///
/// Why: a few entry points — `tm doctor`'s worktree-disk probe above all — run
/// with no daemon handle, and resolving the framework root the same way
/// [`crate::project::registry_data_dir`] does is what keeps them on the file
/// the daemon writes.
///
/// RESOLVE THIS AT AN ENTRY POINT ONLY. A scan function that calls it
/// internally reads the developer's real
/// `~/.trusty-mpm/project-registry/worktrees.json` from its own unit tests,
/// which is how #7357's first round lost test hermeticity; every scan below an
/// entry point takes `adopted: &[PathBuf]` instead.
/// What: [`adopted_anchors`] over [`crate::project::registry_data_dir`].
/// Test: covered through `adopted_anchors`; the `$HOME`-derived root itself is
/// covered by `core::paths`' own tests.
pub fn default_adopted_anchors() -> Vec<PathBuf> {
    adopted_anchors(&super::registry_data_dir())
}

/// Resolve `checkout` to the canonical root of the repository it belongs to.
///
/// Why: an anchor that is merely a directory INSIDE a repository makes git walk
/// up and answer for an unrelated enclosing repository — the same trap
/// `worktree_registry::scan_from_anchor` guards. Confirming the path IS the
/// root is what makes the recorded anchor safe to hand the scan later.
/// What: `None` unless the path is a directory that git reports as its own
/// repository root.
/// Test: `backfill_ignores_a_path_that_is_not_a_repository_root`.
fn canonical_repo_root(checkout: &Path) -> Option<PathBuf> {
    if !checkout.is_dir() {
        return None;
    }
    let root = registry_root_for(checkout)?;
    let canonical_root = std::fs::canonicalize(&root).unwrap_or(root);
    let canonical_checkout = std::fs::canonicalize(checkout).ok()?;
    (canonical_checkout == canonical_root).then_some(canonical_checkout)
}

/// Every worktree candidate under `checkout`, plus the ones left out.
///
/// Why: git's registry is the enumeration that matters, and reusing
/// [`list_registered_worktrees`] rather than re-parsing porcelain here keeps
/// one parser in the crate. The directory sweep afterwards exists for the
/// remainder only — a directory shaped like a worktree that git does not name,
/// which is either registered to some other checkout or broken.
/// What: `(path, branch)` pairs, and one [`UnrecordedWorktree`] per candidate
/// directory with no readable gitdir pointer. The main checkout and bare
/// records are dropped: neither is a worktree an operator could reclaim.
/// Test: `backfill_records_every_pre_existing_worktree`,
/// `backfill_skips_a_worktree_with_no_gitdir_pointer_and_still_succeeds`,
/// `backfill_names_the_skipped_worktree_and_why`.
fn discover_worktrees(
    checkout: &Path,
) -> (Vec<(PathBuf, Option<String>)>, Vec<UnrecordedWorktree>) {
    let mut found: Vec<(PathBuf, Option<String>)> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for wt in list_registered_worktrees(checkout).unwrap_or_default() {
        if wt.bare || wt.is_main {
            continue;
        }
        let path = std::fs::canonicalize(&wt.path).unwrap_or_else(|_| wt.path.clone());
        if seen.insert(path.clone()) {
            found.push((path, wt.branch));
        }
    }

    let mut skipped: Vec<UnrecordedWorktree> = Vec::new();
    for parent in WORKTREE_PARENTS {
        let Ok(entries) = std::fs::read_dir(checkout.join(parent)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if seen.contains(&canonical) {
                continue;
            }
            // #7357: git did not name it, so the only thing that can say
            // whether this is a worktree at all is its own gitdir pointer — the
            // `.git` FILE `git worktree add` writes. Unreadable means unknown,
            // and unknown is skipped, never adopted on the strength of the
            // directory's name.
            if !has_gitdir_pointer(&path) {
                warn!(
                    worktree = %path.display(),
                    "#7357: no readable gitdir pointer — skipping this directory rather than \
                     adopting it"
                );
                skipped.push(UnrecordedWorktree {
                    path,
                    reason: SkipReason::NoGitdirPointer,
                });
                continue;
            }
            seen.insert(canonical.clone());
            found.push((canonical, None));
        }
    }
    (found, skipped)
}

/// Does `path` carry the gitdir pointer `git worktree add` writes (#7357)?
///
/// Why: a linked worktree's `.git` is a FILE holding `gitdir: <admin dir>`,
/// and that pointer is the only local evidence the directory is a worktree at
/// all. Asking git instead would answer about the ENCLOSING repository — the
/// project checkout — and so would say yes for any directory sitting inside it,
/// including an empty one the harness left behind.
/// What: `true` only when `<path>/.git` reads as a `gitdir:` pointer.
/// Test: `backfill_skips_a_worktree_with_no_gitdir_pointer_and_still_succeeds`.
fn has_gitdir_pointer(path: &Path) -> bool {
    std::fs::read_to_string(path.join(".git"))
        .is_ok_and(|raw| raw.trim_start().starts_with("gitdir:"))
}

#[cfg(test)]
#[path = "worktree_adoption_tests.rs"]
mod worktree_adoption_tests;
