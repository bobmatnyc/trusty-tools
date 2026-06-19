//! UI-agnostic command results.
//!
//! Why: the executor must hand a UI something structured — not a pre-formatted
//! string — so each UI (Telegram HTML, ratatui rows, CLI stdout) can render it
//! in its own idiom. [`CommandResult`] is that structured, transport-free
//! result type.
//! What: [`CommandResult`] enumerates one variant per command outcome, carrying
//! plain data the formatters consume. Errors are a variant, not a `Result::Err`,
//! so an unreachable daemon becomes a renderable message rather than a panic.
//! Test: `cargo test -p trusty-mpm-client` exercises the executor that produces
//! these; formatting is tested per UI crate.

/// A compact session summary for command results.
///
/// Why: a UI rendering `/sessions` needs the id, status, and workdir without
/// the full daemon `Session` shape.
/// What: the three fields every session list renders.
/// Test: covered by the executor's sessions test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session id (UUID string).
    pub id: String,
    /// Lifecycle status string.
    pub status: String,
    /// Working directory.
    pub workdir: String,
}

/// Recent overseer decision counts.
///
/// Why: the `/overseer` result reports how the overseer has been deciding.
/// What: the allow / block / flag tallies.
/// Test: covered by the executor's overseer test.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecisionCounts {
    /// Number of `allow` decisions.
    pub allow: u64,
    /// Number of `block` decisions.
    pub block: u64,
    /// Number of `flag` decisions.
    pub flag: u64,
}

/// One tmux session summary for command results.
///
/// Why: the `/tmux` result lists tmux session names; the `/tmux` keyboard also
/// offers an "Adopt" button for sessions trusty-mpm does not yet manage.
/// What: the session name and whether trusty-mpm already manages it (internal
/// `tmpm-*` / `trusty-mpm-*` sessions) versus an external one.
/// Test: covered by the executor's tmux test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSessionSummary {
    /// tmux session name.
    pub name: String,
    /// True when trusty-mpm created/manages the session; false when external.
    pub managed: bool,
}

/// One discovered Claude Code project for command results.
///
/// Why: the `/projects` result lists projects mined from `~/.claude/projects/`
/// so the operator can register one with a tap.
/// What: the project path, its recorded session count, and the last-used date.
/// Test: covered by the executor's projects test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredProjectSummary {
    /// Absolute project path.
    pub path: String,
    /// Number of recorded Claude Code sessions for the project.
    pub session_count: usize,
    /// ISO-8601 last-session timestamp, or `None` when the project has none.
    pub last_session: Option<String>,
}

/// One Claude Code config recommendation summary.
///
/// Why: the `/config` result surfaces analyzer recommendations.
/// What: the recommendation id and message.
/// Test: covered by the executor's config test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecommendationSummary {
    /// Stable recommendation id.
    pub id: String,
    /// Human-readable description.
    pub message: String,
}

/// A daemon health snapshot for the `health` verb.
///
/// Why: the `health` verb must report a genuinely useful, transport-free picture
/// of the stack in one shape every adapter can render — whether the daemon is
/// reachable, what its `/health` probe reports (liveness + catalog freshness),
/// and a quick fleet summary (how many managed sessions, how many are blocked on
/// an operator decision). A dead daemon is conveyed by `reachable: false` rather
/// than an error, so the result is always renderable.
/// What: `reachable` is false when the daemon could not be reached at all (the
/// remaining fields then carry their defaults); `status` is the daemon's liveness
/// word (`"ok"`, or `"unreachable"` when down); `catalog_stale`/`catalog_unknown`
/// mirror the `/health` flags; `managed_total` counts managed sessions and
/// `managed_pending_decisions` counts those blocked on a pending decision.
/// Test: covered by the executor's `execute_health_*` tests.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HealthReport {
    /// Whether the daemon was reachable at all (false → the daemon is down).
    pub reachable: bool,
    /// Daemon URL the probe targeted.
    pub url: String,
    /// Liveness word from `GET /health` (`"unreachable"` when the daemon is down).
    pub status: String,
    /// True when deployed agents/skills drift from the synced catalog (HR-3).
    pub catalog_stale: bool,
    /// True when the catalog has never been synced (nothing to compare).
    pub catalog_unknown: bool,
    /// Total number of managed sessions (0 when unreachable).
    pub managed_total: usize,
    /// How many managed sessions are blocked on a pending operator decision.
    pub managed_pending_decisions: usize,
}

/// The structured, UI-agnostic outcome of executing a [`crate::TrustyCommand`].
///
/// Why: keeping the result structured (not a string) lets each UI format it in
/// its own idiom; an error variant keeps a dead daemon renderable.
/// What: one variant per command outcome, carrying plain data.
/// Test: produced by the executor, covered by `executor.rs` tests.
#[derive(Debug, Clone)]
pub enum CommandResult {
    /// `/sessions` — the managed session list.
    Sessions(Vec<SessionSummary>),
    /// `/status` — one session's status and recent event names.
    SessionDetail {
        /// Session id or name as supplied.
        id: String,
        /// Lifecycle status string (or `"unknown"` when not listed).
        status: String,
        /// Recent hook-event wire names, newest last.
        events: Vec<String>,
    },
    /// `/overseer` — the overseer's status and recent decision tally.
    OverseerStatus {
        /// Whether the overseer is enabled.
        enabled: bool,
        /// Active overseer strategy name.
        handler: String,
        /// Recent allow / block / flag counts.
        decisions: DecisionCounts,
    },
    /// `/tmux` — every tmux session on the daemon host.
    TmuxSessions(Vec<TmuxSessionSummary>),
    /// `/projects` — projects discovered from the Claude Code configuration.
    DiscoveredProjects(Vec<DiscoveredProjectSummary>),
    /// `/adopt` — an external tmux session was adopted for oversight.
    Adopted {
        /// The tmux session name that was adopted.
        session: String,
    },
    /// `/discover` — auto-discovery scanned tmux and adopted Claude Code sessions.
    Discovered {
        /// Number of tmux sessions newly registered by the scan.
        count: usize,
    },
    /// A discovered project was registered with the daemon ("Set Active").
    ProjectRegistered {
        /// The absolute path of the registered project.
        path: String,
    },
    /// `/config` — Claude Code config analyzer recommendations.
    ConfigAnalysis {
        /// Project directory analyzed.
        project: String,
        /// The analyzer's recommendations (empty when the config is healthy).
        recommendations: Vec<RecommendationSummary>,
    },
    /// `/snapshot` — a captured tmux pane.
    Snapshot {
        /// tmux session name.
        session: String,
        /// Captured pane text (may be empty).
        output: String,
    },
    /// `/kill` — a session was killed.
    Killed {
        /// The session id or name that was killed.
        session_id: String,
    },
    /// `/send` — a prompt was sent to a session; carries the captured output.
    CommandSent {
        /// The resolved session id or friendly name.
        session: String,
        /// The captured pane output after the prompt ran (may be truncated).
        output: String,
    },
    /// LLM chat reply for a free-text message.
    ChatReply {
        /// The assistant's reply text.
        reply: String,
    },
    /// `/approve` — a permission request was approved.
    Approved {
        /// The session id or name that was approved.
        session_id: String,
    },
    /// `/deny` — a permission request was denied.
    Denied {
        /// The session id or name that was denied.
        session_id: String,
    },
    /// `tm pair` — the daemon generated a one-time pairing code.
    PairCode {
        /// The pairing code.
        code: String,
        /// Seconds until the code expires.
        expires_in_seconds: u64,
    },
    /// `/pair <code>` — pairing completed successfully.
    PairSuccess {
        /// Human-readable description of the paired chat.
        chat_info: String,
    },
    /// `/start` or `/pair` with no code — the current pairing status.
    PairState {
        /// Whether a chat is currently paired with the daemon.
        paired: bool,
    },
    /// `/alerts` — the current alert subscription, one line per entry.
    AlertSubscriptions(Vec<String>),
    /// `/doctor` — the full system diagnostic report.
    Doctor(crate::core::doctor::DoctorReport),
    /// `/launch` or `/connect` — a Claude Code session was started or attached.
    ///
    /// Why: both commands report the same outcome — a named tmux session is now
    /// running — and `deployed` tells the UI whether the framework-deployment
    /// sequence was run (`launch`) or skipped (`connect`).
    SessionStarted {
        /// The tmux session name that is now running.
        session: String,
        /// Project directory the session runs in.
        workdir: String,
        /// True when the framework was deployed (`launch`); false for `connect`.
        deployed: bool,
    },
    /// `/help` — the command list text.
    Help(String),
    /// `/health` — a daemon health snapshot (reachability + catalog + fleet).
    Health(HealthReport),

    // ── Managed session-manager outcomes (`/api/v1/sessions/managed/*`) ──────
    /// `managed-new` — a managed session was spawned.
    ///
    /// Why: the operator needs the new session's identity and the exact tmux
    /// command to attach to it immediately after a spawn.
    /// What: the new session's id, friendly name, lifecycle state, runtime
    /// backend, and the attach command string.
    ManagedSpawned {
        /// Managed session id (UUID string).
        id: String,
        /// tmux session name.
        name: String,
        /// Current lifecycle state word.
        state: String,
        /// Runtime backend (`claude-code` | `tcode`).
        runtime: String,
        /// tmux attach command string.
        attach_cmd: String,
    },
    /// `managed-list` — the managed session-manager session list.
    ManagedSessions(Vec<ManagedSessionView>),
    /// `managed-get` — one managed session's record.
    ManagedSession(ManagedSessionView),
    /// `managed-send` — text was injected into a managed session's pane.
    ManagedSent {
        /// The resolved managed session id.
        id: String,
        /// tmux session name the text was sent to.
        tmux_name: String,
    },
    /// `managed-answer` — a pending decision was answered.
    ///
    /// Why: confirms the real `.../answer` endpoint accepted the operator's
    /// decision so the harness unblocks.
    /// What: the resolved session id, the answer injected, and the tmux name.
    ManagedAnswered {
        /// The resolved managed session id.
        id: String,
        /// The answer text that was injected.
        answer: String,
        /// tmux session name the answer was sent to.
        tmux_name: String,
    },
    /// `managed-activity` — a managed session's activity snapshot.
    ManagedActivity {
        /// The resolved managed session id.
        id: String,
        /// Classified activity state (`unknown` when no classifier ran).
        state: String,
        /// Human-readable summary of what the session is doing.
        summary: String,
        /// Raw pane content (last lines).
        raw_pane: String,
        /// Whether the tmux runtime is currently alive.
        runtime_active: bool,
        /// A pending decision question, if surfaced.
        pending_decision: Option<String>,
    },
    /// `managed-attach` — the tmux command to attach to a managed session.
    ManagedAttachCmd {
        /// The resolved managed session id.
        id: String,
        /// tmux attach command string.
        attach_cmd: String,
    },
    /// `managed-stop`/`managed-resume`/`managed-decommission` — the updated
    /// lifecycle state after a runtime transition.
    ///
    /// Why: all three terminal/lifecycle verbs report the same shape — the
    /// session's id, name, and new state — so a UI renders them uniformly.
    /// What: the resolved id, friendly name, the new lifecycle state, and the
    /// human-readable verb that produced it (`stopped`/`resumed`/`decommissioned`).
    ManagedLifecycle {
        /// The resolved managed session id.
        id: String,
        /// tmux session name.
        name: String,
        /// New lifecycle state word.
        state: String,
        /// The verb applied (`stopped` | `resumed` | `decommissioned`).
        action: String,
    },

    /// Any failure rendered as a message rather than a panic.
    Error(String),
}

/// A flat, UI-agnostic view of a managed session.
///
/// Why: the managed list/get results render the same flat summary every adapter
/// consumes; a dedicated view keeps the UIs off the wire DTO
/// ([`crate::client::ManagedSessionSummary`]) so a wire-shape change does not
/// ripple into every formatter.
/// What: the identity and lifecycle fields the UIs render, plus the pending
/// decision a session may be blocked on.
/// Test: covered by the executor's `managed_*` tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSessionView {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name.
    pub name: String,
    /// Lifecycle state word.
    pub state: String,
    /// Provisioned workspace path, if any.
    pub workspace_path: Option<String>,
    /// Repository URL the session was provisioned from.
    pub repo_url: Option<String>,
    /// Git branch or ref checked out.
    pub branch: Option<String>,
    /// A pending decision question, if surfaced.
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
}

impl From<crate::client::ManagedSessionSummary> for ManagedSessionView {
    /// Why: the executor maps the wire DTO to the UI view at the dispatch seam
    /// so adapters never see the transport shape.
    /// What: moves the shared fields across one-to-one.
    /// Test: covered by `execute_managed_list_against_test_daemon`.
    fn from(s: crate::client::ManagedSessionSummary) -> Self {
        Self {
            id: s.id,
            name: s.name,
            state: s.state,
            workspace_path: s.workspace_path,
            repo_url: s.repo_url,
            branch: s.branch,
            pending_decision: s.pending_decision,
            proposed_default: s.proposed_default,
        }
    }
}
