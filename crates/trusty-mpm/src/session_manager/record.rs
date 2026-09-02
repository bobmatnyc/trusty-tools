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
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

use super::injection_status::InjectionStatus;

/// The literal path a record carries when its working directory could not be
/// resolved at all.
///
/// Why: this string was spelled out four times — `adopt::derive_source_id`,
/// `reconcile`'s known-workspace set, `dedup::is_resolved_existing`, and (since
/// #6118) [`SessionRecord::workspace_unresolvable`] — and every one of them is
/// deciding the same question about the same value. A fifth spelling is how the
/// four drift apart, so they now read one constant.
/// What: `/unknown`, the value the pre-#6126 external-adopt path stubbed into
/// `cwd`/`workspace_path` when `get_pane_cwd` returned nothing. It is not a real
/// path and is never created on disk.
/// Test: `unresolvable_filter_selects_a_live_ghost_pane`,
/// `is_resolved_existing_false_for_none_and_unknown_sentinel`.
pub const UNRESOLVED_PATH_SENTINEL: &str = "/unknown";

/// The `task` an AUTOMATIC adoption writes into the record it mints (#6116).
///
/// Why: it is the only field distinguishing a session the daemon ADOPTED from
/// one it CREATED, and #6116's tombstone arm must act on the first and never on
/// the second — a project legitimately named `xtest-…` derives a name in the
/// reserved namespace, and its own sessions stay ordinary resumable sessions.
/// What: `adopted session`, written by
/// [`super::SessionManager::reconcile_on_boot`]'s external-adopt loop.
/// [`SessionRecord::is_leaked_test_adoption`] matches it as a PREFIX, because
/// `SessionManager::mark_errored` appends `[error: …]` to whatever task a
/// record already carries.
/// Test: `leaked_test_adoption_requires_both_a_reserved_name_and_an_adopted_task`
/// in `record_tests.rs`.
pub const ADOPTED_TASK: &str = "adopted session";

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

    /// Derive the id boot reconciliation adopts a live tmux pane under (#6117).
    ///
    /// Why: the external-adopt loop used to mint `new()` for every pane its
    /// snapshot of the store did not name. That snapshot is taken once, before
    /// the loop, and a second daemon (or a second pass racing the first) holds
    /// its own — so two adopters of one pane each saw the name as unknown and
    /// each wrote a record under a fresh random id. The store is keyed by id,
    /// so both survived: 11 tmux names carried two records apiece in the
    /// reporting store, several pairs written 30-60 ms apart. Deriving the id
    /// from the tmux name instead makes the second write land on the SAME key
    /// as the first, so the store collapses the pair to one record without any
    /// cross-process locking. Re-adoption is then idempotent by construction,
    /// not by whichever snapshot happened to be fresh.
    /// What: UUIDv5 of `name` under a fixed namespace — same name, same id,
    /// forever, on every machine. Random `new()` stays the constructor for
    /// every path that mints a genuinely new session.
    /// Test: `adopted_id_is_stable_for_one_tmux_name`,
    /// `adopted_id_differs_per_tmux_name`,
    /// `adopting_the_same_pane_twice_writes_one_record` in `naming_tests.rs`.
    pub fn for_adopted_tmux_name(name: &str) -> Self {
        /// Fixed UUIDv5 namespace for reconciliation-adopted tmux panes.
        /// Never change it: the value IS the identity of every already-adopted
        /// record, so a new namespace re-mints them all as duplicates.
        const ADOPTED_PANE_NAMESPACE: Uuid =
            Uuid::from_u128(0xe23a_84ff_432c_4007_a690_e7d4_43ba_2050);
        Self(Uuid::new_v5(&ADOPTED_PANE_NAMESPACE, name.as_bytes()))
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
    /// inverse of `Display`/serde `rename_all = "snake_case"`.
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

/// Why a session's runtime stopped, recorded at the moment it stopped (#6194).
///
/// Why: [`ManagedSessionState::Stopped`] answers "is the runtime running" and
/// nothing else, so the two ways a session reaches it are indistinguishable
/// afterwards — somebody ended it on purpose, or it died with nothing asking.
/// The automatic resume paths need that difference: relaunching a session whose
/// runtime crashed is the point of `auto_resume`, and relaunching one the
/// operator just killed undoes their decision. Before this enum, `tmux
/// kill-session` and `tm session stop` were both undone within one supervisor
/// interval, and only `tm session decommission` — a terminal, workspace-deleting
/// state — made a session stay down.
/// What: two variants written by the transitions into `Stopped`, never inferred
/// later. Deliberate is written by [`super::SessionManager::stop`], the one path
/// every "end this session" request reaches — `tm session stop`, the HTTP and
/// MCP stop routes, and the idle auto-stop — and by the tmux-gone reaper when
/// the tmux server is still up, which makes a missing session someone's
/// decision. Unexpected covers everything that cannot be attributed to a
/// decision: [`super::SessionManager::mark_runtime_exited_stopped`] (the runtime
/// exited under a pane that is still alive), boot reconciliation (the daemon
/// was down and cannot know what happened), and the same reaper when the whole
/// tmux server is gone rather than one session — see
/// [`super::SessionManager::stop_with_cause`] for why the reaper's two cases
/// differ.
/// `None` on a record means no transition into `Stopped` has been recorded —
/// every pre-#6194 record loads that way, and reads as auto-resumable, which is
/// exactly the behavior those records had before this field existed.
/// Test: `stop_records_deliberate_cause`, `runtime_exit_records_unexpected_cause`,
/// `legacy_record_without_stop_cause_deserializes_as_auto_resumable` in
/// `stop_cause_tests.rs`; `reap_marks_a_targeted_kill_deliberate`,
/// `reap_leaves_a_whole_server_loss_auto_resumable` in `daemon::state`'s tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    /// Someone ended this session on purpose — an explicit stop request, or
    /// this session's tmux target killed while the tmux server kept running.
    Deliberate,
    /// Nothing asked for the stop: the runtime exited on its own, the tmux
    /// server itself went away, or the daemon restarted and found the session's
    /// tmux target already gone.
    Unexpected,
    /// The runtime kept exiting within the flap window of its own auto-resume,
    /// enough times in a row that auto-resume is now PARKED (#6568).
    ///
    /// Why: `Unexpected` says "relaunch this", and for seven sessions the
    /// relaunch succeeded and the runtime was gone again ~60 seconds later —
    /// 2,170 stops against 2,128 resumes in 48 hours, forever. A repeatedly
    /// SUCCEEDING resume that restores nothing is not a transient failure, so it
    /// needs a cause of its own rather than a retry budget layered onto
    /// `Unexpected`.
    /// What: written only by
    /// [`super::SessionManager::mark_runtime_exited_stopped`], only when
    /// [`super::resume_breaker::evaluate`] returns
    /// [`super::resume_breaker::BreakerVerdict::Park`]. Read back by
    /// [`SessionRecord::is_auto_resumable`] (false) and
    /// [`SessionRecord::auto_resume_park_reason`] (the operator-facing string).
    /// An operator's own `tm session resume` clears it like any other cause.
    ResumeFlapping,
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
    /// deserialize to [`RuntimeKind::ClaudeCode`](crate::runtime::RuntimeKind::ClaudeCode) — the pre-#1203 behavior — so
    /// old sessions resume on Claude Code exactly as before.
    #[serde(default)]
    pub runtime: crate::runtime::RuntimeKind,
    /// Whether this session is EPHEMERAL — a test / throwaway session that the
    /// bulk-teardown and age-based auto-reap paths may decommission automatically.
    ///
    /// Why (#1508): the store was monotonically append-only and accumulated 239
    /// stale TEST sessions because there was no way to mark a session as
    /// throwaway and no bulk teardown. Tagging a session at creation lets
    /// [`SessionManager::decommission_all_ephemeral`](crate::session_manager::SessionManager::decommission_all_ephemeral) and the age-based reaper
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

    /// When this record ENTERED a terminal state (`Decommissioned`/`Deleted`).
    ///
    /// Why: the record-retention sweep
    /// ([`super::retention`](super::retention)) evicts terminal records once
    /// they age out, and needs the moment the record died — not
    /// [`created_at`](Self::created_at), which says when the session was born
    /// and is routinely months earlier. A session created in January,
    /// decommissioned today, is a one-day-old tombstone, and a retention clock
    /// keyed off `created_at` would delete it immediately.
    ///
    /// Written at exactly one place — [`Self::set_lifecycle_state`] — which
    /// every transition INTO a terminal state goes through. That is the
    /// enforced half, and the one the retention sweep depends on.
    ///
    /// Leaving a terminal state is NOT enforced. `mark_reactivated`, the only
    /// production path out, routes through the setter and so clears the stamp;
    /// `mark_errored` and `set_workspace` assign a live state directly and
    /// would not. Neither is reachable on a tombstone today, and a stale stamp
    /// on a live record is inert in any case: [`super::retention_verdict`]
    /// short-circuits on `!is_terminal()`, and the next terminal transition
    /// restamps rather than trusting the old value.
    ///
    /// `#[serde(default)]` (→ `None`) keeps every pre-retention record
    /// deserializable. `None` on a terminal record is backfilled on the next
    /// sweep from [`super::retention::inferred_terminal_at`] — the record's
    /// latest evidence of life — not from the current time, which would
    /// grandfather the entire pre-existing backlog for another full window.
    #[serde(default)]
    pub terminal_at: Option<DateTime<Utc>>,

    /// Why this session's runtime stopped, when it did (#6194).
    ///
    /// Why: the automatic resume paths must not undo a stop somebody asked for.
    /// See [`StopCause`] for the full rationale and for which transition writes
    /// which variant; [`Self::is_auto_resumable`] is the one place it is read.
    ///
    /// `#[serde(default)]` (→ `None`) keeps every pre-#6194 record
    /// deserializable, and `None` reads as auto-resumable — the behavior those
    /// records already had. A record is not migrated on load: the next
    /// transition into `Stopped` writes the cause, and a record already sitting
    /// in `Stopped` from before the fix stays auto-resumable until then.
    #[serde(default)]
    pub stop_cause: Option<StopCause>,
}

impl SessionRecord {
    /// Whether an AUTOMATIC resume may relaunch this session (#6194).
    ///
    /// Why: the two unattended relaunch paths — the supervisor's per-tick sweep
    /// ([`crate::supervisor::poller::run_tick`]) and the auto-resume tail of
    /// [`super::SessionManager::reconcile_on_boot`] — both used to act on state
    /// alone, so every `Stopped` record was a relaunch candidate. That respawned
    /// sessions an operator had just killed: `tmux kill-session` on a leaked
    /// pane, and `tm session stop`, were each undone within one interval, and
    /// only `tm session decommission` stopped the loop. This predicate is the
    /// single place both paths ask the question, so the two cannot drift.
    /// What: `true` only for a `Stopped` record whose stop was not
    /// [`StopCause::Deliberate`] and which is not a leaked test adoption
    /// ([`Self::is_leaked_test_adoption`], #6116). Every other state is `false`, including
    /// `Errored` — neither caller relaunches those today, and this predicate
    /// answers "may an automatic path relaunch it", not "may it be resumed":
    /// an operator's own `tm session resume` is unaffected by this and still
    /// revives a deliberately-stopped session.
    /// Test: `only_a_stop_nobody_asked_for_is_auto_resumable` (the full state ×
    /// cause matrix) and
    /// `legacy_record_without_stop_cause_deserializes_as_auto_resumable` in
    /// `stop_cause_tests.rs`; the #6116 clause by
    /// `a_stopped_leaked_test_adoption_is_never_auto_resumable` in
    /// `record_tests.rs`; the two automatic callers by
    /// `tick_never_resumes_a_deliberately_stopped_session` in
    /// `supervisor::tests` and
    /// `boot_reconcile_never_requeues_a_deliberately_stopped_session` in
    /// `stop_cause_tests.rs`.
    pub fn is_auto_resumable(&self) -> bool {
        matches!(self.state, ManagedSessionState::Stopped)
            // #6568: `ResumeFlapping` joins `Deliberate` here — a session whose
            // runtime died within the flap window of its own auto-resume K
            // times in a row is parked, and an automatic path must leave it
            // down until an operator resumes it by hand.
            && !matches!(
                self.stop_cause,
                Some(StopCause::Deliberate) | Some(StopCause::ResumeFlapping)
            )
            // #6116: never relaunch a test's session. The reaper stamps a
            // gone-tmux record `Unexpected` when other sessions are live, which
            // is auto-resumable — so without this the supervisor RECREATES the
            // leaked pane, reconcile re-adopts it, and the loop sustains itself
            // (observed three times in one day's daemon log).
            && !self.is_leaked_test_adoption()
    }

    /// The operator-facing reason auto-resume is parked, if it is (#6568).
    ///
    /// Why: "the supervisor stopped resuming this" must be readable somewhere an
    /// operator looks, not inferable only from the absence of resume lines in a
    /// 3GB log. This is the one place the wording lives, so the wire summary and
    /// any future surface cannot describe the same state two ways.
    /// What: `Some(<reason>)` exactly when `stop_cause` is
    /// [`StopCause::ResumeFlapping`]; `None` for every other record, including a
    /// deliberately stopped one — that is an operator's own decision, not a
    /// circuit-breaker trip.
    /// Test: `park_reason_is_set_only_for_a_flapping_record` in
    /// `resume_breaker_tests.rs`.
    pub fn auto_resume_park_reason(&self) -> Option<&'static str> {
        match self.stop_cause {
            Some(StopCause::ResumeFlapping) => Some(
                "auto-resume parked: the runtime kept exiting within seconds of \
                 each auto-resume (#6568). Resume it by hand once the cause is \
                 fixed — `tm session resume <id>` clears the park.",
            ),
            _ => None,
        }
    }

    /// Whether this record is an AUTOMATIC adoption of a session in the
    /// test-owned namespace (#6116).
    ///
    /// Why: the one predicate every #6116 decision asks, so the boot
    /// reconciler's tombstone arm and [`Self::is_auto_resumable`] cannot
    /// disagree about which records are leaked test sessions. It deliberately
    /// asks TWO questions: a reserved NAME alone would also match a session the
    /// daemon created for a project legitimately named `xtest-…`, and that
    /// session is real work — tombstoning or refusing to resume it would be the
    /// defect, not the fix.
    /// What: the tmux name is in
    /// [`trusty_common::session_naming::RESERVED_TEST_PREFIX`] AND the task
    /// begins with [`ADOPTED_TASK`], which only an automatic adoption writes.
    /// Test: `leaked_test_adoption_requires_both_a_reserved_name_and_an_adopted_task`
    /// (the adopted / created / ordinary-adoption matrix) and
    /// `a_stopped_leaked_test_adoption_is_never_auto_resumable` in
    /// `record_tests.rs`;
    /// `a_live_leaked_test_pane_is_never_readopted_or_recreated` in
    /// `naming_tests.rs`.
    pub fn is_leaked_test_adoption(&self) -> bool {
        trusty_common::session_naming::is_reserved_test_session_name(&self.tmux_name)
            && self.task.starts_with(ADOPTED_TASK)
    }

    /// Set this record's lifecycle state, keeping [`Self::terminal_at`]
    /// consistent with it.
    ///
    /// Why: the single mutation point for `terminal_at`. Four call sites cross
    /// the terminal boundary — `decommission`'s full teardown and its
    /// record-only variant, `delete`'s soft-delete, and `mark_reactivated`'s
    /// `Decommissioned -> Active` revival. Each assigning `state` by hand left
    /// the retention clock one forgotten assignment away from never starting in
    /// one direction, and from surviving a revival in the other. Only the
    /// entering half is enforced — see [`Self::terminal_at`] for what that does
    /// and does not promise about live-state assignments elsewhere.
    /// What: sets `state` and, when `state` is terminal, sets `terminal_at` to
    /// `now`. Re-entering a terminal state the record already holds does NOT
    /// refresh the stamp — a repeated decommission must not extend the
    /// retention window. A non-terminal `state` clears the stamp, so a record
    /// resurrected out of a tombstone does not carry a stale death time.
    /// Test: `set_lifecycle_state_stamps_once`,
    /// `set_lifecycle_state_clears_stamp_on_revival` in `record_tests.rs`;
    /// the live revival path by `mark_reactivated_clears_the_terminal_stamp`
    /// in `reactivate_tests.rs`.
    pub fn set_lifecycle_state(&mut self, state: ManagedSessionState, now: DateTime<Utc>) {
        let was_terminal = self.state.is_terminal();
        self.state = state;
        if !self.state.is_terminal() {
            self.terminal_at = None;
        } else if !was_terminal || self.terminal_at.is_none() {
            self.terminal_at = Some(now);
        }
    }

    /// Whether this record names no workspace a caller could ever resume,
    /// attach to, or reason about (#6118).
    ///
    /// Why: the pre-#6126 external-adopt path minted a record for every
    /// unrecognised tmux pane, and stubbed `cwd` to
    /// [`UNRESOLVED_PATH_SENTINEL`] with `workspace_path` unset whenever
    /// `get_pane_cwd` came back empty. #6126 stopped MINTING those, but nothing
    /// could SELECT the ones already in the store: 23 of them sat `Active` on
    /// the reporting host, and every existing selector missed them — `--state`
    /// has no variant for them, `prune-idle` keys on an activity verdict, and
    /// the `tm ls` auto-prune requires a dead pane. This predicate is the
    /// missing question, asked once so the prune engine and its tests agree on
    /// the answer.
    ///
    /// 🔴 It is deliberately a RECORD-SHAPE test with no filesystem probe. A
    /// record naming a real directory on an unmounted volume is NOT unresolvable
    /// by this rule, which is what keeps the selector out of the mass-tombstone
    /// hazard `session_picker_prune::workspace_verified_gone` documents at
    /// length: `try_exists` answers `Ok(false)` for every path on an unplugged
    /// drive, so any probe-based widening would select a whole volume's sessions
    /// at once. The sentinel cannot be produced that way — only by an adopt that
    /// already failed to resolve anything.
    ///
    /// A healthy session therefore never satisfies this, whatever its state or
    /// liveness: it was spawned with a real `cwd`, and a provisioned one also
    /// carries a `workspace_path`.
    /// What: `true` only when BOTH coordinates are unresolvable — `cwd` is the
    /// sentinel, AND `workspace_path` is absent or is itself the sentinel. One
    /// resolvable coordinate is enough to keep the record.
    /// Test: `unresolvable_filter_selects_a_live_ghost_pane`,
    /// `healthy_active_session_is_never_selected_by_the_unresolvable_filter`,
    /// `unresolvable_filter_keeps_a_record_that_still_names_a_workspace`.
    pub fn workspace_unresolvable(&self) -> bool {
        let sentinel = Path::new(UNRESOLVED_PATH_SENTINEL);
        let cwd_unresolvable = self.cwd == sentinel;
        let workspace_unresolvable = match &self.workspace_path {
            None => true,
            Some(p) => p == sentinel,
        };
        cwd_unresolvable && workspace_unresolvable
    }
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
