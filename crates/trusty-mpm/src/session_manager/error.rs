//! The session manager's error type (#3764 SLOC split).
//!
//! Why: [`ManagedError`] is a large, heavily-documented enum — its variant
//! docs alone run well over a hundred lines. Adding the #3764
//! [`ManagedError::ForeignActiveWorktree`] corruption-guard variant pushed
//! `manager.rs` past the 500-SLOC production cap, so the enum moves out to its
//! own module exactly as [`super::driver::ManagedTmuxDriver`] did under #1955.
//! No behaviour changes: the type is re-exported from `manager` so every
//! existing `super::manager::ManagedError` import path keeps resolving.
//! What: the [`ManagedError`] enum and nothing else.
//! Test: variants are exercised throughout the session-manager unit tests;
//! the #3764 variant specifically by
//! `decommission_refuses_to_delete_live_peer_worktree` in
//! `super::worktree_identity_guard_tests`.

use thiserror::Error;

use crate::core::names::SessionNameError;

use super::record::ManagedSessionId;
use super::store::StoreError;

/// Errors produced by the session manager.
///
/// Why: HTTP handlers dispatch on error variants to choose status codes;
/// a typed enum keeps that mapping clean and avoids stringly-typed matching.
/// What: one variant per failure mode: tmux problems, missing sessions,
/// store I/O, miscellaneous I/O errors, and invalid state transitions.
/// Test: `ManagedError` variants are exercised by the manager unit tests.
#[derive(Debug, Error)]
pub enum ManagedError {
    /// tmux was unavailable or a tmux operation failed.
    #[error("tmux error: {0}")]
    TmuxUnavailable(String),

    /// The requested session id was not present in the store.
    #[error("session not found: {0}")]
    SessionNotFound(String),

    /// The session store operation failed.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// A miscellaneous I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A name derived from the cwd hint collided with an existing session.
    #[error("name already in use: {0} — use `tm session ls` to find it")]
    NameCollision(String),

    /// The operation is not valid for the current session state.
    #[error("invalid state transition for session {0}: {1}")]
    InvalidState(String, String),

    /// Adoption was requested for a tmux session that does not exist on the host.
    ///
    /// Why: adoption CONNECTS to a pre-existing, unmanaged pane — there is nothing
    /// to drive if the pane is absent. This is the inverse of [`NameCollision`]:
    /// `create` fails when a name exists, `adopt_existing` fails when it does NOT.
    #[error("tmux session does not exist: {0} — adoption requires a live pane")]
    TmuxSessionMissing(String),

    /// Adoption was requested for a tmux session this store already tracks.
    ///
    /// Why: re-adopting a session the manager already owns would create a second,
    /// conflicting record for the same pane. The operator should drive the existing
    /// record instead.
    #[error("tmux session already adopted/registered: {0}")]
    AlreadyAdopted(String),

    /// A session-name derivation failure (currently: all 99 `tm-<leaf>-NN`
    /// serials for a project are in use).
    ///
    /// Why (#1955, renamed in the #1966 review follow-up): the serial-numbered
    /// naming scheme caps at two digits per project; this surfaces
    /// [`SessionNameError`] through the same typed-error seam as every other
    /// create-path failure instead of stringly-typed-wrapping it into
    /// [`TmuxUnavailable`](Self::TmuxUnavailable). Named `SessionName` (not
    /// `NameSerialExhausted`, its original name) and given a generic message
    /// ("session name error", not "serial exhausted") because of the `#[from]`
    /// below: [`SessionNameError`] currently has exactly one variant
    /// ([`SessionNameError::SerialExhausted`]), but `#[from]` auto-converts
    /// ANY future variant into this one — a name/message naming one specific
    /// variant would silently mislabel a later, unrelated `SessionNameError`
    /// variant.
    #[error("session name error: {0}")]
    SessionName(#[from] SessionNameError),

    /// No fallback candidate for a session's workdir exists on disk during
    /// `resume` (#2250).
    ///
    /// Why: prior to #2250, `resume()`'s recreate branch handed
    /// `workspace_path` straight to tmux with no existence check — a
    /// removed/stale worktree silently rooted the recreated pane at `$HOME`,
    /// discarding the project-tier `.claude/` skills/persona/MCP config that
    /// lives only under the real workspace. All three fallback candidates
    /// (`last_cwd`, `workspace_path`, `cwd`) are now existence-checked by
    /// [`super::resume_workdir::resolve_existing_workdir`]; when NONE exist,
    /// failing loudly here beats silently spawning a pane at `$HOME`.
    /// What: `(session_id, path)` — `path` is the most-informative candidate
    /// considered (`workspace_path` if set, else `cwd`), surfaced in the error
    /// message so the operator knows exactly which directory vanished.
    #[error(
        "workspace directory {1} no longer exists; cannot resume session {0} — the worktree may have been removed"
    )]
    WorkspaceMissing(String, String),

    /// The session's recorded `pane_id` no longer exists, though its tmux
    /// SESSION is still alive (sibling-window hijack, follow-up to #2456).
    ///
    /// Why: `resume`'s reuse branch previously trusted `session_exists` alone
    /// to mean "the recorded pane survived" — but a tmux session stays alive
    /// as long as ANY pane/window in it is open, including a sibling that has
    /// nothing to do with this record. Recreating in that situation via the
    /// existing `kill_session` + `create_and_verify_pane` path would destroy
    /// the sibling too; silently respawning via a session-scoped target would
    /// land the runtime in the sibling instead of failing. Neither is safe,
    /// so this variant surfaces the ambiguity to the operator explicitly
    /// rather than guessing.
    /// What: `(session_id, pane_id)` — the missing pane's id, surfaced in the
    /// error message so the operator knows exactly what vanished.
    #[error(
        "recorded pane {1} for session {0} no longer exists, but its tmux session is still \
         alive (likely a sibling window) — refusing to respawn into an unrelated active pane; \
         close the sibling window and delete/recreate this session, or manually verify pane \
         state with `tmux list-panes`"
    )]
    PaneGone(String, String),

    /// A `caller`-identified decommission refused because the target's
    /// worktree has a KNOWN, non-ownerless owner that disagrees with the
    /// caller (#3649). `(caller, owner, target)`.
    #[error("session {0} refused to decommission {2}'s worktree — owned by session {1}")]
    WorktreeOwnerMismatch(ManagedSessionId, ManagedSessionId, ManagedSessionId),

    /// A decommission refused because the worktree ON DISK declares a
    /// DIFFERENT, still-Active owner than the record being torn down (#3764).
    ///
    /// Why: distinct from [`Self::WorktreeOwnerMismatch`], which is the #3649
    /// *authority* gate ("you, session X, may not tear down session Y's
    /// worktree") and only fires for a self-identified `caller`. THIS variant
    /// is the *corruption* gate: the record's own `workspace_path` points at a
    /// live peer's worktree, so the record itself is wrong — the removal is
    /// refused regardless of who asked, including operator/daemon-internal
    /// authority. It is returned rather than silently skipped because the
    /// tombstone path clears `workspace_path`, which would destroy the only
    /// remaining evidence of the mis-pointing record.
    /// What: `(target, owner, path)` — the record asked about, the live owner
    /// the worktree's on-disk sentinel names, and the contested path. No files
    /// are touched when this is returned; both records need investigation
    /// before any retry.
    #[error(
        "refusing to remove worktree {2} while decommissioning session {0}: its on-disk sentinel names session {1}, still ACTIVE — this record points at a LIVE peer's worktree (#3764)"
    )]
    ForeignActiveWorktree(ManagedSessionId, ManagedSessionId, String),
}
