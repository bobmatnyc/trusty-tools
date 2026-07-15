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
/// What: five variants covering the full lifecycle from first provisioning
/// through active use, voluntary/involuntary runtime stop, resume, and final
/// teardown. `Dead`/`Orphaned`/`Idle`/`Adopted` are intentionally absent —
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
}

impl fmt::Display for ManagedSessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Provisioning => "provisioning",
            Self::Active => "active",
            Self::Stopped => "stopped",
            Self::Errored => "errored",
            Self::Decommissioned => "decommissioned",
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
mod tests {
    use super::*;

    #[test]
    fn managed_session_id_round_trip() {
        let id = ManagedSessionId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        let back: ManagedSessionId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
        assert_eq!(id.as_uuid(), back.as_uuid());
    }

    #[test]
    fn state_display() {
        assert_eq!(
            ManagedSessionState::Provisioning.to_string(),
            "provisioning"
        );
        assert_eq!(ManagedSessionState::Active.to_string(), "active");
        assert_eq!(ManagedSessionState::Stopped.to_string(), "stopped");
        assert_eq!(ManagedSessionState::Errored.to_string(), "errored");
        assert_eq!(
            ManagedSessionState::Decommissioned.to_string(),
            "decommissioned"
        );
    }

    #[test]
    fn record_serde_round_trip() {
        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-quiet-falcon".into(),
            cwd: PathBuf::from("/tmp/project"),
            task: "implement feature X".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: Some(Utc::now()),
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, record.id);
        assert_eq!(back.tmux_name, record.tmux_name);
        assert_eq!(back.state, record.state);
    }

    #[test]
    fn stopped_state_survives_serde() {
        // Why: reconciliation persists Stopped state; this guards the serde
        // round-trip for the new variant.
        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-test".into(),
            cwd: PathBuf::from("/tmp"),
            task: "task".into(),
            state: ManagedSessionState::Stopped,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: Some(PathBuf::from("/tmp/ws")),
            repo_url: Some("https://github.com/owner/repo".into()),
            branch: Some("main".into()),
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.state, ManagedSessionState::Stopped);
        assert_eq!(back.workspace_path, record.workspace_path);
    }

    #[test]
    fn decommissioned_state_survives_serde() {
        // Why: tombstone records for decommissioned sessions must survive restarts.
        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-gone".into(),
            cwd: PathBuf::from("/tmp"),
            task: "task".into(),
            state: ManagedSessionState::Decommissioned,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None, // removed from disk
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.state, ManagedSessionState::Decommissioned);
        assert!(back.workspace_path.is_none());
    }

    #[test]
    fn record_without_runtime_field_defaults_to_claude_code() {
        // Why: #1203 added `runtime` with `#[serde(default)]`; records persisted
        // before this field existed (no `runtime` key) must still deserialize
        // and resume on the pre-#1203 default (claude-code).
        let legacy_json = serde_json::json!({
            "id": ManagedSessionId::new(),
            "tmux_name": "tmpm-legacy",
            "cwd": "/tmp",
            "task": "legacy task",
            "state": "active",
            "created_at": Utc::now().to_rfc3339(),
            "last_activity_at": null,
            "workspace_path": null,
            "repo_url": null,
            "branch": null,
            "pending_decision": null,
            "proposed_default": null
        })
        .to_string();
        let back: SessionRecord = serde_json::from_str(&legacy_json).expect("deserialize legacy");
        assert_eq!(back.runtime, crate::runtime::RuntimeKind::ClaudeCode);
    }

    #[test]
    fn record_round_trips_tcode_runtime() {
        // Why: a tcode-backed session must persist its runtime so `resume`
        // re-spawns on tcode, not claude-code.
        let mut record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-tcode".into(),
            cwd: PathBuf::from("/tmp"),
            task: "task".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        };
        record.runtime = crate::runtime::RuntimeKind::Tcode;
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.runtime, crate::runtime::RuntimeKind::Tcode);
    }

    #[test]
    fn record_without_ephemeral_field_defaults_to_false() {
        // Why (#1508): the 239 legacy records — and every other pre-#1508 record —
        // have no `ephemeral` key; they MUST deserialize as non-ephemeral so the
        // automatic teardown/auto-reap paths never touch them. This pins the
        // `#[serde(default)]` → false backward-compat contract.
        let legacy_json = serde_json::json!({
            "id": ManagedSessionId::new(),
            "tmux_name": "tmpm-legacy",
            "cwd": "/tmp",
            "task": "legacy task",
            "state": "stopped",
            "created_at": Utc::now().to_rfc3339(),
            "last_activity_at": null,
            "workspace_path": null,
            "repo_url": null,
            "branch": null,
            "pending_decision": null,
            "proposed_default": null
        })
        .to_string();
        let back: SessionRecord = serde_json::from_str(&legacy_json).expect("deserialize legacy");
        assert!(
            !back.ephemeral,
            "a record with no `ephemeral` key must default to false (non-ephemeral)"
        );
    }

    #[test]
    fn record_round_trips_ephemeral_true() {
        // Why (#1508): a session tagged ephemeral at creation must persist the flag
        // so the bulk-teardown + age-based reap paths can later target it.
        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-ephemeral".into(),
            cwd: PathBuf::from("/tmp"),
            task: "throwaway".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: true,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert!(back.ephemeral, "ephemeral=true must round-trip");
    }

    #[test]
    fn record_without_workspace_owned_field_defaults_to_false() {
        // Why (#1511): every pre-#1511 record has no `workspace_owned` key; they
        // MUST deserialize as unowned (false) so the decommission path never
        // auto-deletes a workspace it did not provision. "Prefer not deleting" is
        // the safe direction — a lost workspace can be cleaned up manually.
        let legacy_json = serde_json::json!({
            "id": ManagedSessionId::new(),
            "tmux_name": "tmpm-legacy",
            "cwd": "/tmp",
            "task": "legacy task",
            "state": "stopped",
            "created_at": Utc::now().to_rfc3339(),
            "last_activity_at": null,
            "workspace_path": "/tmp/some-workspace",
            "repo_url": null,
            "branch": null,
            "pending_decision": null,
            "proposed_default": null
        })
        .to_string();
        let back: SessionRecord = serde_json::from_str(&legacy_json).expect("deserialize legacy");
        assert!(
            !back.workspace_owned,
            "a record with no `workspace_owned` key must default to false (unowned — safe)"
        );
    }

    #[test]
    fn record_without_scrollback_fields_defaults_to_none() {
        // Why (#1816): pre-#1816 records have no `scrollback_path` or `last_cwd`
        // keys; they MUST deserialize with both as `None` so resume continues to
        // work from workspace_path/cwd as before — zero behavior change.
        let legacy_json = serde_json::json!({
            "id": ManagedSessionId::new(),
            "tmux_name": "tmpm-legacy",
            "cwd": "/tmp",
            "task": "legacy task",
            "state": "stopped",
            "created_at": Utc::now().to_rfc3339(),
            "last_activity_at": null,
            "workspace_path": "/tmp/ws",
            "repo_url": null,
            "branch": null,
            "pending_decision": null,
            "proposed_default": null
        })
        .to_string();
        let back: SessionRecord = serde_json::from_str(&legacy_json).expect("deserialize legacy");
        assert!(
            back.scrollback_path.is_none(),
            "scrollback_path must default to None for legacy records"
        );
        assert!(
            back.last_cwd.is_none(),
            "last_cwd must default to None for legacy records"
        );
    }

    #[test]
    fn record_round_trips_scrollback_fields() {
        // Why (#1816): records written after idle auto-stop must persist both
        // scrollback_path and last_cwd so resume can restore context.
        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-snap".into(),
            cwd: PathBuf::from("/home/user/project"),
            task: "add feature".into(),
            state: ManagedSessionState::Stopped,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: Some(PathBuf::from("/managed/ws")),
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: true,
            source_id: None,
            claude_session_id: None,
            scrollback_path: Some(PathBuf::from("/managed/ws/.trusty-mpm/scrollback.txt")),
            last_cwd: Some(PathBuf::from("/managed/ws/src")),
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.scrollback_path,
            Some(PathBuf::from("/managed/ws/.trusty-mpm/scrollback.txt"))
        );
        assert_eq!(back.last_cwd, Some(PathBuf::from("/managed/ws/src")));
    }

    #[test]
    fn record_round_trips_workspace_owned_true() {
        // Why (#1511): a clone-provisioned session must persist workspace_owned=true
        // so decommission knows it is safe to remove the workspace.
        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-clone".into(),
            cwd: PathBuf::from("/managed/root/owner/repo/abc"),
            task: "fix bug".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: Some(PathBuf::from("/managed/root/owner/repo/abc")),
            repo_url: Some("https://github.com/owner/repo".into()),
            branch: Some("fix/thing".into()),
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: true,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert!(back.workspace_owned, "workspace_owned=true must round-trip");
    }

    #[test]
    fn record_without_deliverable_id_field_defaults_to_none() {
        // Why (#2379): every record persisted before this field existed has no
        // `deliverable_id` key; it MUST deserialize as `None` (unbound) — no
        // session created before the Deliverable layer existed was ever bound
        // to one. This pins the `#[serde(default)]` back-compat contract that
        // lets an old store load cleanly under the new binary, and (by the
        // same additive-field contract) lets an OLD binary reading a NEWER
        // store simply ignore the extra key it does not know about.
        let legacy_json = serde_json::json!({
            "id": ManagedSessionId::new(),
            "tmux_name": "tmpm-legacy",
            "cwd": "/tmp",
            "task": "legacy task",
            "state": "active",
            "created_at": Utc::now().to_rfc3339(),
            "last_activity_at": null,
            "workspace_path": null,
            "repo_url": null,
            "branch": null,
            "pending_decision": null,
            "proposed_default": null
        })
        .to_string();
        let back: SessionRecord = serde_json::from_str(&legacy_json).expect("deserialize legacy");
        assert!(
            back.deliverable_id.is_none(),
            "a record with no `deliverable_id` key must default to None (unbound)"
        );
    }

    #[test]
    fn record_round_trips_deliverable_id() {
        // Why (#2379): a session bound via `tm sessions new --deliverable <id>`
        // must persist the link so `resume`/`ls`/`status` all see it.
        let did = crate::deliverable::DeliverableId::new();
        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-bound".into(),
            cwd: PathBuf::from("/tmp"),
            task: "implement WI-13".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: Some(did),
            pane_id: None,
            injection_status: Default::default(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let back: SessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.deliverable_id, Some(did));
    }
}
