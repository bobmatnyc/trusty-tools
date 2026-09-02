//! Agent delegation model.
//!
//! Why: the dashboard must render a per-session delegation tree (which subagent
//! delegated to which, on what model tier, with what circuit-breaker state).
//! A shared type keeps the daemon's tracker, the TUI tree widget, and the MCP
//! `agent_delegate` tool aligned on one representation.
//! What: `ModelTier` (haiku/sonnet/opus), `DelegationId`, `DelegationStatus`,
//! and `Delegation` — a node in the per-session delegation tree.
//! Test: `cargo test -p trusty-mpm-core` round-trips a `Delegation` through JSON
//! and checks tier parsing.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::core::circuit::CircuitState;
use crate::core::session::SessionId;

/// Coarse model tier an agent delegation runs on.
///
/// Why: claude-mpm enforces a tier policy (PM/planner on opus, specialists on
/// sonnet, cheap tasks on haiku). The dashboard colour-codes by tier and the
/// circuit breaker counts opus delegations more strictly.
/// What: three variants mapping to Claude's model families.
/// Test: `tier_parses_from_model_id` covers the `from_model_id` mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Cheapest, fastest tier — Haiku family.
    Haiku,
    /// Mid tier — Sonnet family (default for specialists).
    Sonnet,
    /// Top tier — Opus family (PM, planner, architecture work).
    Opus,
}

impl ModelTier {
    /// Infer a tier from a Claude model identifier.
    ///
    /// Why: hook payloads and agent frontmatter carry full model ids
    /// (`claude-opus-4-7`); the dashboard wants the coarse tier.
    /// What: substring match on the model family; unknown ids fall back to
    /// `Sonnet` (the safe default specialist tier).
    /// Test: `tier_parses_from_model_id`.
    pub fn from_model_id(model: &str) -> Self {
        let m = model.to_ascii_lowercase();
        if m.contains("opus") {
            ModelTier::Opus
        } else if m.contains("haiku") {
            ModelTier::Haiku
        } else {
            ModelTier::Sonnet
        }
    }
}

/// Tool names Claude Code uses to dispatch a native subagent.
///
/// Why (#2864): delegation tracking hangs off the `PreToolUse`/`PostToolUse`
/// hook, which names the tool being invoked. The subagent-dispatch tool was
/// called `Task` in earlier Claude Code releases and is called `Agent` in
/// current ones (empirically confirmed against Claude Code 2.1.220, whose
/// `PreToolUse` payload carries `"tool_name": "Agent"`). Matching only one of
/// the two names silently disables tracking on half the installed base, so both
/// are recognised and neither may be removed without re-probing the live hook
/// payload.
/// What: the exact `tool_name` values that mean "a subagent is being spawned".
/// Test: `subagent_dispatch_tool_matches_both_names`.
pub const SUBAGENT_DISPATCH_TOOLS: &[&str] = &["Task", "Agent"];

/// Is `tool_name` the native subagent-dispatch tool?
///
/// Why: the single predicate both the `tm hook` payload builder and the daemon's
/// delegation tracker consult, so the two can never disagree about what counts
/// as a delegation. It is also the hot-path early-out — every non-dispatch tool
/// call in every managed session hits this and nothing else.
/// What: case-sensitive membership test against [`SUBAGENT_DISPATCH_TOOLS`]
/// (Claude Code emits the tool name verbatim, so an exact match is correct and
/// avoids matching an unrelated user tool named `agent`).
/// Test: `subagent_dispatch_tool_matches_both_names`.
pub fn is_subagent_dispatch_tool(tool_name: &str) -> bool {
    SUBAGENT_DISPATCH_TOOLS.contains(&tool_name)
}

/// Keys retained from (and recognised in) a subagent-dispatch `tool_response`.
///
/// Why: this list is the contract between the two halves of #2864, and both
/// halves must agree on it or the daemon silently misreads the world. `tm hook`
/// (S1) projects an incoming `tool_response` down to exactly these keys, so a
/// subagent's unbounded output is never forwarded; the daemon (S2) treats a
/// response carrying at least one of them as *understood*, and only an
/// understood response is evidence about whether the dispatch left a subagent
/// running. If the two lists drifted, the daemon would either see a response it
/// could not interpret (fail-closed, merely no tracking) or — worse — treat an
/// uninterpretable object as a synchronous return. One definition removes the
/// possibility.
///
/// `agentId` is the join between `tool_use_id` and `SubagentStop.agent_id`;
/// `status`/`isAsync` say whether the dispatch returned synchronously or was
/// merely launched (`"async_launched"` — subagent still running);
/// `resolvedModel` refines the model tier; `is_error` distinguishes a failed
/// dispatch from a successful one.
/// Test: `compacts_tool_response_to_correlation_keys` (S1),
/// `post_tool_use_with_unrecognized_response_stays_running` (S2).
pub const TOOL_RESPONSE_KEYS: &[&str] =
    &["agentId", "status", "isAsync", "resolvedModel", "is_error"];

/// Stable identifier for one agent delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DelegationId(pub Uuid);

impl DelegationId {
    /// Generate a fresh random delegation id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DelegationId {
    fn default() -> Self {
        Self::new()
    }
}

/// Lifecycle state of a single agent delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatus {
    /// Delegation has been requested but the subagent has not started.
    Queued,
    /// Subagent is actively running.
    Running,
    /// Subagent finished successfully.
    Completed,
    /// Subagent failed.
    Failed,
    /// Delegation was cancelled before completion.
    Cancelled,
    /// Tracking lost this delegation: it sat in a live status past the liveness
    /// budget and no terminal signal ever arrived (#2864 review, HIGH 2).
    ///
    /// Why this is a distinct status and not `Completed`/`Cancelled`: "we know
    /// it finished" and "we lost track of it" are different facts, and
    /// collapsing them is exactly the false report this feature exists to
    /// prevent. `Stale` asserts only that the record may no longer be trusted as
    /// evidence of work in flight. It is deliberately **neither live nor
    /// terminal** — see [`DelegationStatus::is_live`] and
    /// [`DelegationStatus::is_terminal`] — so it stops suppressing the idle
    /// nudge, yet a late `SubagentStop` can still resolve it to the truth.
    /// Test: `stale_running_delegation_stops_suppressing_the_nudge`,
    /// `late_subagent_stop_still_resolves_a_stale_delegation`.
    Stale,
}

impl DelegationStatus {
    /// Does a delegation in this status still count as work in flight?
    ///
    /// Why: the single definition of "live" shared by
    /// [`crate::daemon::idle_nudge::has_live_children`] (which must not nudge a
    /// session whose children are still running) and the staleness sweep (which
    /// must only ever act on live records). Two copies of this predicate would
    /// eventually disagree, and a disagreement here is a false idle report.
    /// What: `true` for `Queued` and `Running` only.
    /// Test: `has_live_children_true_for_running`,
    /// `has_live_children_false_when_terminal`.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    /// Is this a settled outcome the tracker will never move away from?
    ///
    /// Why: `SubagentStop` must not re-terminalize an already-closed delegation
    /// (idempotence), but it *must* still be able to close a `Stale` one — that
    /// recovery path is what keeps the staleness sweep from being a one-way
    /// false negative for a genuinely long-running agent.
    /// What: `true` for `Completed`/`Failed`/`Cancelled`. `Stale` is **not**
    /// terminal.
    /// Test: `subagent_stop_is_idempotent`,
    /// `late_subagent_stop_still_resolves_a_stale_delegation`.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// How a delegation record came to exist.
///
/// Why (#2864): tracking used to be opt-in — a delegation existed only if the PM
/// voluntarily called the `agent_delegate` MCP tool, so a PM that dispatched
/// work with the native subagent tool alone left no trace at all. The daemon now
/// also *observes* dispatches from the `PreToolUse` hook, which is non-opt-in.
/// Recording which of the two produced a record lets the dedup pass merge the
/// declaration and the observation of the *same* delegation into one node
/// instead of double-counting it, and lets consumers tell a PM-asserted
/// intention from an observed fact.
/// What: `McpDeclared` — created by the `agent_delegate` MCP tool (the PM said
/// it would delegate). `HookObserved` — created from a `PreToolUse` hook naming
/// a [`SUBAGENT_DISPATCH_TOOLS`] tool (the runtime actually dispatched).
/// Test: `delegation_defaults_to_mcp_declared_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationSource {
    /// Declared through the `agent_delegate` MCP tool.
    #[default]
    McpDeclared,
    /// Observed from a `PreToolUse` subagent-dispatch hook.
    HookObserved,
}

/// A node in a session's agent-delegation tree.
///
/// Why: the dashboard renders delegations as a tree (PM → research → ...).
/// `parent` lets the TUI reconstruct that tree from a flat list.
/// What: pairs the delegating relationship with the target agent, its model
/// tier, current status, and the circuit-breaker state for that agent.
/// Test: `delegation_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegation {
    /// Unique id for this delegation.
    pub id: DelegationId,
    /// Session the delegation belongs to.
    pub session: SessionId,
    /// Parent delegation, or `None` for a top-level (PM) delegation.
    #[serde(default)]
    pub parent: Option<DelegationId>,
    /// Target agent name (matches an `AgentArtifact::name`).
    pub agent: String,
    /// Model tier the delegation runs on.
    pub tier: ModelTier,
    /// Current lifecycle status.
    pub status: DelegationStatus,
    /// Circuit-breaker state for this agent at the time of the snapshot.
    pub circuit: CircuitState,
    /// Short description of the delegated task.
    #[serde(default)]
    pub task: String,
    /// When the delegation was created (UTC).
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// How this record was created (#2864).
    #[serde(default)]
    pub source: DelegationSource,
    /// Claude Code's `tool_use_id` for the dispatching tool call, when known.
    ///
    /// Why: this is the key that joins `PreToolUse` to its matching
    /// `PostToolUse` — both carry the identical `toolu_…` id. It is what makes
    /// correlation exact under concurrency instead of a "most recent" guess.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Claude Code's subagent `agent_id`, learned from the `PostToolUse`
    /// response and matched against `SubagentStop.agent_id` (#2864).
    ///
    /// Why: `SubagentStop` does not carry `tool_use_id`; it carries `agent_id`.
    /// `PostToolUse.tool_response.agentId` is the only place the two identifier
    /// spaces meet, so it must be persisted here for the stop hook to resolve.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Transcript the dispatching turn was writing to, when known.
    #[serde(default)]
    pub transcript_path: Option<std::path::PathBuf>,
    /// Working directory the dispatch was issued from, when known.
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    /// The working tree the subagent is actually running in, when it differs
    /// from [`Self::cwd`] (#4311).
    ///
    /// Why: [`Self::isolation`] records that a worktree was ASKED for; it does
    /// not say where the harness put one. trusty-mpm creates no agent worktrees
    /// (ADR-0044 decision 4), so the path is not knowable at dispatch time — and
    /// without it the tree has no owner, which is what leaves it unreapable
    /// (ADR-0020's fail-closed rule correctly refuses to remove an owner-unknown
    /// worktree). This field is the recorded parentage DOC-66 §5 requires: the
    /// dispatching session and workstream own this delegation, and this
    /// delegation owns that directory.
    /// What: the subagent's own `cwd`, learned from a hook event the SUBAGENT
    /// emitted (its payload carries `agent_id`), and only when it differs from
    /// the dispatcher's `cwd`. `None` means the subagent shares the dispatcher's
    /// tree, or has not made a tool call yet — never that a worktree is absent.
    /// Test: `subagent_tool_call_registers_its_worktree`,
    /// `subagent_sharing_the_dispatchers_tree_registers_nothing`.
    #[serde(default)]
    pub worktree_path: Option<std::path::PathBuf>,
    /// The directory this subagent's most recent hook event ran in (#6556).
    ///
    /// Why: [`Self::worktree_path`] is a LATCH — once an agent reports a tree of
    /// its own, that field never changes back, because the tree stays this
    /// delegation's to own even after the agent walks out of it. That is right
    /// for the reap, which asks "may this directory be deleted", and wrong for
    /// the shared-tree guard, which asks "where is this agent writing NOW". An
    /// agent can enter a worktree and leave it again — `EnterWorktree` followed
    /// by `ExitWorktree` with `action: "keep"` restores the dispatcher's cwd —
    /// and the latch alone would exclude it from the shared-tree count for the
    /// rest of its life.
    /// What: the `cwd` of the last hook event carrying this delegation's
    /// `agent_id`, rewritten whenever it changes. `None` until the subagent's
    /// first tool call.
    /// Test: `an_agent_that_leaves_its_worktree_blocks_the_shared_tree_again`,
    /// `subagent_tool_calls_track_the_current_working_directory`.
    #[serde(default)]
    pub last_agent_cwd: Option<std::path::PathBuf>,
    /// Whether this record was staled by matching a `SubagentStop`'s
    /// `agent_type` rather than its `agent_id` (#6556).
    ///
    /// Why: that reconciliation identifies a record by TYPE, which is weaker
    /// than by id — a stop whose own dispatch was never observed can name a
    /// still-running sibling of the same type. `Stale` alone would then release
    /// the tree for BOTH questions the delegation map answers, and one of them
    /// must not be released: admitting a second file-mutating agent onto a HEAD
    /// a live writer holds is the ADR-0048 harm. This flag is what lets
    /// [`crate::daemon::state::DaemonState::live_shared_tree_writers`] and
    /// [`crate::daemon::state::DaemonState::shared_tree_occupants`] answer
    /// differently for one record.
    /// What: set only by the type-reconciliation path; never by the staleness
    /// sweep and never by #6497's dead-session reaper, both of which act on
    /// evidence about the record's own owner.
    /// Test: `a_type_reconciled_record_still_occupies_the_tree_for_a_dispatch`.
    #[serde(default)]
    pub stale_by_agent_type: bool,
    /// The `isolation` mode the dispatch declared, when it declared one (#4480).
    ///
    /// Why: without this, a delegation record cannot answer the one question
    /// that separates a safe concurrent dispatch from a dangerous one — whether
    /// the subagent got a working tree of its own or inherited the dispatcher's.
    /// `None` is the DEFAULT and the hazardous case, not missing data: the Agent
    /// tool's `isolation` parameter is opt-in, so an absent field means the
    /// subagent is running in [`Self::cwd`] alongside every sibling.
    /// What: the raw declared value (`"worktree"`, `"remote"`, …), interpreted
    /// through
    /// [`isolation_separates_working_tree`](crate::core::dispatch_isolation::isolation_separates_working_tree)
    /// rather than compared here — storing the literal keeps the record a
    /// faithful account of what was dispatched even as the policy evolves.
    #[serde(default)]
    pub isolation: Option<String>,
    /// When the subagent actually started running (UTC), when known.
    #[serde(default)]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the delegation reached a **terminal** status (UTC), when known.
    ///
    /// This means exactly that and nothing broader — every writer sets it
    /// alongside `Completed`/`Failed`/`Cancelled`. In particular the staleness
    /// sweep does NOT stamp it: a [`DelegationStatus::Stale`] record has no
    /// `ended_at`, because it did not end — tracking merely stopped trusting it,
    /// and the subagent may still be running. Keeping the field single-meaning
    /// is what lets it serve as the terminal-retention clock without putting a
    /// possibly-live record on that clock (#2864 re-review).
    #[serde(default)]
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Delegation {
    /// Build a freshly-queued delegation stamped with the current time.
    pub fn new(
        session: SessionId,
        parent: Option<DelegationId>,
        agent: impl Into<String>,
        tier: ModelTier,
        task: impl Into<String>,
    ) -> Self {
        Self {
            id: DelegationId::new(),
            session,
            parent,
            agent: agent.into(),
            tier,
            status: DelegationStatus::Queued,
            circuit: CircuitState::Closed,
            task: task.into(),
            created_at: chrono::Utc::now(),
            source: DelegationSource::McpDeclared,
            tool_use_id: None,
            agent_id: None,
            transcript_path: None,
            cwd: None,
            worktree_path: None,
            last_agent_cwd: None,
            stale_by_agent_type: false,
            isolation: None,
            started_at: None,
            ended_at: None,
        }
    }

    /// Build a `Running` delegation observed from a subagent-dispatch hook.
    ///
    /// Why (#2864): a `PreToolUse` naming a [`SUBAGENT_DISPATCH_TOOLS`] tool is
    /// proof the runtime is spawning a subagent *now* — not queueing it — so
    /// unlike [`Self::new`] this starts in [`DelegationStatus::Running`] with
    /// `started_at` stamped. Carrying `tool_use_id` from birth is what lets the
    /// matching `PostToolUse` find exactly this record among concurrent
    /// siblings.
    /// What: same shape as [`Self::new`] but `source = HookObserved`, status
    /// `Running`, and `started_at = created_at`.
    /// Test: `observed_delegation_starts_running`.
    pub fn observed(
        session: SessionId,
        agent: impl Into<String>,
        task: impl Into<String>,
        tool_use_id: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: DelegationId::new(),
            session,
            parent: None,
            agent: agent.into(),
            tier: ModelTier::Sonnet,
            status: DelegationStatus::Running,
            circuit: CircuitState::Closed,
            task: task.into(),
            created_at: now,
            source: DelegationSource::HookObserved,
            tool_use_id,
            agent_id: None,
            transcript_path: None,
            cwd: None,
            worktree_path: None,
            last_agent_cwd: None,
            stale_by_agent_type: false,
            isolation: None,
            started_at: Some(now),
            ended_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_parses_from_model_id() {
        assert_eq!(ModelTier::from_model_id("claude-opus-4-7"), ModelTier::Opus);
        assert_eq!(
            ModelTier::from_model_id("claude-haiku-3-5"),
            ModelTier::Haiku
        );
        assert_eq!(
            ModelTier::from_model_id("claude-sonnet-4"),
            ModelTier::Sonnet
        );
        // Unknown ids fall back to Sonnet.
        assert_eq!(ModelTier::from_model_id("mystery-model"), ModelTier::Sonnet);
    }

    #[test]
    fn delegation_round_trips() {
        let d = Delegation::new(
            SessionId::new(),
            None,
            "research",
            ModelTier::Sonnet,
            "find the bug",
        );
        let json = serde_json::to_string(&d).unwrap();
        let back: Delegation = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent, "research");
        assert_eq!(back.status, DelegationStatus::Queued);
        assert_eq!(back.tier, ModelTier::Sonnet);
        assert!(back.parent.is_none());
    }

    #[test]
    fn subagent_dispatch_tool_matches_both_names() {
        // Both the legacy (`Task`) and current (`Agent`, Claude Code 2.1.220)
        // names must match — recognising only one silently disables tracking.
        assert!(is_subagent_dispatch_tool("Task"));
        assert!(is_subagent_dispatch_tool("Agent"));
        // Exact match only: ordinary tools and lookalikes must not match.
        assert!(!is_subagent_dispatch_tool("Bash"));
        assert!(!is_subagent_dispatch_tool("agent"));
        assert!(!is_subagent_dispatch_tool("TaskRunner"));
        assert!(!is_subagent_dispatch_tool(""));
    }

    #[test]
    fn delegation_defaults_to_mcp_declared_source() {
        // `Delegation::new` is the `agent_delegate` path and must keep its
        // pre-#2864 semantics: Queued, MCP-declared, no hook correlation keys.
        let d = Delegation::new(SessionId::new(), None, "qa", ModelTier::Sonnet, "test");
        assert_eq!(d.source, DelegationSource::McpDeclared);
        assert_eq!(d.status, DelegationStatus::Queued);
        assert!(d.tool_use_id.is_none());
        assert!(d.started_at.is_none());
        assert!(d.ended_at.is_none());
    }

    #[test]
    fn observed_delegation_starts_running() {
        let d = Delegation::observed(
            SessionId::new(),
            "engineer",
            "implement",
            Some("toolu_01ABC".into()),
        );
        assert_eq!(d.source, DelegationSource::HookObserved);
        assert_eq!(d.status, DelegationStatus::Running);
        assert_eq!(d.tool_use_id.as_deref(), Some("toolu_01ABC"));
        assert_eq!(d.started_at, Some(d.created_at));
        assert!(d.ended_at.is_none());
    }

    #[test]
    fn legacy_delegation_json_without_new_fields_round_trips() {
        // Records persisted before #2864 carry none of the new fields; they must
        // still deserialize (every new field is `#[serde(default)]`) and default
        // to the MCP-declared source they in fact came from.
        let legacy = serde_json::json!({
            "id": Uuid::new_v4(),
            "session": SessionId::new(),
            "agent": "research",
            "tier": "sonnet",
            "status": "queued",
            "circuit": CircuitState::Closed,
            "task": "investigate",
            "created_at": chrono::Utc::now(),
        });
        let back: Delegation =
            serde_json::from_value(legacy).expect("legacy delegation JSON must deserialize");
        assert_eq!(back.source, DelegationSource::McpDeclared);
        assert!(back.tool_use_id.is_none());
        assert!(back.agent_id.is_none());
        assert!(back.cwd.is_none());
    }

    #[test]
    fn delegation_tree_parent_links() {
        let session = SessionId::new();
        let root = Delegation::new(session, None, "pm", ModelTier::Opus, "orchestrate");
        let child = Delegation::new(
            session,
            Some(root.id),
            "engineer",
            ModelTier::Sonnet,
            "implement",
        );
        assert_eq!(child.parent, Some(root.id));
    }
}
