//! Register the BASE CHECKOUT behind a worktree with trusty-search (#5069).
//!
//! Why: #5060 registers every new worktree BM25+KG-only (`skip_vector: true`),
//! on the ruling that exact text and the symbol graph are branch-specific while
//! conceptual similarity is not — so the embedding lane is built ONCE, on the
//! base checkout. Nothing built it. `worktree_index` refuses a plain clone by
//! design and session launch only ever registers the directory the session runs
//! in, which under `tm` is always a worktree. The result on this machine: 8
//! worktrees registered, the checkout they were cut from absent from the
//! registry entirely, and no index anywhere carrying vectors for this repo.
//! Every worktree session got BM25 and KG and never semantic search.
//!
//! What: [`ensure_base_facet_indexed`] resolves the primary checkout a worktree
//! was cut from and registers ITS index with the vector lane ON
//! (`skip_vector: false`, the default). Two call sites reach it — worktree
//! creation (`core::worktree_index`) and session launch
//! (`core::session_launch::register_project_index`) — because the ~59 worktrees
//! that already exist never run the creation path again.
//!
//! Eager, not lazy (#2611 open question 1, "base-facet ownership"): the base
//! index is created as a side effect of trusty-mpm cutting a worktree in that
//! repo, not on first semantic query. A lazy path would need a query to fail
//! first, then wait ~35 s plus the embed pass before it could answer — so the
//! first semantic query of every session would still miss.
//!
//! Opt-in model (#767): this adds no new grant. The only paths that reach here
//! are a worktree trusty-mpm itself created and a session trusty-mpm itself
//! launched, both inside a repo the operator is already running `tm` in and
//! already registering indexes for. The target is that same repo's primary
//! checkout, `allow_sensitive_path` stays `false`, and `POST /indexes` enforces
//! the daemon's denylist unchanged.
//!
//! Not silent: like `worktree_index`, every outcome is a typed value plus a log
//! line, so "the base facet was registered" is never inferred from the absence
//! of an error. #5068 made the trusty-search half loud already — a semantic
//! query pinned against a `skip_vector` index answers `503 vector_unavailable`
//! rather than lexical rows under a `200` — but nothing on this side ever
//! observed, or reported, that no base facet existed to route to.
//!
//! Scope: this module creates the target. Routing a worktree's semantic query
//! ONTO it is the other half of #5069 and is not implemented here.
//!
//! Test: `base_facet_index_tests.rs`.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// Why a base checkout was or was not registered.
///
/// Why: the work runs on a detached thread where a return value would be
/// dropped, so each refusal has to be distinguishable in a test rather than
/// only in a log. Same reasoning, and same shape, as
/// [`crate::core::worktree_index::WorktreeIndexOutcome`].
/// What: `Registered` means the daemon CONFIRMED the base checkout's index with
/// a 2xx; every other variant names the specific reason nothing is known to have
/// been registered.
/// Test: `plain_clone_is_not_a_base_facet_source`,
/// `unresolvable_gitdir_pointer_is_reported_not_registered`,
/// `base_facet_targets_the_base_checkout_not_the_worktree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseFacetOutcome {
    /// The daemon confirmed an index under this id for the base checkout.
    Registered(String),
    /// A registration was REQUESTED for the base checkout but never confirmed:
    /// daemon unreachable, non-2xx, or suppressed because this is a test
    /// process. The base facet is NOT known to exist, and nothing may treat
    /// this as success.
    RegistrationUnconfirmed {
        /// The derived index id of the BASE checkout. Reported so a log line
        /// can name it, never so a caller can pin it.
        index_id: String,
        /// Which non-confirmation happened.
        reason: trusty_common::search_index::IndexRegistration,
    },
    /// The path handed in is not a git worktree, so there is no base facet to
    /// derive. A plain clone IS its own base facet and is registered by the
    /// ordinary session-launch path.
    NotAWorktree,
    /// The path is a worktree but its `gitdir:` pointer does not lead to a
    /// primary checkout — a bare main repo, a submodule, or a stale pointer
    /// whose repository has been moved or deleted.
    BaseCheckoutUnresolved,
    /// The base checkout's index id derived to the empty string.
    EmptyIndexId,
}

/// Options the base facet is registered with.
///
/// Why: `skip_vector: false` is the entire point of this module and the one
/// thing a well-meaning "make all trusty-mpm indexes consistent" change would
/// flip. Naming it as a function gives that invariant a test of its own
/// (`base_facet_keeps_the_vector_lane_on`) rather than leaving it implicit in a
/// `default()` call.
/// What: [`trusty_common::search_index::IndexOptions::default`] verbatim —
/// `skip_vector: false` (build embeddings) and `allow_sensitive_path: false`
/// (the daemon's denylist stays armed).
/// Test: `base_facet_keeps_the_vector_lane_on`.
fn base_facet_options() -> trusty_common::search_index::IndexOptions {
    trusty_common::search_index::IndexOptions::default()
}

/// Fire-and-forget: register the base checkout behind `worktree_path`.
///
/// Why: session launch must never wait on indexing. The base checkout is the
/// EXPENSIVE index — a cold BM25+KG walk of this workspace took 35 s before the
/// embed pass even starts — so a synchronous call here would put that in front
/// of a shell prompt.
/// What: spawns a detached OS thread (through `worktree_index`'s
/// [`crate::core::worktree_index::spawn_detached`], the one place the
/// never-block guarantee lives) running [`ensure_base_facet_indexed`] and logs
/// its outcome.
/// Test: the never-block property is pinned on the shared helper by
/// `worktree_index`'s `spawn_detached_returns_before_work_finishes`; the work it
/// runs is covered by the [`ensure_base_facet_indexed`] tests.
pub fn ensure_base_facet_indexed_in_background(worktree_path: PathBuf) {
    crate::core::worktree_index::spawn_detached(move || {
        log_outcome(&worktree_path, ensure_base_facet_indexed(&worktree_path));
    });
}

/// Report a [`BaseFacetOutcome`] at the level its severity earns.
///
/// Why: an unconfirmed registration is the case that used to be invisible, so
/// it warns; the two structural refusals are ordinary and debug.
/// What: `info` for `Registered`, `warn` for `RegistrationUnconfirmed`, `debug`
/// otherwise.
/// Test: each variant it renders is asserted by the
/// [`ensure_base_facet_indexed`] tests.
fn log_outcome(worktree_path: &Path, outcome: BaseFacetOutcome) {
    match outcome {
        BaseFacetOutcome::Registered(id) => info!(
            worktree = %worktree_path.display(),
            index_id = %id,
            "base-facet index registered (vector lane ON) — #5069"
        ),
        BaseFacetOutcome::RegistrationUnconfirmed { index_id, reason } => warn!(
            worktree = %worktree_path.display(),
            index_id = %index_id,
            ?reason,
            "base-facet index REQUESTED but not confirmed — semantic search has no \
             index to route to from this worktree (#5069)"
        ),
        other => debug!(
            worktree = %worktree_path.display(),
            outcome = ?other,
            "base-facet index not registered"
        ),
    }
}

/// Synchronous body of [`ensure_base_facet_indexed_in_background`].
///
/// Why: split out so both guards and the id derivation are testable without a
/// spawned thread and without a live daemon — `trusty_common::search_index`
/// refuses daemon writes from a test process (#4255), so every branch runs for
/// real while the operator's registry stays untouched.
/// What: resolves the base checkout with [`base_checkout_of`], then delegates
/// to `trusty_common::search_index::ensure_project_indexed_reporting` with
/// [`base_facet_options`] and maps the report onto [`BaseFacetOutcome`]. The
/// call is idempotent: `POST /indexes` is find-or-create and the reindex trigger
/// is freshness-gated, so a repeat costs two short HTTP round trips and no walk.
/// Test: `base_facet_targets_the_base_checkout_not_the_worktree`,
/// `plain_clone_is_not_a_base_facet_source`,
/// `unresolvable_gitdir_pointer_is_reported_not_registered`,
/// `base_facet_posts_the_base_checkout_with_the_vector_lane_on`.
pub fn ensure_base_facet_indexed(worktree_path: &Path) -> BaseFacetOutcome {
    if !crate::core::worktree_index::is_git_worktree(worktree_path) {
        return BaseFacetOutcome::NotAWorktree;
    }

    let Some(base) = base_checkout_of(worktree_path) else {
        warn!(
            worktree = %worktree_path.display(),
            "base-facet index skipped: the worktree's gitdir pointer does not lead to a \
             primary checkout"
        );
        return BaseFacetOutcome::BaseCheckoutUnresolved;
    };

    // #5069: the base checkout owns the embedding lane, so `skip_vector` stays
    // at its `false` default here — the opposite of the worktree's own index.
    let report =
        trusty_common::search_index::ensure_project_indexed_reporting(&base, base_facet_options());

    let Some(index_id) = report.index_id else {
        return BaseFacetOutcome::EmptyIndexId;
    };

    match report.registration {
        trusty_common::search_index::IndexRegistration::Confirmed => {
            BaseFacetOutcome::Registered(index_id)
        }
        reason => BaseFacetOutcome::RegistrationUnconfirmed { index_id, reason },
    }
}

/// The primary checkout a git worktree was cut from, if there is one.
///
/// Why: the base facet has to be found without shelling out — this runs on the
/// session-launch path — and without walking the filesystem upward, which would
/// find the wrong repository whenever worktrees are kept outside the checkout.
/// git already records the answer: a worktree's `.git` is a FILE holding
/// `gitdir: <common>/worktrees/<name>`, where `<common>` is the primary
/// checkout's own `.git` directory.
/// What: reads that pointer (resolving a relative one against the worktree, as
/// git writes when `core.relativeWorktrees` is set), strips the
/// `worktrees/<name>` tail, and returns the parent of `<common>` only when that
/// directory is itself a PRIMARY checkout — its `.git` is a directory. That last
/// check is what rejects a bare main repo, a submodule (whose `.git` file points
/// into `modules/`, not `worktrees/`, and is already rejected by the tail
/// check), and a pointer whose repository has been moved or deleted.
/// Test: `resolves_the_base_checkout_of_a_real_worktree`,
/// `unresolvable_gitdir_pointer_is_reported_not_registered`,
/// `gitdir_outside_a_worktrees_directory_resolves_to_nothing`.
pub fn base_checkout_of(worktree_path: &Path) -> Option<PathBuf> {
    let pointer = std::fs::read_to_string(worktree_path.join(".git")).ok()?;
    let raw = pointer
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    if raw.is_empty() {
        return None;
    }

    let gitdir = Path::new(raw);
    let gitdir = if gitdir.is_absolute() {
        gitdir.to_path_buf()
    } else {
        worktree_path.join(gitdir)
    };

    // `<common>/worktrees/<name>` — anything else is not a linked worktree.
    let worktrees_dir = gitdir.parent()?;
    if worktrees_dir.file_name()? != "worktrees" {
        return None;
    }
    let base = worktrees_dir.parent()?.parent()?;

    // A primary checkout's `.git` is a DIRECTORY. A bare repo has no checkout
    // to index, and a stale pointer has none that exists.
    if !base.join(".git").is_dir() {
        return None;
    }
    Some(base.to_path_buf())
}

#[cfg(test)]
#[path = "base_facet_index_tests.rs"]
mod tests;
