//! Session record types for the managed session-manager.
//!
//! Why: the session manager needs a canonical, serializable representation of
//! every managed session so that state can survive daemon restarts and be
//! exchanged between components without ambiguity.
//! What: defines [`ManagedSessionId`] (a UUID newtype), [`ManagedSessionState`]
//! (the session lifecycle FSM), and [`SessionRecord`] (the full record persisted
//! to disk and returned over the API).
//! Test: serde round-trips are verified in `record_serde_round_trip`; lifecycle
//! variant names are tested in `state_display`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

use super::injection_status::InjectionStatus;

/// Opaque identifier for a managed session.
///
/// Why: a newtype over [`Uuid`] prevents accidental confusion with other
/// UUID-typed identifiers (e.g. `SessionId` in the core module) at the
/// type level rather than relying on naming conventions.
/// What: wraps `uuid::Uuid`; implements `Display`, `Debug`, and
/// serde derive for transparent JSON/TOML serialization.
/// Test: `managed_session_id_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ManagedSessionId(pub Uuid);

impl ManagedSessionId {
    /// Generate a new random managed session id.
    ///
    /// Why: all new sessions need a stable, globally unique identifier
    /// assigned at creation time.
    /// What: wraps `Uuid::new_v4()`.
    /// Test: used throughout manager tests.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Return the inner UUID value.
    ///
    /// Why: some callers (e.g. name derivation via `name_from_uuid`) need the
    /// raw UUID without the newtype wrapper.
    /// What: extracts the inner `Uuid`.
    /// Test: `managed_session_id_round_trip`.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for ManagedSessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ManagedSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for ManagedSessionId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

/// Lifecycle state of a managed session.
///
/// Why: a session ENDURES from provisioning until explicit decommissioning —
/// the running `claude` process is transient inside an enduring session.
/// The state machine captures where in the lifecycle a session currently sits
/// so operators and reconciliation logic can make informed decisions.
///
/// FSM: `Provisioning` → `Active` ⇄ `Stopped` / `Errored` → `Decommissioned`.
///
/// Key invariant: `Stopped` means the RUNTIME is not running but the workspace
/// directory and record are INTACT and RESUMABLE. Only `Decommissioned` means
/// the workspace has been removed from disk.
///
/// What: six variants covering the full lifecycle from first provisioning
/// through active use, voluntary/involuntary runtime stop, resume, final
/// teardown (`Decommissioned`), and explicit operator deletion (`Deleted`).
/// `Dead`/`Orphaned`/`Idle`/`Adopted` are intentionally absent —
/// a stopped-or-gone runtime must never read as "session lost".
/// Test: `state_display`, serde round-trips in `record_serde_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSessionState {
    /// Workspace is being provisioned; tmux session and runtime not yet started.
    Provisioning,
    /// Workspace provisioned, tmux session created, runtime (claude) is running.
    Active,
    /// Runtime is NOT running; workspace directory and record INTACT and RESUMABLE.
    ///
    /// Entered when: (a) the operator calls `stop`, (b) the runtime exits on its
    /// own, or (c) the daemon restarts and finds no live tmux session for a
    /// previously-active record (post-reboot reconciliation).
    Stopped,
    /// Provisioning or runtime spawn failed; record preserved for post-mortem.
    ///
    /// Resumable after the operator fixes the underlying issue and calls `resume`.
    Errored,
    /// Terminal state: workspace removed from disk; only a tombstone record remains.
    ///
    /// Entered when the operator calls `decommission`. No resume is possible.
    Decommissioned,
    /// Terminal state: the operator explicitly DELETED the record.
    ///
    /// Why: `tm sessions delete` marks the record `Deleted` (rendered
    /// `--deleted--` in the master list) instead of silently dropping it from
    /// the store, preserving the "fully-tracked lifecycle, no fire-and-forget"
    /// standard — a deleted session is still visible so the operator sees what
    /// happened. Distinct from `Decommissioned` (which is the workspace-teardown
    /// terminal state). Permanent removal from the store happens via
    /// `tm sessions prune --state deleted` (or `--state all`). No resume is
    /// possible.
    Deleted,
}

impl ManagedSessionState {
    /// Whether this is a TERMINAL tombstone state (`Decommissioned` or `Deleted`).
    ///
    /// Why: a terminal record is gone for good — it must never be offered for
    /// resume/attach/restart, and no lifecycle transition (`stop`, zombie
    /// reconcile, …) may move it back to a live state. Centralising the
    /// definition on the enum makes it the SINGLE source of truth, so every
    /// surface (the picker's live-filter, the `stop` guard) agrees rather than
    /// each hand-rolling a `state == "decommissioned"` string check that a new
    /// terminal variant (like `Deleted`) would silently slip past.
    /// What: `true` for [`Decommissioned`](Self::Decommissioned) and
    /// [`Deleted`](Self::Deleted); `false` for every live/resumable state.
    /// Test: `is_terminal_covers_tombstones` in `super::record`'s tests.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Decommissioned | Self::Deleted)
    }

    /// Parse a wire/display token (the snake_case form) back into a state.
    ///
    /// Why: CLI/display surfaces carry the state as a bare string (the daemon
    /// serializes it); this lets them ask the ENUM about terminality (via
    /// [`is_terminal`](Self::is_terminal)) instead of re-hardcoding a token list
    /// that a new terminal variant would silently slip past. The tokens are the
    /// inverse of [`Display`](Self::fmt)/serde `rename_all = "snake_case"`.
    /// What: `Some(state)` for a recognised token, `None` for anything else.
    /// Test: `from_wire_round_trips_every_variant` in `super::record`'s tests.
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "provisioning" => Some(Self::Provisioning),
            "active" => Some(Self::Active),
            "stopped" => Some(Self::Stopped),
            "errored" => Some(Self::Errored),
            "decommissioned" => Some(Self::Decommissioned),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

impl fmt::Display for ManagedSessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Provisioning => "provisioning",
            Self::Active => "active",
            Self::Stopped => "stopped",
            Self::Errored => "errored",
            Self::Decommissioned => "decommissioned",
            Self::Deleted => "deleted",
        };
        write!(f, "{s}")
    }
}

/// Full record for a managed session, persisted to disk.
///
/// Why: persistence enables crash recovery — the manager can reload all known
/// sessions on startup and reconcile them against live tmux state rather than
/// losing track of sessions between restarts. Records survive daemon restarts;
/// decommissioned tombstones too, so `ls` can show history.
/// What: captures every field needed to identify, describe, and operate on a
/// session: its id, tmux name, working directory, human-readable task
/// description, lifecycle state, timestamps, workspace path, git coordinates,
/// and any pending decision fields.
/// Test: `record_serde_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Unique managed session identifier.
    pub id: ManagedSessionId,
    /// tmux session name (e.g. `tm-quiet-falcon` or `tm-trusty-tools-01`).
    pub tmux_name: String,
    /// Working directory the session was started in.
    pub cwd: PathBuf,
    /// Human-readable task description supplied at creation.
    pub task: String,
    /// Current lifecycle state.
    pub state: ManagedSessionState,
    /// When the session record was created.
    pub created_at: DateTime<Utc>,
    /// When the session last showed activity, if ever.
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Isolated workspace path provisioned by the workspace provisioner.
    pub workspace_path: Option<PathBuf>,
    /// Repository URL this session was provisioned from.
    pub repo_url: Option<String>,
    /// Git branch or ref this session was checked out at.
    pub branch: Option<String>,
    /// A pending decision question surfaced by the harness.
    pub pending_decision: Option<String>,
    /// The harness's proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
    /// Session ↔ artifact correlation: links this session to its worktree,
    /// branch, PR, and/or issue so the driver's autonomy policy can validate
    /// that generated work stays in-scope before auto-accepting.
    ///
    /// `#[serde(default)]` keeps records persisted before this field existed
    /// deserializable — they load with an empty (fully-unset) correlation.
    #[serde(default)]
    pub correlation: crate::driver::SessionCorrelation,
    /// Which runtime backend hosts this session's harness.
    ///
    /// Why: the runtime is chosen at spawn time but `resume` must re-spawn the
    /// SAME backend; persisting it on the record keeps the choice authoritative
    /// across daemon restarts and resume cycles.
    ///
    /// `#[serde(default)]` makes records persisted before this field existed
    /// deserialize to [`RuntimeKind::ClaudeCode`] — the pre-#1203 behavior — so
    /// old sessions resume on Claude Code exactly as before.
    #[serde(default)]
    pub runtime: crate::runtime::RuntimeKind,
    /// Whether this session is EPHEMERAL — a test / throwaway session that the
    /// bulk-teardown and age-based auto-reap paths may decommission automatically.
    ///
    /// Why (#1508): the store was monotonically append-only and accumulated 239
    /// stale TEST sessions because there was no way to mark a session as
    /// throwaway and no bulk teardown. Tagging a session at creation lets
    /// [`SessionManager::decommission_all_ephemeral`] and the age-based reaper
    /// target ONLY test sessions — REAL sessions default `false` and so are
    /// unreachable by either automatic path (the core safety invariant).
    ///
    /// `#[serde(default)]` (→ `false`) keeps the 239 legacy records — and every
    /// other pre-#1508 record — deserializable: they load as non-ephemeral, so an
    /// automatic teardown never touches them (the explicit by-state prune is the
    /// tool for purging those legacy tombstones).
    #[serde(default)]
    pub ephemeral: bool,

    /// Whether the SM **provisioned** (cloned/created) the `workspace_path` itself.
    ///
    /// Why (#1511): `decommission` previously `remove_dir_all`'d `workspace_path`
    /// unconditionally, which deleted a real user repository when the #1502
    /// local-path spawn set `workspace_path` to a pre-existing on-disk directory.
    /// This flag marks ownership: `true` ONLY when the SM provisioned the directory
    /// via a git clone (the normal `SpawnParams` + `WorkspaceProvisioner` path);
    /// `false` for local-path spawn (#1502), explicit `adopt_existing` (#1433), and
    /// every legacy record (safe default — prefer NOT deleting over accidental
    /// deletion). The decommission path checks this flag BEFORE calling
    /// `remove_dir_all`, so a local-path or adopted workspace is never deleted.
    ///
    /// `#[serde(default)]` (→ `false`) ensures every pre-#1511 record deserializes
    /// as UNOWNED: legacy records are treated as not-owned → never auto-deleted,
    /// which is the safe direction — a "lost" workspace can be cleaned up manually;
    /// an accidentally deleted live repo cannot be un-deleted.
    #[serde(default)]
    pub workspace_owned: bool,

    /// Opaque identifier linking this session to its source project.
    ///
    /// Why (#1707): the in-project spawn path (#1706) creates sessions that are
    /// associated with a specific GitHub `owner/repo`; recording the source id
    /// (e.g. `"owner/repo"`) lets callers filter the session list by project
    /// and lets `tm` reconnect to an existing session for the same project
    /// instead of spawning a duplicate.
    ///
    /// `#[serde(default)]` keeps records persisted before this field existed
    /// deserializable — they load with `source_id = None`, which is correct:
    /// sessions spawned via the old paths carry no project identity.
    #[serde(default)]
    pub source_id: Option<String>,

    /// The Claude Code internal session UUID captured from the `SessionStart` hook.
    ///
    /// Why (#1744): when a managed session exits ungracefully (terminal closed,
    /// tmux pane killed without `/quit`), resume can restore the prior
    /// conversation by passing `--resume <id>` to the new `claude` process.
    /// This field holds the UUID that Claude Code assigns to its own session,
    /// delivered via the `CLAUDE_SESSION_ID` env var and forwarded through the
    /// `SessionStart` hook. Without it, resume falls back to `--continue`
    /// (most-recent conversation in the workspace) or a fresh launch.
    ///
    /// `#[serde(default)]` keeps records persisted before this field existed
    /// deserializable — they load with `claude_session_id = None`, which is
    /// correct: legacy sessions have no captured id and fall back gracefully.
    #[serde(default)]
    pub claude_session_id: Option<String>,

    /// Path to the scrollback snapshot captured just before the session was stopped.
    ///
    /// Why (#1816): the idle auto-stop feature captures the pane's scrollback
    /// before killing tmux so context is not lost. The file is written to
    /// `<workspace_path>/.trusty-mpm/scrollback.txt`; this field records where
    /// to find it so the resume path (and future tooling) can surface it.
    ///
    /// `#[serde(default)]` (→ `None`) keeps all pre-#1816 records deserializable
    /// with no scrollback path — they resume normally without a snapshot.
    #[serde(default)]
    pub scrollback_path: Option<PathBuf>,

    /// The pane's current working directory captured just before the session was stopped.
    ///
    /// Why (#1816): `resume()` restores the tmux session's working directory to
    /// where the operator left off rather than falling back to the workspace root,
    /// making context restoration more ergonomic.
    ///
    /// `#[serde(default)]` (→ `None`) keeps all pre-#1816 records deserializable —
    /// they resume from `workspace_path` / `cwd` as before.
    #[serde(default)]
    pub last_cwd: Option<PathBuf>,

    /// Which Deliverable this session is working on (DOC-35 §10.6, #2379).
    ///
    /// Why: 1 Deliverable ↔ many Sessions (DOC-30 Decision #7, carried forward
    /// unchanged) — a session works on at most ONE Deliverable at a time;
    /// `None` is the common case for ad-hoc sessions not tracked against a
    /// Deliverable. This is a PURE POINTER: linking a session to a Deliverable
    /// never auto-transitions the Deliverable's status (§11 forbids
    /// auto-transitions — only an explicit `set-status` call, #2380, mutates
    /// it) and decommissioning the session never auto-unlinks the pointer —
    /// a tombstoned session still records which Deliverable it worked on.
    ///
    /// `#[serde(default)]` keeps every pre-#2379 record deserializable — they
    /// load with `deliverable_id = None`, which is correct: no session before
    /// this field existed was ever bound to a Deliverable.
    #[serde(default)]
    pub deliverable_id: Option<crate::deliverable::DeliverableId>,

    /// The tmux `pane_id` (e.g. `"%5"`) of this session's ORIGINAL pane,
    /// captured at spawn time and refreshed whenever the runtime-exit
    /// reconcile confirms the pane is still alive (#2453 review finding 1,
    /// round 2).
    ///
    /// Why: the bare-`tm` in-pane relaunch (`guided::run_guided_default`'s
    /// nested-session guard, #2453) must confirm the OPERATOR'S CURRENT pane
    /// is genuinely the one bound to this record before driving a destructive
    /// `exec` into it — a session-name-only match (every window/pane in a
    /// tmux session shares the session name) is not enough, and a
    /// process-env-var comparison was PROVEN insufficient too: tmux's
    /// session-scoped `set-environment` (used to heal `TM_MANAGED_SESSION_ID`
    /// into a pane that never got the durable publish, #2157 item 3) is
    /// inherited into the process env of every NEW pane/window created in
    /// that tmux session AFTER the healing call — verified empirically
    /// against a live tmux 3.6b. `pane_id` is tmux's own stable per-pane
    /// identifier (distinct from `pane_pid`, which the OS can reuse across a
    /// pane's lifetime); it is NEVER inherited across panes, making it the
    /// only reliable "is this literally the same pane" signal available.
    /// What: `None` for every record created before this field existed, or
    /// whenever the driver could not resolve a pane_id (fails CLOSED — the
    /// nested-session guard treats an absent/mismatched `pane_id` as
    /// "identity unconfirmed" and refuses the in-place relaunch rather than
    /// trusting a weaker signal).
    ///
    /// `#[serde(default)]` (→ `None`) keeps every pre-#2453 record
    /// deserializable — they load with `pane_id = None`, which is correct:
    /// no session before this field existed had one captured. Mirrors the
    /// `deliverable_id` rollback-safety pattern immediately above.
    #[serde(default)]
    pub pane_id: Option<String>,

    /// Delivery status of the turnkey `--task` pane injection (#2364).
    ///
    /// Why: `inject_task_when_ready` was fire-and-forget before this field
    /// existed — see [`InjectionStatus`]'s doc for the full rationale.
    ///
    /// `#[serde(default)]` (→ [`InjectionStatus::NotApplicable`]) keeps every
    /// pre-#2364 record deserializable — they load as "injection never
    /// attempted", which is correct: no session before this field existed
    /// tracked delivery status.
    #[serde(default)]
    pub injection_status: InjectionStatus,

    /// The managed session that owns this record's on-disk worktree, if the
    /// ownership registry established it (#3649, Option B).
    ///
    /// Why: three independent worktree stores exist (`.base/.worktrees/<id>`
    /// clone-based, in-project `<repo>/.worktrees/<name>`, and the
    /// out-of-scope harness `.claude/worktrees/`) and NOTHING previously
    /// recorded who is entitled to reclaim a given worktree. The orphan-GC
    /// sweep and a peer session's `decommission` call both need a fast,
    /// non-filesystem way to ask "does this session own this worktree?"
    /// without re-reading a sentinel file for every check. This field is set
    /// to `Some(self.id)` immediately after a session's workspace is
    /// provisioned via [`super::decommission::WORKTREE_SENTINEL_FILE`]'s
    /// JSON-payload sentinel (`workspace.rs::provision_in`,
    /// `inproject.rs::create_session_worktree`) — see
    /// [`super::SessionManager::set_worktree_owner`].
    ///
    /// `#[serde(default)]` (→ `None`) keeps every pre-#3649 record
    /// deserializable: legacy records load as OWNER-UNKNOWN. This is the safe
    /// direction — an owner-unknown worktree is NEVER auto-reclaimed by the
    /// orphan-GC and NEVER blocks a `caller`-gated decommission (the gate
    /// only refuses when a KNOWN owner disagrees with the caller); it simply
    /// surfaces forever via the existing `prune --dry-run`/doctor flows for
    /// explicit human action (zero-migration by design — see ADR-0020).
    #[serde(default)]
    pub worktree_owner: Option<ManagedSessionId>,
}

/// Error types for session record operations.
///
/// Why: callers that manipulate session records need structured errors they can
/// pattern-match rather than opaque strings.
/// What: one variant per failure mode — malformed data, missing fields, etc.
/// Test: exercised indirectly through `SessionStore` tests.
#[derive(Debug, Error)]
pub enum RecordError {
    /// A required field was absent or invalid during deserialization.
    #[error("invalid session record: {0}")]
    Invalid(String),
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
