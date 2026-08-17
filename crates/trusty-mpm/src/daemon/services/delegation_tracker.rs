//! Non-opt-in delegation tracking from Claude Code hooks (#2864 S2).
//!
//! Why: delegation tracking had two defects that together made it useless as a
//! basis for "which agents are in flight?". (1) It was opt-in — a record existed
//! only if the PM chose to call the `agent_delegate` MCP tool, so a PM that
//! dispatched work with the native subagent tool alone left no trace. (2) Status
//! never moved: the only production construction is
//! [`Delegation::new`](crate::core::agent::Delegation::new), which yields
//! `Queued`, and nothing ever wrote `Running`/`Completed`/`Failed`. So
//! [`has_live_children`](crate::daemon::idle_nudge::has_live_children) — which
//! treats `Queued` and `Running` as live — reported *every* delegation as live
//! forever, permanently suppressing the idle nudge for any session that had ever
//! delegated once.
//!
//! This module fixes both by observing hooks the daemon already receives: a
//! `PreToolUse` with `matcher: "*"` is installed on every managed session, so
//! every native subagent dispatch already arrives here. No new hook, no opt-in.
//!
//! # Correlation model (empirically established, #2864 Step 0)
//!
//! Captured from live Claude Code 2.1.220 stdin with two *concurrent*
//! subagents — the case that breaks any "most recent delegation" guess:
//!
//! ```text
//! PreToolUse   tool_name=Agent  tool_use_id=toolu_01DA…   (no agent id yet)
//! PostToolUse  tool_name=Agent  tool_use_id=toolu_01DA…
//!              tool_response={ isAsync: true,
//!                              status: "async_launched",
//!                              agentId: "a403cdbc078b5c474" }
//! SubagentStop                  agent_id="a403cdbc078b5c474"
//! ```
//!
//! Two consequences drive the whole design, and **neither may be "simplified"
//! away without re-running the probe**:
//!
//! 1. **`PostToolUse` does NOT mean the subagent finished.** For an async
//!    dispatch it fires ~1 ms after launch (`duration_ms: 1`) with
//!    `status: "async_launched"`, while the subagent runs on for minutes.
//!    Terminalizing there would mark every delegation `Completed` almost
//!    immediately and report "no agents in flight" while they are all still
//!    running — strictly worse than not tracking at all. `PostToolUse` is
//!    therefore a *join* step (it teaches us `agentId`), and terminalizes only
//!    when the dispatch genuinely returned synchronously.
//! 2. **`SubagentStop` DOES carry an exact correlation key** — `agent_id`,
//!    identical to `tool_response.agentId`. It is the authoritative terminal
//!    signal, not an advisory one. Because the key is exact, closing one of two
//!    concurrent delegations cannot close the other.
//!
//! Absent an `agent_id` (an older Claude Code, or a dispatch whose `PostToolUse`
//! was missed) a `SubagentStop` terminalizes **nothing**. Guessing "the most
//! recent Running delegation" would close the wrong one under concurrency and
//! manufacture a false idle, which is the specific failure this design exists to
//! prevent.
//!
//! # Fail direction
//!
//! Every branch here fails **closed**: uncertainty leaves a delegation `Running`
//! and never terminalizes it. A false "still running" delays an idle nudge; a
//! false "completed" tells a human that no agents are in flight when several
//! are, which is the one report this feature must never produce. So
//! [`on_subagent_stop`] acts only on an exact `agent_id`, and [`on_launched`]
//! acts only on a `tool_response` it [`recognized_response`]-ised — an absent or
//! unrecognized response is "we do not know", not "it finished".
//!
//! The cost of failing closed is that a delegation can enter `Running` with no
//! route out: `SubagentStop` is not guaranteed to arrive (`tm hook` POSTs fail
//! open on a 2 s budget, an interrupted subagent emits no stop, a dispatch that
//! never learned an `agent_id` can never be resolved by one). That is bounded
//! outside the hot path by
//! [`DaemonState::sweep_delegations`](crate::daemon::state::DaemonState::sweep_delegations),
//! which the 60 s reap loop drives: a long-overdue live delegation becomes
//! `Stale` — explicitly *not* `Completed`, and still recoverable by a late stop —
//! so it stops suppressing the idle nudge without anyone ever being told it
//! finished.
//!
//! # Cost
//!
//! [`observe`] runs inside the synchronous `PreToolUse` hook, which has a 5 s
//! timeout and sits in front of *every* tool call in *every* managed session.
//! The non-delegation path — which is essentially all traffic — is one
//! `payload.get("tool")` plus an exact string compare against two constants, and
//! then returns. The delegation path is in-memory `DashMap` work only: no disk,
//! no network, no async, no blocking lock.
//!
//! # Known limitation
//!
//! Records are keyed by the session id the event arrives under: the Claude
//! session UUID for hook-observed records (matching
//! [`crate::daemon::idle_nudge::maybe_nudge_parked_session`]), but whatever id
//! the PM passed for `agent_delegate` records. When those differ, [`dedup`]
//! cannot match and a declaration plus its observation remain two records. That
//! id ambiguity predates #2864 and is not addressed here.
//!
//! Test: the `#[cfg(test)]` suite below.

use chrono::Utc;
use serde_json::Value;

use crate::core::agent::{
    Delegation, DelegationId, DelegationSource, DelegationStatus, ModelTier, TOOL_RESPONSE_KEYS,
    is_subagent_dispatch_tool,
};
use crate::core::dispatch_isolation::{dispatch_isolation, isolation_separates_working_tree};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_ownership::{
    AgentWorktreeOwner, SentinelOwner, read_sentinel_owner, write_agent_sentinel,
};

use crate::session_manager::worktree_ownership::is_harness_agent_worktree;

/// How long after an `agent_delegate` call an observed dispatch may be treated
/// as the *same* delegation.
///
/// Why: a PM that both declares (`agent_delegate`) and dispatches (native tool)
/// must produce one record, not two. The declaration immediately precedes the
/// dispatch *in the same turn*, so the true gap is seconds; the window only has
/// to absorb a slow turn, not a whole work item. It was 300 s, which is long
/// enough for a PM that dispatches the same agent repeatedly — this repo
/// routinely runs several `rust-engineer`s — to have an unrelated later dispatch
/// absorbed into an earlier declaration (#2864 review, MEDIUM 2). Two minutes
/// keeps the legitimate merge and cuts the collision surface; [`tasks_match`] is
/// the second, independent discriminator.
/// Test: `dedups_declaration_and_observation`, `dedup_window_expires`.
const DEDUP_WINDOW_SECS: i64 = 120;

/// Observe one hook event and update delegation tracking.
///
/// Why: the single entry point [`crate::daemon::services::hook_service::HookService::process`]
/// calls, so the hook pipeline gains delegation tracking by way of one line and
/// all the correlation rules stay here.
/// What: routes a subagent-dispatch `PreToolUse` to [`on_dispatch`], its
/// `PostToolUse`/`PostToolUseFailure` to [`on_launched`], and
/// `SubagentStop`/`SubagentStopFailure` to [`on_subagent_stop`]. Every other
/// event returns immediately. Never panics and never blocks; a payload missing
/// the fields it needs is skipped silently (fail-open — tracking is
/// observational and must never affect the hook verdict).
/// Test: `ignores_unrelated_tools`, `pre_tool_use_creates_running_delegation`,
/// and the rest of the suite below.
pub fn observe(state: &DaemonState, session: SessionId, event: HookEvent, payload: &Value) {
    match event {
        // Hot path: the overwhelming majority of PreToolUse events are ordinary
        // tools. Bail on the tool-name compare before touching any state.
        HookEvent::PreToolUse => {
            // #4311: a SUBAGENT's own tool call is the only event that names the
            // tree the harness gave it. Costs one `payload.get("agent_id")` on
            // the hot path — the same class as the tool-name compare below — and
            // writes only when the answer changed.
            register_agent_worktree(state, session, payload);
            if dispatch_tool(payload).is_some() {
                on_dispatch(state, session, payload);
            }
        }
        HookEvent::PostToolUse | HookEvent::PostToolUseFailure => {
            if dispatch_tool(payload).is_some() {
                on_launched(state, session, payload, event);
            }
        }
        HookEvent::SubagentStop | HookEvent::SubagentStopFailure => {
            on_subagent_stop(state, session, payload, event);
        }
        _ => {}
    }
}

/// The tool name, when this payload names a subagent-dispatch tool.
///
/// Why: the hot-path early-out — one map lookup and one exact string compare.
/// Test: `ignores_unrelated_tools`.
fn dispatch_tool(payload: &Value) -> Option<&str> {
    payload
        .get("tool")
        .and_then(Value::as_str)
        .filter(|t| is_subagent_dispatch_tool(t))
}

/// Read a `&str` field from a payload, treating empty as absent.
fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// The `agentId` a `tool_response` actually offers as a usable correlation
/// handle: present, a string, and non-empty.
///
/// Why (#4163): [`classify_dispatch`]'s presence-check and [`on_launched`]'s
/// consumption used to ask different questions —
/// `r.get("agentId").is_some()` treated `null` and `""` as "we have an id",
/// while the consumer's `.and_then(Value::as_str)` (for `null`) or
/// [`on_subagent_stop`]'s `field` (for `""`) both reject them. A response
/// carrying either shape took the `Launched` branch, pinning the delegation
/// `Running` with an `agent_id` no `SubagentStop` can ever quote back —
/// burning the full 6 h staleness window as a phantom in-flight entry. This
/// is a thin wrapper around [`field`] — the exact same extraction
/// [`on_subagent_stop`] already uses for `agent_id` — rather than an
/// independent body, so all three sites route through one implementation and
/// are structurally incapable of disagreeing again.
/// Test: `null_agent_id_does_not_launch_a_phantom_delegation`,
/// `empty_string_agent_id_does_not_launch_a_phantom_delegation`.
fn usable_agent_id(response: &Value) -> Option<&str> {
    field(response, "agentId")
}

/// Record the working tree a subagent is actually running in (#4311).
///
/// Why: trusty-mpm creates no agent worktrees (ADR-0044 decision 4), so it
/// cannot know the path at dispatch time — the harness chooses it. Without the
/// path the tree has no owner, and an owner-unknown worktree is one ADR-0020
/// correctly forbids anything from removing. That is the whole reason 56
/// unowned trees accumulated under `.claude/worktrees` by the 2026-07-29 count.
///
/// The agent reports its own path, and it is the only party that can. Claude
/// Code stamps `agent_id` into every hook payload a SUBAGENT emits (the same
/// marker `pm_guard_fanout` reads to tell a subagent from the PM) and `tm hook`
/// already forwards it, alongside the `cwd` that hook process is standing in.
/// For an isolated agent that cwd IS the harness worktree. Nothing is created,
/// nothing is relocated, and no new hook is installed — this reads an event the
/// daemon already receives and discards.
/// What: matches `payload.agent_id` against the `agent_id` `on_launched`
/// learned, and writes `payload.cwd` to that delegation's `worktree_path`.
/// Writes nothing when the cwd equals the DISPATCHER's `cwd` — that subagent
/// inherited the caller's tree, so there is no child tree to own — and nothing
/// when the value is already recorded, which is what keeps this off the hot
/// path after the first tool call.
///
/// It is retried on every subagent tool call rather than run once, because the
/// first one can lose a race: `agent_id` is taught only by the dispatch's
/// `PostToolUse`, that hook is installed `async: true`, and until it lands there
/// is no record to match against. A registration that misses simply happens on
/// the next call.
///
/// # The sentinel is written FIRST, and a failed write registers nothing
///
/// The in-memory record is what grants
/// [`super::agent_worktree_reap`] authority to run `git worktree remove
/// --force` on that path. The sentinel is the only evidence of that authority
/// that survives a daemon restart (`daemon::state::core` initialises the
/// delegation map empty and never loads one), so registering after a failed
/// write would grant deletion authority whose durable record does not exist —
/// the fail-OPEN branch, and the exact shape of the bug this whole change
/// closes: a worktree that silently becomes unattributable.
///
/// So the order is write-then-register, and a write failure declines the
/// registration. That is affordable precisely because this function is
/// retried on every subagent tool call: a transient failure self-heals on the
/// next one at no extra cost. Declining leaves the tree owner-UNKNOWN, which
/// is the pre-#4311 state and is never auto-deleted by anything — fail-closed
/// in both directions. It is never fatal to the hook: [`observe`] is
/// observational and must never affect the tool verdict, so the failure is
/// logged at `error!` and the hook returns normally.
///
/// # `cwd` is agent-controlled, so two gates stand in front of the write
///
/// The `cwd` here is `std::env::current_dir()` of the `tm hook` process
/// (`core::standalone::misc`), which is wherever the agent last `cd`-ed. It is
/// not a path trusty-mpm chose, and an agent that visits its own main checkout,
/// `/private/tmp`, or a peer's worktree reports that instead. Writing there
/// would truncate whatever sentinel already sat at that path and retarget the
/// reap at a directory the agent merely walked through.
///
/// So [`is_harness_agent_worktree`] must accept the path — the identical
/// predicate [`super::agent_worktree_reap::reap_worktree`] uses as its own gate,
/// reused rather than restated so the write and the delete cannot disagree
/// about what a worktree is — AND the path's existing sentinel must be either
/// absent/unparsable ([`SentinelOwner::Unknown`], nothing to overwrite) or
/// already this same agent's. Anything else refuses and logs, exactly like the
/// write-failure arm below.
/// Test: `subagent_tool_call_registers_its_worktree`,
/// `subagent_sharing_the_dispatchers_tree_registers_nothing`,
/// `an_unknown_agent_id_registers_nothing`,
/// `a_failed_sentinel_write_registers_no_worktree`,
/// `a_cwd_outside_the_harness_store_gets_no_sentinel`,
/// `a_cwd_owned_by_another_agent_is_never_overwritten`,
/// `a_cwd_owned_by_a_managed_session_is_never_overwritten`.
/// Why this agent may NOT claim `cwd` as its worktree, or `None` if it may.
///
/// Why: the `cwd` a subagent reports is `std::env::current_dir()` of its own
/// `tm hook` process, so it follows wherever the agent `cd`-ed. It is a value
/// the agent controls, and a claim on it grants the reaper authority to delete
/// that directory. Four independent claims must all hold, and each answers a
/// question the others cannot.
/// What: `Some(reason)` refuses and is logged; `None` proceeds to the write.
///
/// 1. **Shape** — [`is_harness_agent_worktree`], the identical predicate
///    [`super::agent_worktree_reap::reap_worktree`] gates on, imported rather
///    than restated so the write and the delete cannot disagree about what a
///    worktree is. Rejects a main checkout, a scratchpad, a nested path.
/// 2. **Project** — the store this path sits in must be the store of the
///    DISPATCHING session's own checkout. Claim 1 has already established the
///    path is `<X>/.claude/worktrees/<name>`, so `<X>` is the checkout owning
///    that store; the dispatcher's `cwd` must be `<X>` or sit under it. Under
///    [ADR-0037](../../../../../docs/adr/0037-pm-placement-precedence-main-checkout-by-default.md)
///    a PM is normally in the main checkout, which IS `<X>`, and under
///    [ADR-0036](../../../../../docs/adr/0036-all-worktrees-are-siblings-under-claude-worktrees.md)
///    a PM working from a worktree is a flat sibling beneath the same `<X>` —
///    both normal placements pass. This is deliberately LEXICAL rather than a
///    `harness_root_for` call: that resolves through `git rev-parse`, and this
///    runs inside the synchronous `PreToolUse` budget. The lexical form refuses
///    a superset (a PM in a different checkout of the same project), which is
///    the fail-closed direction.
/// 3. **Sentinel** — the path's existing sentinel must be absent/unparsable or
///    already this same agent's. `fs::write` truncates, so without this an
///    agent reporting a peer's tree erases that peer's ownership record, after
///    which the peer's tree reaps on the wrong agent's exit and the peer's own
///    exit reaps nothing.
/// 4. **Registration** — no OTHER non-terminal delegation may already record
///    this path. Claim 3 cannot see a peer worktree that carries no sentinel,
///    which is every directory in the store until they acquire one; this claim
///    covers exactly that case from the daemon's own live state, and is the
///    write-side mirror of the reap's in-use gate.
///
/// Residual, stated: a sentinel-LESS peer whose delegation was also lost to a
/// daemon restart is invisible to claims 3 and 4 both. Such a directory is
/// unattributed and unregistered, so nothing else can reclaim it either; the
/// exposure closes as worktrees acquire sentinels.
///
/// A convention that WOULD close it — the harness names each tree
/// `agent-<agent_id>`, which holds for all 16 such directories on this machine
/// — is deliberately not used. Nothing in this codebase or in any contract with
/// Claude Code establishes that name, so a harness rename would silently stop
/// every registration rather than failing visibly.
/// Test: `a_cwd_outside_the_harness_store_gets_no_sentinel`,
/// `a_cwd_in_another_projects_store_is_refused`,
/// `a_cwd_owned_by_another_agent_is_never_overwritten`,
/// `a_cwd_owned_by_a_managed_session_is_never_overwritten`,
/// `a_cwd_another_live_delegation_holds_is_refused`,
/// `an_agent_may_rewrite_its_own_sentinel`.
fn claim_refused(
    state: &DaemonState,
    session: SessionId,
    agent_id: &str,
    cwd: &std::path::Path,
) -> Option<String> {
    if !is_harness_agent_worktree(cwd) {
        return Some("it is not a `.claude/worktrees/<name>` leaf".to_string());
    }
    // `<X>/.claude/worktrees/<name>` — three parents up is `<X>`, the checkout
    // that owns the store. Claim 1 guarantees the first two exist.
    let store_owner = cwd
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)?;
    let dispatcher_cwd = state
        .delegations_for(session)
        .into_iter()
        .find(|d| d.agent_id.as_deref() == Some(agent_id))
        .and_then(|d| d.cwd);
    match dispatcher_cwd {
        Some(dispatch_from) if dispatch_from.starts_with(store_owner) => {}
        Some(dispatch_from) => {
            return Some(format!(
                "its store belongs to {}, but the dispatching session works from {} — an \
                 agent may not claim a worktree in another project's store",
                store_owner.display(),
                dispatch_from.display()
            ));
        }
        // No recorded dispatcher cwd is no proof of a shared project.
        None => {
            return Some(
                "the dispatching delegation records no working directory, so nothing \
                 establishes this store belongs to its project"
                    .to_string(),
            );
        }
    }
    match read_sentinel_owner(cwd) {
        SentinelOwner::Unknown => {}
        SentinelOwner::Agent(existing, _) if existing.agent_id == agent_id => {}
        occupied => return Some(format!("{occupied:?} already owns it")),
    }
    if let Some(holder) = state.all_delegations().into_iter().find(|d| {
        !d.status.is_terminal()
            && d.agent_id.as_deref() != Some(agent_id)
            && d.worktree_path.as_deref() == Some(cwd)
    }) {
        return Some(format!(
            "agent {} is still running and already registers it",
            holder.agent_id.as_deref().unwrap_or("<unknown>")
        ));
    }
    None
}

fn register_agent_worktree(state: &DaemonState, session: SessionId, payload: &Value) {
    let Some(agent_id) = field(payload, "agent_id") else {
        return;
    };
    let Some(cwd) = field(payload, "cwd").map(std::path::PathBuf::from) else {
        return;
    };
    let Some(id) = state.find_delegation(session, |d| {
        d.agent_id.as_deref() == Some(agent_id)
            && d.worktree_path.as_deref() != Some(cwd.as_path())
            && d.cwd.as_deref() != Some(cwd.as_path())
    }) else {
        return;
    };
    if let Some(refusal) = claim_refused(state, session, agent_id, &cwd) {
        tracing::warn!(
            agent_id,
            cwd = %cwd.display(),
            "delegation: not claiming this worktree — {refusal} (#4311)"
        );
        return;
    }
    let owner = AgentWorktreeOwner {
        agent_id: agent_id.to_string(),
        delegation_id: id,
        parent_session_id: session,
    };
    if let Err(e) = write_agent_sentinel(&cwd, owner) {
        tracing::error!(
            agent_id,
            worktree = %cwd.display(),
            "delegation: could not write the agent worktree's ownership sentinel ({e}); \
             leaving the tree unregistered and owner-unknown rather than granting a reap \
             authority nothing on disk records — retried on this agent's next tool call (#4311)"
        );
        return;
    }
    tracing::info!(
        agent_id,
        worktree = %cwd.display(),
        "delegation: registered the agent worktree its dispatch was granted (#4311)"
    );
    state.mutate_delegation(id, |d| d.worktree_path = Some(cwd.clone()));
}

/// `PreToolUse` on a dispatch tool: a subagent is starting now.
///
/// Why: this is the moment tracking must record a *live* child, so a PM with
/// work in flight is never mistaken for idle.
/// What: reads `input.subagent_type` (agent) and `input.description` (task).
/// Idempotent on `tool_use_id` — a redelivered hook updates nothing twice. Tries
/// [`dedup`] first so a matching `agent_delegate` record is promoted in place to
/// `Running`; otherwise inserts a fresh
/// [`Delegation::observed`](crate::core::agent::Delegation::observed). Records
/// `cwd`, `isolation` (#4480) and `transcript_path` when present.
/// Test: `pre_tool_use_creates_running_delegation`,
/// `dedups_declaration_and_observation`, `duplicate_pre_tool_use_is_idempotent`,
/// `pre_tool_use_records_declared_isolation`.
fn on_dispatch(state: &DaemonState, session: SessionId, payload: &Value) {
    let _guard = state.dispatch_record_guard();
    on_dispatch_locked(state, session, payload);
}

/// Correct an existing delegation's `isolation`, or create the record (#5769).
///
/// Why: `tm hook --pm-guard` rewrites an unisolated dispatch made from a main
/// checkout into an isolated one (ADR-0048 decision 1), but [`on_dispatch`]
/// observes the ORIGINAL payload, so the record read `isolation: None` — naming
/// an agent that had just been moved into its own worktree as a live writer in
/// the shared checkout. Decision 10 then denies `git pull` there on that record,
/// for the six hours of `RUNNING_STALE_AFTER_SECS`, which blocks the release
/// flow's fast-forward of the main checkout.
///
/// It is an UPSERT and not a second call to [`observe`] for two reasons read
/// from this file. [`on_dispatch`] returns early when a delegation with the same
/// `tool_use_id` already exists, so an `observe`-based grant would correct
/// nothing whenever the tracker's hook won the race — and the two hooks fire on
/// the same event, so which one wins is not decidable here. And the route the
/// guard would otherwise post to re-derives eligibility and rejects an isolated
/// dispatch by design. So isolation must be written over whatever landed first,
/// which makes the two arrival orders converge instead of one of them deciding.
/// What: with the dispatch-record lock held, sets `isolation` on the delegation
/// carrying `payload.tool_use_id`, or creates the record from this payload when
/// none exists — in which case the tracker's own later hook is a no-op on
/// `tool_use_id` and the granted isolation survives.
///
/// Two payloads record NOTHING and return `false`, and both rules live HERE
/// rather than in the caller. One with no `tool_use_id`: without that key the
/// record could not be found again, so creating one would leave a second,
/// unisolated record beside the tracker's — the phantom duplicated rather than
/// removed. And one whose `isolation` does not
/// [`isolation_separates_working_tree`]: this function's whole purpose is to
/// erase a record that names a writer as sharing the checkout, so writing a
/// non-separating mode over a correct record would write the very phantom it
/// exists to remove. The route that calls this re-derives the same test to
/// decide eligibility; keeping it in the writer as well means a second caller,
/// or a refactor of that route, cannot reach the write without it.
/// Test: `a_grant_and_the_tracker_converge_in_either_order`,
/// `record_granted_isolation_refuses_a_non_separating_mode`.
pub fn record_granted_isolation(state: &DaemonState, session: SessionId, payload: &Value) -> bool {
    let Some(tool_use_id) = field(payload, "tool_use_id") else {
        return false;
    };
    let declared = dispatch_isolation(payload.get("input"));
    if !isolation_separates_working_tree(declared) {
        return false;
    }
    let Some(isolation) = declared.map(str::to_string) else {
        return false;
    };
    let _guard = state.dispatch_record_guard();
    if let Some(id) =
        state.find_delegation(session, |d| d.tool_use_id.as_deref() == Some(tool_use_id))
    {
        state.mutate_delegation(id, |d| d.isolation = Some(isolation.clone()));
        return true;
    }
    on_dispatch_locked(state, session, payload);
    true
}

/// [`on_dispatch`] with the dispatch-record lock already held.
///
/// Why: [`record_granted_isolation`] holds that lock across its own
/// find-then-insert and then falls through to this body, so the locking cannot
/// live here — it would be a second, non-reentrant acquisition of a
/// `parking_lot::Mutex`, which deadlocks rather than failing.
fn on_dispatch_locked(state: &DaemonState, session: SessionId, payload: &Value) {
    let input = payload.get("input");
    let agent = input
        .and_then(|i| i.get("subagent_type"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let task = input
        .and_then(|i| i.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_use_id = field(payload, "tool_use_id").map(str::to_string);

    // Idempotence: an already-tracked tool_use_id means this hook was redelivered.
    if let Some(id) = tool_use_id.as_deref()
        && state
            .find_delegation(session, |d| d.tool_use_id.as_deref() == Some(id))
            .is_some()
    {
        return;
    }

    let cwd = field(payload, "cwd").map(std::path::PathBuf::from);
    let transcript = field(payload, "transcript_path").map(std::path::PathBuf::from);
    // #4480: what the dispatch declared, recorded verbatim. Absent is the
    // default and the hazardous case — the subagent inherits `cwd`.
    let isolation = dispatch_isolation(input).map(str::to_string);

    // Merge with a matching `agent_delegate` declaration when one is pending.
    if let Some(existing) = dedup(state, session, agent, task) {
        state.mutate_delegation(existing, |d| {
            d.status = DelegationStatus::Running;
            d.started_at = Some(Utc::now());
            d.tool_use_id = tool_use_id.clone();
            d.cwd = cwd.clone();
            d.isolation = isolation.clone();
            d.transcript_path = transcript.clone();
            if d.task.is_empty() && !task.is_empty() {
                d.task = task.to_string();
            }
        });
        return;
    }

    let mut delegation = Delegation::observed(session, agent, task, tool_use_id);
    delegation.cwd = cwd;
    delegation.isolation = isolation;
    delegation.transcript_path = transcript;
    state.upsert_delegation(delegation);
}

/// Find a pending `agent_delegate` declaration this dispatch should merge into.
///
/// Why: a PM instructed to call `agent_delegate` *and* actually dispatch would
/// otherwise produce two records for one subagent, inflating the live-children
/// count and leaving an immortal `Queued` orphan (nothing would ever terminalize
/// the declaration, since only the observed record carries the correlation
/// keys).
/// What: the most recent same-session, same-agent delegation that is still
/// `Queued`, was `McpDeclared`, has not already been bound to a `tool_use_id`,
/// was created within [`DEDUP_WINDOW_SECS`], and whose task [`tasks_match`]es
/// this dispatch's description.
///
/// The agent name alone is NOT a sufficient key (#2864 review, MEDIUM 2): this
/// repo dispatches several `rust-engineer`s concurrently, so "same agent,
/// recently" false-merges two distinct dispatches — halving the live-child count
/// and leaving the merged record showing the *declaration's* task text, which is
/// precisely the string `/tm-session-pause` puts in front of a human. When in
/// doubt this now declines to merge: two records for one subagent overcounts and
/// is corrected by the staleness sweep, whereas a false merge undercounts work
/// in flight and mislabels it, with no recovery.
/// Test: `dedups_declaration_and_observation`, `dedup_window_expires`,
/// `dedup_ignores_different_agent`, `dedup_does_not_merge_a_different_task`.
fn dedup(state: &DaemonState, session: SessionId, agent: &str, task: &str) -> Option<DelegationId> {
    let cutoff = Utc::now() - chrono::Duration::seconds(DEDUP_WINDOW_SECS);
    state.latest_delegation_matching(session, |d| {
        d.source == DelegationSource::McpDeclared
            && d.status == DelegationStatus::Queued
            && d.tool_use_id.is_none()
            && d.agent == agent
            && d.created_at >= cutoff
            && tasks_match(&d.task, task)
    })
}

/// Do a declared task and a dispatched description describe the same work?
///
/// Why: the task text is the only discriminator available between two dispatches
/// to the same agent inside the dedup window, and it must tolerate the PM
/// wording the `agent_delegate` task and the dispatch `description` slightly
/// differently while still rejecting two plainly different work items.
/// What: case- and whitespace-insensitive prefix match in either direction
/// (which subsumes equality), so a short dispatch description matches the longer
/// declaration it summarises.
///
/// The two empty cases are NOT symmetric (#2864 re-review, LOW 1). A dispatch
/// with no description offers nothing to discriminate on, and merging it would
/// keep the *declaration's* text as the label for a dispatch we cannot identify
/// — precisely the mislabel this discriminator exists to prevent — so it
/// declines. An unlabelled *declaration* is different: merging adopts the
/// dispatch's own text (`on_dispatch` fills an empty task), so no wrong label
/// can survive, and it merges.
/// Test: `dedups_declaration_and_observation`,
/// `dedup_does_not_merge_a_different_task`,
/// `dedup_declines_a_description_less_dispatch`.
fn tasks_match(declared: &str, dispatched: &str) -> bool {
    if dispatched.is_empty() {
        return false;
    }
    if declared.is_empty() {
        return true;
    }
    let normalize = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let (a, b) = (normalize(declared), normalize(dispatched));
    a.starts_with(&b) || b.starts_with(&a)
}

/// `PostToolUse` on a dispatch tool: learn `agentId`, terminalize only if the
/// dispatch really returned.
///
/// Why: see the module note — an async dispatch returns in ~1 ms while the
/// subagent runs on, so this is primarily the *join* that binds `tool_use_id` to
/// the `agent_id` that `SubagentStop` will quote.
/// What: locates the delegation by `tool_use_id`, stores
/// `tool_response.agentId`, and refines the tier from `resolvedModel`. Whether
/// to terminalize is decided entirely by [`classify_dispatch`], which is a
/// three-way answer — `Launched` / `Returned` / `Unknown` — not a boolean.
/// Test: `async_launch_keeps_delegation_running`,
/// `synchronous_post_tool_use_completes_delegation`,
/// `post_tool_use_failure_marks_failed`,
/// `post_tool_use_without_tool_response_stays_running`,
/// `post_tool_use_with_unrecognized_response_stays_running`,
/// `post_tool_use_with_only_an_agent_id_stays_running`,
/// `changed_async_status_value_with_an_agent_id_stays_running`,
/// `null_agent_id_does_not_launch_a_phantom_delegation`,
/// `empty_string_agent_id_does_not_launch_a_phantom_delegation`.
fn on_launched(state: &DaemonState, session: SessionId, payload: &Value, event: HookEvent) {
    let Some(tool_use_id) = field(payload, "tool_use_id") else {
        return;
    };
    let Some(id) =
        state.find_delegation(session, |d| d.tool_use_id.as_deref() == Some(tool_use_id))
    else {
        return;
    };

    // Only a response we actually recognized carries evidence either way.
    let response = payload
        .get("tool_response")
        .filter(|r| recognized_response(r));
    // #4163: use the same extraction `classify_dispatch` uses for its
    // presence-check, so a null/empty `agentId` is never stored as a handle
    // nothing can ever resolve.
    let agent_id = response.and_then(usable_agent_id).map(str::to_string);
    let tier = response
        .and_then(|r| r.get("resolvedModel"))
        .and_then(Value::as_str)
        .map(ModelTier::from_model_id);

    // The dispatch call itself failed: no subagent was left running.
    let errored = event == HookEvent::PostToolUseFailure
        || response
            .and_then(|r| r.get("is_error"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let outcome = classify_dispatch(response);

    state.mutate_delegation(id, |d| {
        if agent_id.is_some() {
            d.agent_id = agent_id.clone();
        }
        if let Some(t) = tier {
            d.tier = t;
        }
        // Fail CLOSED. `Launched` never terminalizes, whatever else the event
        // says — a running subagent outranks an errored dispatch call, and
        // `SubagentStop` will close it. `Unknown` terminalizes only on an
        // explicit dispatch failure, which is itself positive evidence that
        // nothing was left running.
        let terminal = match outcome {
            DispatchOutcome::Launched => None,
            DispatchOutcome::Returned => Some(if errored {
                DelegationStatus::Failed
            } else {
                DelegationStatus::Completed
            }),
            DispatchOutcome::Unknown if errored => Some(DelegationStatus::Failed),
            DispatchOutcome::Unknown => None,
        };
        if let Some(status) = terminal {
            d.status = status;
            d.ended_at = Some(Utc::now());
        }
    });
}

/// What a dispatch `PostToolUse` response says about whether a subagent is
/// still running.
///
/// Why this is three-valued and not a boolean: "I could parse this response"
/// and "this response says nothing was left running" are different
/// propositions, and the first round of #2864 review was about exactly that
/// conflation one layer up. `Unknown` is a first-class answer here for the same
/// reason [`DelegationStatus::Stale`] is a first-class status: not knowing is
/// not the same as knowing it finished.
/// Test: the `post_tool_use_*` suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchOutcome {
    /// A subagent is running now — an explicit async marker, or an `agentId`
    /// handle for a future `SubagentStop` to quote.
    Launched,
    /// The call returned rather than launching; nothing is left running.
    Returned,
    /// The response is absent or unrecognized — no conclusion is available.
    Unknown,
}

/// Classify a dispatch response (#2864 review HIGH 1, re-review MEDIUM 1).
///
/// Why: an absent response used to read as "not async, therefore finished",
/// terminalizing a still-running subagent ~1 ms after launch — and absence is
/// not hypothetical, since `tm hook`'s S1 projection ([`TOOL_RESPONSE_KEYS`])
/// omits `tool_response` entirely when the payload is not an object or carries
/// none of those keys. That is [`DispatchOutcome::Unknown`].
///
/// The re-review then found a narrower conflation: a response holding only
/// `agentId` produced `Completed` *and* stored the `agent_id` in the same
/// breath — recording "expect a `SubagentStop` for this agent" and "this
/// already finished" simultaneously. Hence the launch test comes first and
/// includes `agentId`.
///
/// What, in order:
///
/// - `Launched` — `isAsync == true`, `status == "async_launched"`, or a
///   *usable* `agentId` ([`usable_agent_id`]: non-null, non-empty) is present.
///   An `agentId` is by design the *async* correlation key; handing one back
///   is evidence of a launch, never of a return. Testing it first is also
///   what makes a `status` whose **value** drifted (rather than its key)
///   safe, since such a response still carries its `agentId`. A `null` or
///   `""` `agentId` is not a usable handle (#4163) — nothing downstream could
///   ever resolve it — so it does not take this branch.
/// - `Returned` — any other recognized response.
/// - `Unknown` — absent, not an object, or carrying no [`TOOL_RESPONSE_KEYS`]
///   member.
///
/// # KNOWN LIMITATION — a response with no usable `agentId` is read as a return
///
/// `Returned` is deliberately still *residual* rather than affirmative
/// (`isAsync == false` or a non-launch `status`), so a recognized response that
/// is silent about liveness — `resolvedModel` alone, `is_error` alone — is
/// treated as a synchronous return. That is a real fail-open band and it is
/// **knowingly left open** here, because closing it would make things worse
/// today, not better:
///
/// [`on_subagent_stop`] resolves a delegation *only* by `agent_id`, and
/// `agent_id` is taught *only* by this function. A response with no *usable*
/// `agentId` — absent, `null`, `""`, or any non-string ([`usable_agent_id`])
/// therefore has **no** route to termination other than this branch — nothing
/// can ever quote it back to us. Compounding that, `PostToolUse` is installed
/// `async: true` while `SubagentStop` is synchronous
/// (`core::standalone::hooks`), so the two are independent `tm hook` processes
/// whose arrival order at the daemon is not guaranteed; an out-of-order stop
/// matches nothing and nothing re-checks. Making this branch affirmative without
/// first giving `on_subagent_stop` a recovery path would convert a rare
/// fail-open into a guaranteed six-hour phantom "agent in flight" for every such
/// dispatch.
///
/// The band is unobserved: Claude Code 2.1.220 returns
/// `{isAsync, status, agentId, resolvedModel}` for a dispatch, which takes the
/// `Launched` branch. Tightening is blocked on the stop-side recovery work and
/// is tracked with it, not attempted here.
/// Test: `post_tool_use_without_tool_response_stays_running`,
/// `post_tool_use_with_unrecognized_response_stays_running`,
/// `post_tool_use_with_only_an_agent_id_stays_running`,
/// `changed_async_status_value_with_an_agent_id_stays_running`,
/// `synchronous_post_tool_use_completes_delegation`,
/// `liveness_silent_response_is_read_as_a_return_known_gap`,
/// `null_agent_id_does_not_launch_a_phantom_delegation`,
/// `empty_string_agent_id_does_not_launch_a_phantom_delegation`.
fn classify_dispatch(response: Option<&Value>) -> DispatchOutcome {
    let Some(r) = response else {
        return DispatchOutcome::Unknown;
    };
    if r.get("isAsync").and_then(Value::as_bool) == Some(true)
        || r.get("status").and_then(Value::as_str) == Some("async_launched")
        // #4163: was `r.get("agentId").is_some()` — key presence only, so a
        // `null` or `""` agentId took this branch even though nothing
        // downstream could ever resolve it. Use the same extraction the
        // consumer in `on_launched` uses.
        || usable_agent_id(r).is_some()
    {
        return DispatchOutcome::Launched;
    }
    DispatchOutcome::Returned
}

/// Is this `tool_response` one we can draw a conclusion from?
///
/// Why: `tm hook`'s S1 projection already restricts the forwarded object to
/// [`TOOL_RESPONSE_KEYS`], but the daemon must not depend on a filter living in
/// a different crate target for its fail-direction. Re-asserting it here means a
/// more permissive S1 (a raw string, a content-block array) cannot silently turn
/// "unrecognized" back into "understood".
/// What: `true` iff `response` is an object carrying at least one
/// [`TOOL_RESPONSE_KEYS`] member.
/// Test: `post_tool_use_with_unrecognized_response_stays_running`.
fn recognized_response(response: &Value) -> bool {
    response
        .as_object()
        .is_some_and(|o| TOOL_RESPONSE_KEYS.iter().any(|k| o.contains_key(*k)))
}

/// `SubagentStop`: terminalize exactly the delegation that ended.
///
/// Why: this is the authoritative completion signal, and the only one that
/// arrives when the subagent actually finishes.
/// What: resolves `payload.agent_id` against the stored `agent_id` and
/// terminalizes that record alone (`Failed` for `SubagentStopFailure`, else
/// `Completed`). When `agent_id` is absent, or matches nothing, **nothing is
/// terminalized** — see the module note on why a "most recent" fallback is
/// forbidden.
/// Test: `subagent_stop_completes_matching_delegation`,
/// `subagent_stop_without_agent_id_terminalizes_nothing`,
/// `concurrent_delegations_terminalize_independently`.
fn on_subagent_stop(state: &DaemonState, session: SessionId, payload: &Value, event: HookEvent) {
    let Some(agent_id) = field(payload, "agent_id") else {
        tracing::trace!(
            "SubagentStop without agent_id — no delegation terminalized (#2864); a \
             'most recent' guess would close the wrong one under concurrency"
        );
        return;
    };
    // `Stale` is deliberately not terminal, so a stop that arrives after the
    // staleness sweep gave up still replaces "we lost track" with the truth.
    let Some(id) = state.find_delegation(session, |d| {
        d.agent_id.as_deref() == Some(agent_id) && !d.status.is_terminal()
    }) else {
        return;
    };
    let status = if event == HookEvent::SubagentStopFailure {
        DelegationStatus::Failed
    } else {
        DelegationStatus::Completed
    };
    state.terminate_delegation(id, status);
}

#[cfg(test)]
#[path = "delegation_tracker_tests.rs"]
mod tests;
