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
/// What: `Registered` carries the derived index id; the other variants name
/// the specific reason nothing was registered.
/// Test: `skips_path_that_is_not_a_worktree`, `skips_missing_path`,
/// `registers_index_for_real_worktree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeIndexOutcome {
    /// An index was registered (or found already registered) under this id.
    Registered(String),
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
/// logged and swallowed. A failure IS discoverable: a worktree whose index
/// never materialised fails `tm doctor`'s existing `search` check, which
/// probes the index id derived from the current project directory (see
/// `daemon::doctor::check_search` / `expected_search_index_id`). #5045 owns
/// widening that check to sweep every live worktree; this module deliberately
/// adds no second doctor check.
/// Test: `background_entry_point_routes_through_spawn_detached`; the
/// never-block guarantee itself is pinned by
/// `spawn_detached_returns_before_work_finishes`, and the decision logic by
/// the [`index_new_worktree`] tests.
pub fn index_new_worktree_in_background(worktree_path: PathBuf) {
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
fn spawn_detached<F: FnOnce() + Send + 'static>(work: F) {
    std::thread::spawn(work);
}

/// Synchronous body of [`index_new_worktree_in_background`].
///
/// Why: split out so the guards are testable without a spawned thread and
/// without a live daemon — the registration call itself is a no-op under the
/// test harness (`trusty_common::search_index` refuses daemon writes from a
/// test process, #4255), so every branch below is exercised for real while the
/// operator's registry stays untouched.
/// What: three guards, then registration. (1) the path must exist and be a
/// directory; (2) it must be a git WORKTREE, not a plain clone — see
/// [`is_git_worktree`]; (3) the derived index id must be non-empty. On
/// success, delegates to `trusty_common::search_index::ensure_project_indexed_with`
/// with `skip_vector: true` and `allow_sensitive_path: false`.
///
/// Opt-in model: this adds no new bypass and no new grant. `POST /indexes`
/// enforces trusty-search's hard denylist (credential dirs, `.env`, OS-temp
/// prefixes) and `allow_sensitive_path: false` keeps every one of those checks
/// armed, exactly as `session_launch::register_project_index` already does for
/// the same directories. What this narrows relative to that caller is the
/// input: only a path trusty-mpm itself just created as a worktree reaches
/// here, never an arbitrary cwd.
/// Test: `skips_missing_path`, `skips_path_that_is_not_a_worktree`,
/// `registers_index_for_real_worktree`, `is_idempotent_for_same_worktree`.
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

    let index_id = trusty_common::derive_index_id(worktree_path);
    if index_id.trim().is_empty() {
        warn!(
            worktree = %worktree_path.display(),
            "worktree index skipped: derived index id is empty"
        );
        return WorktreeIndexOutcome::EmptyIndexId;
    }

    // `skip_vector: true` is the whole point (see the module doc): BM25 + KG
    // are built worktree-accurate, embeddings are not built at all.
    // `allow_sensitive_path: false` mirrors `register_project_index` (#2914) —
    // a worktree is always a real directory inside a real repo, never a
    // legitimately OS-temp root, so the denylist stays armed.
    let opts = trusty_common::search_index::IndexOptions {
        allow_sensitive_path: false,
        skip_vector: true,
    };
    match trusty_common::search_index::ensure_project_indexed_with(worktree_path, opts) {
        Some(id) => WorktreeIndexOutcome::Registered(id),
        None => WorktreeIndexOutcome::EmptyIndexId,
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
/// Test: `is_git_worktree_true_for_worktree`, `is_git_worktree_false_for_clone`,
/// `is_git_worktree_false_for_plain_dir`.
pub fn is_git_worktree(path: &Path) -> bool {
    path.join(".git").is_file()
}

#[cfg(test)]
#[path = "worktree_index_tests.rs"]
mod tests;
