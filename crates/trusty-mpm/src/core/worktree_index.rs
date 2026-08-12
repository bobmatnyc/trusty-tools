//! Register a freshly created git worktree with trusty-search, BM25 + KG only (#5060).
//!
//! Why: a worktree only became searchable when a managed session was LAUNCHED
//! in it — `core::session_launch::register_project_index` runs at launch, not
//! at creation. Most worktrees never reach that point in a state anyone
//! queries, so of the 70+ live worktrees for this repo only ~4 had a
//! registered index (#5045). A bare `search` from an unindexed worktree
//! answers `404 unknown index`, which reads as permanent and pushes agents
//! into silent grep fallbacks. Indexing at CREATION closes that window.
//!
//! What: [`index_new_worktree_in_background`] is the one entry point. It
//! spawns a detached OS thread and returns immediately, then registers the
//! worktree's index with `skip_vector: true` — BM25 and KG only, no
//! embeddings.
//!
//! Why BM25+KG and not a full index (owner ruling, 2026-08-06): a worktree
//! differs from its base checkout by a small diff. Exact text (BM25) and the
//! symbol graph (KG) are branch-specific and must be worktree-accurate;
//! conceptual similarity is not — it does not change because a branch moved a
//! few functions. So the expensive lane is built once on the base checkout and
//! the cheap lanes are built per worktree.
//!
//! Why detached and never synchronous: a cold BM25+KG build of this workspace
//! (5,865 tracked files, 75,282 chunks) took **35.0 s** measured end-to-end
//! against the live daemon — 33.5 s to `lexical: ready`, 1.6 s more to `graph:
//! ready`. That is nowhere near unnoticeable, so there is no
//! sync-under-threshold path: worktree creation and session launch never wait
//! on indexing at any repo size.
//!
//! Disk cost, which dominates the wall-clock cost (#5065 review): every index
//! trusty-search creates is COLOCATED — `create_index_handler` hardcodes
//! `colocated: true`, so the store lands in `<worktree>/.trusty-search/`. Indexing
//! at creation therefore multiplies on-disk index storage by the number of LIVE
//! worktrees. `skip_vector` does not shrink that much: it suppresses
//! `hnsw.usearch`, but the corpus and KG live in `index.redb`, which has no
//! embeddings table, and the redb is the bigger file. Measured on this repo:
//!
//! | store | `index.redb` | `hnsw.usearch` |
//! |---|---|---|
//! | base checkout (full index) | 2.19 GiB | 777 MiB |
//! | a `skip_vector` worktree | 1.48 GiB | 1.8 KiB (empty) |
//! | a vector-bearing worktree | 1.00 GiB | 121 MiB |
//!
//! So the BM25+KG ruling buys back the HNSW file and roughly nothing else: a
//! worktree index still costs ~1–1.5 GiB. At the 59 live worktrees this repo
//! carries that is a peak on the order of 60–90 GB.
//!
//! Budget, stated so the tradeoff is explicit rather than discovered: this is
//! PEAK usage, not a leak. The store lives inside the worktree directory, so
//! `git worktree remove` reclaims it in full, and `session_manager::search_gc`
//! plus the #2033 orphan sweep reclaim the daemon-side registration. The cost
//! scales with worktrees you are actively keeping, and a fleet large enough to
//! matter is already spending comparable disk on the checkouts themselves.
//! Gating creation-time indexing on repo size, or routing worktree indexes to
//! non-colocated storage, are the two levers if that budget stops holding;
//! neither is taken here (the second needs a trusty-search change).
//!
//! Idempotence: the daemon's `POST /indexes` is find-or-create and its reindex
//! trigger is freshness-gated (skipped when the index already holds chunks
//! indexed within the last hour), so a second call against an already-indexed
//! worktree costs two short HTTP round trips and no walk.
//!
//! Deletion: already handled, and deliberately not re-implemented here.
//! `session_manager::search_gc` derives the SAME id from the SAME
//! `resolve_project_root` + `derive_index_id` pair and deletes it at
//! decommission, with a periodic orphan sweep behind it (#2033).
//!
//! Test: `worktree_index_tests.rs`.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// Why an index was or was not registered for a newly created worktree.
///
/// Why: [`index_new_worktree`] runs on a detached thread where a return value
/// would be dropped, so the outcome would otherwise be observable only by
/// scraping logs — and the "do not fail open silently" requirement needs each
/// skip reason to be distinguishable in a test. Returning a typed outcome lets
/// the decision logic be exercised synchronously and off the daemon.
/// What: `Registered` carries the derived index id and means the daemon
/// CONFIRMED the index with a 2xx; the other variants name the specific reason
/// nothing is known to have been registered.
/// Test: `skips_path_that_is_not_a_worktree`, `skips_missing_path`,
/// `reports_unconfirmed_rather_than_registered_when_the_daemon_never_answered`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeIndexOutcome {
    /// The daemon confirmed an index under this id (created, or already
    /// present — `POST /indexes` is find-or-create).
    Registered(String),
    /// The id was derived and a registration was REQUESTED, but the daemon
    /// never confirmed it: unreachable, non-2xx, or suppressed because this is
    /// a test process. The worktree is NOT known to be indexed — #5045 measured
    /// the daemon-absent case at ~94% of worktrees, so this is the common
    /// outcome, not an exotic one, and nothing may treat it as success.
    RegistrationUnconfirmed {
        /// The derived index id. Reported so a log line can name it, never so
        /// a caller can pin it as if the index existed.
        index_id: String,
        /// Which of the three non-confirmations happened.
        reason: trusty_common::search_index::IndexRegistration,
    },
    /// The path does not exist, or is not a directory.
    PathMissing,
    /// The path exists but is not a git worktree — a plain clone, or no repo
    /// at all. Only worktrees are indexed here; a plain checkout's index is
    /// the operator's own opt-in decision, not something worktree creation
    /// may make on their behalf.
    NotAWorktree,
    /// The index id derived to the empty string (a filesystem-root path).
    EmptyIndexId,
}

/// Fire-and-forget: register `worktree_path` with trusty-search, BM25+KG only.
///
/// Why: worktree creation and session launch must never wait on indexing — a
/// 35 s embed-free walk (see the module doc) in front of a shell prompt is a
/// worse bug than the missing index it fixes. Spawning here rather than at
/// each call site keeps that guarantee in ONE place, so a future caller cannot
/// accidentally add a blocking one.
/// What: spawns a detached OS thread running [`index_new_worktree`] and
/// returns immediately. The thread is never joined; every failure inside it is
/// logged and swallowed.
///
/// How a failure surfaces (#5065 review): in this process, as a `warn` naming
/// the outcome — [`index_new_worktree`] distinguishes a confirmed registration
/// from a requested-but-unconfirmed one, so the log never claims a success it
/// did not observe. It does NOT surface through `tm doctor`'s existing `search`
/// check: that probes only the index id derived from the CURRENT project
/// directory (`daemon::doctor::check_search` / `expected_search_index_id`), so
/// it sees nothing for the other ~58 worktrees — which is the blind spot #5045
/// documents. #5093 adds the pin-resolution check that actually sweeps; this
/// module deliberately adds no second doctor check.
/// Test: `background_entry_point_routes_through_spawn_detached`; the
/// never-block guarantee itself is pinned by
/// `spawn_detached_returns_before_work_finishes`, and the decision logic by
/// the [`index_new_worktree`] tests.
pub fn index_new_worktree_in_background(worktree_path: PathBuf) {
    // #5069: this worktree's index is BM25+KG only, so the base checkout it was
    // cut from has to carry the embedding lane or semantic search has no index
    // to route to. Its own detached thread — the base facet is the expensive
    // index, and a failure there must not delay or mask this one.
    crate::core::base_facet_index::ensure_base_facet_indexed_in_background(worktree_path.clone());

    spawn_detached(move || {
        let outcome = index_new_worktree(&worktree_path);
        match outcome {
            WorktreeIndexOutcome::Registered(id) => {
                info!(
                    worktree = %worktree_path.display(),
                    index_id = %id,
                    "worktree index registered (BM25+KG, no vector lane) — #5060"
                );
            }
            WorktreeIndexOutcome::RegistrationUnconfirmed { index_id, reason } => {
                warn!(
                    worktree = %worktree_path.display(),
                    index_id = %index_id,
                    ?reason,
                    "worktree index REQUESTED but not confirmed — the worktree is \
                     not known to be indexed (#5065)"
                );
            }
            other => {
                debug!(
                    worktree = %worktree_path.display(),
                    outcome = ?other,
                    "worktree index not registered"
                );
            }
        }
    });
}

/// Run `work` on a detached OS thread and return without waiting for it.
///
/// Why: the one place the never-block guarantee is enforced, and the only
/// place it can be TESTED. Asserting the property on
/// [`index_new_worktree_in_background`] directly proves nothing under the test
/// harness: `trusty_common::search_index` refuses daemon writes from a test
/// process (#4255), so the work finishes in microseconds and a `.join()` — the
/// natural "let's return the outcome to the caller" refactor — passes a timing
/// assertion unnoticed. Routing through a helper whose work the test supplies
/// lets the test make that work slow on purpose, so a join fails loudly.
/// What: `std::thread::spawn` with the handle dropped. Dropping a `JoinHandle`
/// detaches the thread rather than waiting on it, so the caller returns as
/// soon as the OS has the thread queued.
/// Test: `spawn_detached_returns_before_work_finishes`.
pub(super) fn spawn_detached<F: FnOnce() + Send + 'static>(work: F) {
    std::thread::spawn(work);
}

/// Synchronous body of [`index_new_worktree_in_background`].
///
/// Why: split out so the guards are testable without a spawned thread and
/// without a live daemon — the registration call itself is a no-op under the
/// test harness (`trusty_common::search_index` refuses daemon writes from a
/// test process, #4255), so every branch below is exercised for real while the
/// operator's registry stays untouched.
/// What: two guards, then registration. (1) the path must exist and be a
/// directory; (2) it must be a git WORKTREE, not a plain clone — see
/// [`is_git_worktree`]. Then delegates to
/// `trusty_common::search_index::ensure_project_indexed_reporting` with
/// `skip_vector: true` and the default `allow_sensitive_path: false`, and maps
/// its report onto this module's outcome: `Confirmed` becomes `Registered`,
/// everything else becomes `RegistrationUnconfirmed`. An empty derived id needs
/// no guard of its own — the shared helper performs the identical check and
/// reports `index_id: None`, which maps to `EmptyIndexId`.
///
/// Opt-in model: this adds no new bypass and no new grant. `POST /indexes`
/// enforces trusty-search's hard denylist (credential dirs, `.env`, OS-temp
/// prefixes) and `allow_sensitive_path: false` keeps every one of those checks
/// armed, exactly as `session_launch::register_project_index` already does for
/// the same directories. What this narrows relative to that caller is the
/// input: only a path trusty-mpm itself just created as a worktree reaches
/// here, never an arbitrary cwd.
/// Test: `skips_missing_path`, `skips_path_that_is_not_a_worktree`,
/// `registers_index_for_real_worktree`, `is_idempotent_for_same_worktree`,
/// `reports_unconfirmed_rather_than_registered_when_the_daemon_never_answered`.
pub fn index_new_worktree(worktree_path: &Path) -> WorktreeIndexOutcome {
    if !worktree_path.is_dir() {
        warn!(
            worktree = %worktree_path.display(),
            "worktree index skipped: path does not exist or is not a directory"
        );
        return WorktreeIndexOutcome::PathMissing;
    }

    if !is_git_worktree(worktree_path) {
        debug!(
            worktree = %worktree_path.display(),
            "worktree index skipped: not a git worktree"
        );
        return WorktreeIndexOutcome::NotAWorktree;
    }

    // `skip_vector: true` is the whole point (see the module doc): BM25 + KG
    // are built worktree-accurate, embeddings are not built at all. The
    // defaulted `allow_sensitive_path: false` mirrors `register_project_index`
    // (#2914) — a worktree is always a real directory inside a real repo, never
    // a legitimately OS-temp root, so the denylist stays armed.
    let opts = trusty_common::search_index::IndexOptions::default().with_skip_vector(true);
    let report = trusty_common::search_index::ensure_project_indexed_reporting(worktree_path, opts);

    let Some(index_id) = report.index_id else {
        warn!(
            worktree = %worktree_path.display(),
            "worktree index skipped: derived index id is empty"
        );
        return WorktreeIndexOutcome::EmptyIndexId;
    };

    match report.registration {
        trusty_common::search_index::IndexRegistration::Confirmed => {
            WorktreeIndexOutcome::Registered(index_id)
        }
        reason => WorktreeIndexOutcome::RegistrationUnconfirmed { index_id, reason },
    }
}

/// Is `path` the root of a git WORKTREE (as opposed to a plain clone)?
///
/// Why: this module must index worktrees and only worktrees. Registering an
/// index for a plain checkout is the operator's own opt-in decision — worktree
/// creation may not make it on their behalf, and a permissive
/// "is there a `.git` anywhere above me" test would do exactly that for any
/// directory handed to it by mistake.
/// What: a worktree's `.git` is a FILE containing a `gitdir:` pointer, while a
/// primary clone's `.git` is a DIRECTORY. Testing the file-ness of
/// `path/.git` distinguishes the two with one stat and no subprocess. A
/// submodule also uses a `.git` file; that is an accepted (and harmless)
/// overlap, since nothing in trusty-mpm hands a submodule root to this module.
/// Test: `is_git_worktree_matches_git_on_disk_shapes` (worktree `.git` file =>
/// true, clone `.git` directory => false), `skips_path_that_is_not_a_worktree`
/// and `skips_plain_directory` cover the two refusal paths as reached through
/// `index_new_worktree`.
pub fn is_git_worktree(path: &Path) -> bool {
    path.join(".git").is_file()
}

#[cfg(test)]
#[path = "worktree_index_tests.rs"]
mod tests;
