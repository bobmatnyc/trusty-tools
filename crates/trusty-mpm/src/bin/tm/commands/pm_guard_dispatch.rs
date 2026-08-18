//! `tm hook --pm-guard` — concurrent shared-working-tree dispatch denial (#4480).
//!
//! Why: an `Agent`/`Task` subagent inherits the dispatching PM's working
//! directory. Two concurrent file-mutating subagents in that one directory race
//! over a single git HEAD, and git does not stop them — `git checkout -b`
//! refuses only when a *tracked* file differs between both branches' committed
//! versions AND carries an uncommitted local change, so untracked new files and
//! edits to files the two branches do not differ on transfer silently. That is
//! the shape of two mostly-disjoint feature branches, which is the ordinary
//! dispatch shape. It was observed live on tm 1.3.0: `git status` showed the PM
//! on one agent's branch with the other agent's changes sitting beside it. The
//! `isolation: "worktree"` escape hatch existed, was opt-in, and nothing
//! enforced it.
//!
//! What: [`evaluate_shared_tree_dispatch`] denies a dispatch when, and only
//! when, ALL of the following hold:
//!
//! 1. the tool is a `SUBAGENT_DISPATCH_TOOLS` member;
//! 2. the caller is the PM, not a subagent (a subagent's dispatch is already
//!    denied outright by [`super::pm_guard_fanout`], so this never reaches it);
//! 3. the dispatch names a bundled engineer-tier agent
//!    (`agent_mutates_files`) and declares no isolation
//!    (`isolation_separates_working_tree`);
//! 4. the daemon reports at least one OTHER live delegation already doing the
//!    same thing in the same directory.
//!
//! Condition 4 is why this asks the daemon at all. The alternative — a
//! hook-local ledger with a time window — cannot answer "is the other agent
//! still running?", because the Agent tool dispatches in the background by
//! default: a sequential second dispatch can legitimately arrive minutes after
//! the first and still be concurrent, while a short window would miss it and a
//! long one would deny a genuinely sequential dispatch. The daemon resolves
//! liveness from real `SubagentStop` signals, so it is asked rather than
//! guessed at.
//!
//! **The question and the claim are one daemon operation (#5324).** This used
//! to be check-then-decide: the daemon was queried and the verdict computed
//! from the answer, with nothing claiming the directory in between, so two
//! dispatches issued in one PM turn could both query before either's
//! `on_dispatch` recording landed, both see an empty set, and both be ALLOWED —
//! and a PM dispatching several agents in one message is the framework's own
//! documented pattern for parallel work, so that was the common shape, not an
//! edge case. [`claim_shared_tree`] now POSTs the dispatch instead of asking
//! about it: the daemon answers and, when the answer is empty, records this
//! dispatch inside the same critical section. The second of two simultaneous
//! dispatches therefore sees the first and is denied.
//!
//! What the daemon records is not a new kind of state — it is the delegation
//! record its own `matcher: "*"` PreToolUse hook would have written for the
//! same dispatch a moment later, and that hook is idempotent on `tool_use_id`,
//! so exactly one record exists either way and its lifecycle is unchanged.
//!
//! Residual, unchanged from #4480: if the daemon's tracker records BOTH
//! dispatches before either guard's claim arrives, each guard sees the other and
//! both are denied. That ordering predates this module and is not made more
//! likely by it; the remedy the deny prints (declare isolation) resolves it.
//!
//! **This guard fails open where a failure says the guard cannot run, and
//! CLOSED where it only says the answer did not arrive (#5923).** An
//! unresolvable cwd, an unrecognised agent name, an untyped dispatch, and a
//! daemon that is not listening all allow — the first three never reach the
//! daemon, and the fourth means no delegation can be registered through it at
//! all, so a deny would block every dispatch on a machine with no daemon
//! running. That arm now prints a warning rather than passing silently.
//!
//! A daemon that IS listening and does not answer usably — a request timeout, a
//! 5xx, a body that does not parse — is the opposite case: a writer may already
//! be recorded and the reply simply did not arrive. Those admitted the dispatch
//! until #5923, and the 2 s budget is reachable on an idle machine, so the
//! guard switched itself off under exactly the load that makes two concurrent
//! dispatches likely; two simultaneous dispatches were measured both admitted.
//! [`claim_shared_tree`] now denies there. The asymmetry between the two is
//! deliberate: a false DENY lands on the PM and halts every dispatch in the
//! system, so it is spent only where the guard's own question is genuinely open.
//!
//! **A second caller reads the same answer without claiming (ADR-0048
//! decision 10).** [`live_shared_tree_writers`] is the query half on its own,
//! for the Bash rule that denies a HEAD-moving `pull`/`merge`/`rebase` in a
//! main checkout another session's writer is standing in. It shares this
//! module's route, projection, and timeouts, and claims nothing: the route
//! re-derives eligibility from the payload, and a `Bash` call is not a dispatch
//! tool, so the record closure never runs.
//!
//! **A third caller records what the guard GRANTED, on a route of its own
//! (#5769).** [`evaluate_granted_worktree`] serves the ADR-0048 worktree grant,
//! and it needs the opposite of what the route above provides: the grant's whole
//! point is that the dispatch now declares isolation, which is exactly the
//! payload `shares_the_callers_tree` rejects, so this route's record closure
//! could never run for it. `…/delegations/granted-worktree` inverts the
//! eligibility test and upserts instead of observing. Posting the ORIGINAL input
//! here instead would be worse than doing nothing: eligibility would pass, an
//! empty answer would CLAIM the directory, and the unisolated record the grant
//! exists to correct would be written by the correcting call itself.
//!
//! Cost: the daemon call is made only after the dispatch itself is classified as
//! a shared-tree writer, so a research, review, or QA dispatch — and every
//! non-dispatch tool call, which is essentially all traffic — pays nothing.
//!
//! Test: `denies_a_second_concurrent_unisolated_engineer`,
//! `allows_the_first_dispatch`, `allows_an_isolated_dispatch`,
//! `allows_a_read_only_agent`, `allows_when_the_agent_is_unknown`,
//! `allows_every_non_dispatch_tool` below. Each declared fail-open branch has an
//! error-arm test: unreachable daemon
//! (`claim_shared_tree_is_empty_when_the_daemon_is_unreachable`), a daemon
//! without the route (`claim_is_empty_when_the_daemon_has_no_such_route`), an
//! answer of the wrong shape
//! (`pm_guard_allows_when_the_daemon_answer_has_the_wrong_shape`),
//! unresolvable cwd (`evaluate_allows_when_the_cwd_cannot_be_resolved`),
//! unknown agent (`allows_when_the_agent_is_unknown`), untyped dispatch (same),
//! and non-dispatch tool (`allows_every_non_dispatch_tool`). The three
//! fail-CLOSED arms have theirs too: `claim_is_unknown_when_the_daemon_times_out`,
//! `claim_is_unknown_when_the_daemon_answers_500`, and
//! `claim_is_unknown_when_the_body_does_not_parse`, each of which allowed before
//! #5923.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::agent::is_subagent_dispatch_tool;
use trusty_mpm::core::dispatch_isolation::{
    dispatch_agent, dispatch_isolation, shares_the_callers_tree,
};

use crate::commands::hook_payload::build_hook_payload;

/// Build the deny message for a blocked concurrent dispatch.
///
/// Why: a bare "denied" leaves the model guessing and it retries the identical
/// call. The text has to name what is already running and say why git will not
/// catch the collision (the reader's prior is that it would). It offers exactly
/// ONE remedy — declare isolation — because that is the only one that always
/// works: `RUNNING_STALE_AFTER_SECS` is six hours, so a crashed subagent that
/// never emits `SubagentStop` holds its directory for that whole window, and
/// "wait for it to report back" would be advice to wait for something that may
/// never happen. Built per call rather than kept as a constant because naming
/// the actual sibling agent is most of its value.
///
/// #5649: the incident showed that single remedy can itself be unavailable, so
/// the message now names a second one — serialize. Serializing and waiting are
/// not the same offer: serializing means dispatching one file-mutating agent at
/// a time GOING FORWARD, which needs nothing from the agent already running, so
/// it always works. Waiting blocks on an agent that may never return, and stays
/// excluded for exactly the reason above.
/// What: a single-paragraph `permissionDecisionReason`.
/// Test: `denies_a_second_concurrent_unisolated_engineer`,
/// `deny_reason_offers_only_remedies_that_always_work`.
fn deny_reason(agent: &str, cwd: &Path, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let running = names.join(", ");
    format!(
        "Concurrent shared-worktree dispatch denied (#4480): {running} is already running in \
         {} without a worktree of its own — possibly dispatched by a different session standing \
         in the same directory (ADR-0048) — and this {agent} dispatch would put a second \
         file-mutating agent on the same git HEAD. Git does not catch this — a `git checkout -b` \
         refuses only when a tracked file differs between both branches AND has an uncommitted \
         change, so untracked files and edits the two branches agree on transfer onto the wrong \
         branch silently, with no error at any step. Re-dispatch this agent with \
         `isolation: \"worktree\"` so it gets its own tree. If isolation is unavailable here, \
         serialize instead: dispatch one file-mutating agent at a time from now on. Do not \
         hand-roll a `git worktree add` in the prompt — this guard reads the declared \
         isolation parameter, never the prompt, so a self-made worktree still counts as \
         sharing this HEAD (#5649).",
        cwd.display()
    )
}

/// Classify one dispatch against the set of agents already writing in this tree.
///
/// Why: kept pure — it takes the daemon's answer as a slice rather than fetching
/// it — so the whole policy is exhaustively unit-testable with no network, and
/// the I/O lives in [`claim_shared_tree`] next door.
/// What: `Some(reason)` (DENY) when `tool_name` is a dispatch tool, the dispatch
/// [`shares_the_callers_tree`], and `live` is non-empty. `None` (ALLOW) in every
/// other case, including every non-dispatch tool.
/// Test: `denies_a_second_concurrent_unisolated_engineer`,
/// `allows_the_first_dispatch`, `allows_an_isolated_dispatch`,
/// `allows_a_read_only_agent`, `allows_every_non_dispatch_tool`.
pub(crate) fn evaluate_shared_tree_dispatch(
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: &Path,
    live: &[String],
) -> Option<String> {
    if live.is_empty() || !dispatch_shares_the_tree(tool_name, tool_input) {
        return None;
    }
    let agent = dispatch_agent(tool_input).unwrap_or("this");
    Some(deny_reason(agent, cwd, live))
}

/// Would this tool call put a file-mutating agent into the caller's own tree?
///
/// Why: the cheap predicate that gates the daemon call, so a read-only or
/// isolated dispatch — and every ordinary tool call — costs nothing.
/// What: `true` when `tool_name` is a dispatch tool AND the named agent
/// [`shares_the_callers_tree`] under the declared isolation.
/// Test: `allows_a_read_only_agent`, `allows_an_isolated_dispatch`,
/// `allows_when_the_agent_is_unknown`.
pub(crate) fn dispatch_shares_the_tree(tool_name: &str, tool_input: Option<&Value>) -> bool {
    is_subagent_dispatch_tool(tool_name)
        && dispatch_agent(tool_input)
            .is_some_and(|agent| shares_the_callers_tree(agent, dispatch_isolation(tool_input)))
}

/// The directory a dispatch from this hook would land in.
///
/// Why: it must be resolved the SAME way the daemon's recorder resolves it, or
/// the two never match and the guard is a silent no-op. `tm hook` stamps a
/// delegation's `cwd` from its own `std::env::current_dir()` — both hooks are
/// spawned by the harness with the session's directory — so this reads that
/// first and falls back to the payload's `cwd` field only when the process
/// cannot answer.
/// What: `std::env::current_dir()`, else `payload.cwd`, else `None` (which
/// fails open: no directory, no comparison, no deny).
/// Test: `dispatch_cwd_prefers_the_process_directory`.
pub(crate) fn dispatch_cwd(payload: &Value) -> Option<PathBuf> {
    resolve_dispatch_cwd(std::env::current_dir().ok(), payload)
}

/// [`dispatch_cwd`] with the process directory injected.
///
/// Why: `std::env::current_dir()` fails only when the directory has been
/// deleted or made unreadable out from under the process — a state a test
/// cannot enter portably (deleting a live cwd is a no-op on Windows, and on
/// macOS the descriptor survives the unlink, so the call still succeeds). That
/// unreachable-by-test branch is exactly the one the fail-open contract depends
/// on, so the resolution is taken as a parameter instead of read here. This
/// mirrors `pm_guard_bash::PathEnv::from_process` and
/// `pm_guard_deny_by_default::persona_status_for_session`, which split the same
/// way for the same reason.
/// What: `process_cwd` when present, else `payload.cwd` when non-empty, else
/// `None`. [`dispatch_cwd`] is the only caller that reads the real environment,
/// so production behavior is unchanged.
/// Test: `resolve_dispatch_cwd_falls_back_to_the_payload`,
/// `resolve_dispatch_cwd_is_none_when_nothing_resolves`,
/// `evaluate_allows_when_the_cwd_cannot_be_resolved`.
pub(crate) fn resolve_dispatch_cwd(
    process_cwd: Option<PathBuf>,
    payload: &Value,
) -> Option<PathBuf> {
    process_cwd.or_else(|| {
        payload
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    })
}

/// Narrow `payload["input"]` to the three fields the daemon actually reads.
///
/// Why: an `Agent` dispatch's `tool_input` carries the whole subagent prompt,
/// which is unbounded — a long brief is ordinary, not pathological. Forwarding
/// it verbatim would put this POST under axum's default 2 MB body limit and
/// create a failure arm the old read-only GET could not have: a 413 returns an
/// empty answer, so the dispatch is admitted having claimed nothing, and the
/// guard is silently off for exactly the largest dispatches. Rather than test
/// that arm, this removes it — the body is now bounded by an agent name, an
/// isolation mode, and a description.
///
/// The record stays byte-identical: the route reads only `subagent_type`
/// ([`dispatch_agent`]) and `isolation` ([`dispatch_isolation`]), and
/// `delegation_tracker::on_dispatch` stores only those two plus `description`.
/// Nothing downstream reads another key of `input`, so dropping the rest cannot
/// change what is written.
/// What: replaces `input` with an object of just those three keys, each copied
/// only when present. A payload with no `input`, or a non-object one, is left
/// exactly as it is — there is nothing to narrow and inventing a shape here
/// could only make the daemon's classification disagree with the guard's.
/// Test: `claim_payload_carries_only_the_fields_the_daemon_reads`,
/// `claim_payload_projection_leaves_a_non_object_input_alone`.
fn project_dispatch_input(forwarded: &mut Value) {
    const FORWARDED_INPUT_FIELDS: [&str; 3] = ["subagent_type", "isolation", "description"];

    let Some(input) = forwarded.get("input").filter(|i| i.is_object()) else {
        return;
    };
    let mut projected = serde_json::Map::new();
    for key in FORWARDED_INPUT_FIELDS {
        if let Some(value) = input.get(key) {
            projected.insert(key.to_string(), value.clone());
        }
    }
    forwarded["input"] = Value::Object(projected);
}

/// Claim `cwd` for this dispatch, and learn who already holds it (#5324).
///
/// Why: see the module doc — liveness is the daemon's to answer, and it is the
/// only process that resolves it from real terminal signals. It is also the only
/// place the answer and the claim can be made indivisible: a hook process that
/// asked and then acted would leave the window two same-turn dispatches slip
/// through.
/// What: `POST <url>/api/v1/sessions/{session}/delegations/shared-tree-dispatch`
/// carrying this dispatch in the daemon's own forwarded hook shape (built by the
/// one [`build_hook_payload`], so the record the daemon writes is the record its
/// own `matcher: "*"` hook would write) with `cwd` stamped to the directory the
/// guard resolved and `input` narrowed by [`project_dispatch_input`] to the
/// three fields the daemon reads — the dispatch prompt never travels, so the
/// body stays small enough that a size limit is not a reachable failure arm.
/// Sent under the same tight connect/total bounds `pm_guard`'s
/// audit POSTs use (500 ms / 2 s) — this call sits inside a `PreToolUse` budget,
/// so a slow daemon must cost a bounded wait and nothing more.
///
/// **The failure arms are not one outcome (#5923).** They used to be: client
/// build, transport, non-2xx and malformed body all returned an empty vec,
/// which the pure policy above reads as ALLOW. A request that timed out
/// therefore admitted the dispatch, and the 2 s budget is reachable on an idle
/// machine — so the guard was off under exactly the load that makes two
/// concurrent dispatches likely, and two simultaneous dispatches were measured
/// both admitted. What the daemon's silence MEANS now decides the verdict: no
/// daemon at all is a degraded mode that allows and warns, while a daemon that
/// is running and did not answer leaves the question open and denies.
/// Test: `claim_shared_tree_is_empty_when_the_daemon_is_unreachable`,
/// `claim_is_unknown_when_the_daemon_times_out`,
/// `claim_is_unknown_when_the_daemon_answers_500`.
pub(crate) async fn claim_shared_tree(
    url: &str,
    session_id: &str,
    cwd: &Path,
    payload: &Value,
) -> SharedTreeClaim {
    match post_shared_tree(url, session_id, cwd, payload, SHARED_TREE_ROUTE).await {
        SharedTreeReply::Answered(body) => {
            let live = writers_in(&body);
            warn_on_eligibility_divergence(&body, &live);
            SharedTreeClaim::Writers(live)
        }
        // #5923: no daemon to ask, so the guard cannot function here at all —
        // allow, but never silently.
        SharedTreeReply::Unavailable(detail) => {
            warn_guard_unavailable(&detail);
            SharedTreeClaim::Writers(Vec::new())
        }
        SharedTreeReply::Unanswered(detail) => SharedTreeClaim::Unknown(detail),
    }
}

/// What one claim learned about who is already writing in the directory.
///
/// Why: the caller needs three outcomes and a `Vec<String>` carries two —
/// writers and no writers. Folding "no answer" into the empty vec IS the #5923
/// defect, so the third outcome is a variant rather than a convention.
/// What: [`Self::Writers`] is the daemon's answer, empty or not.
/// [`Self::Unknown`] carries why no answer arrived, for the deny message.
/// Test: `claim_is_unknown_when_the_daemon_times_out`,
/// `claim_is_unknown_when_the_daemon_answers_500`.
pub(crate) enum SharedTreeClaim {
    /// The daemon answered: these agents are already writing here.
    Writers(Vec<String>),
    /// A running daemon did not answer, so whether a writer is registered here
    /// is unknown.
    Unknown(String),
}

/// Warn that the guard is not enforcing, and why (#5923).
///
/// Why: allowing on an absent daemon is a deliberate degraded mode — a false
/// DENY lands on the PM and halts every dispatch in the system, and a daemon
/// nobody started is an ordinary state. What kept the fail-open invisible is
/// that the degraded mode was SILENT: an operator with no daemon running saw a
/// guard that looked like it was working.
/// What: one stderr line, which Claude Code surfaces without it reaching the
/// hook's stdout verdict — the same channel and the same reasoning as
/// [`warn_on_eligibility_divergence`].
/// Test: `pm_guard_warns_when_no_daemon_answers_the_claim` in
/// `tests/tm_hook_pm_guard.rs`.
fn warn_guard_unavailable(detail: &str) {
    eprintln!(
        "tm hook --pm-guard: no daemon answered the shared-tree claim (#5923) — {detail}. \
         Concurrent shared-worktree dispatch is NOT being enforced for this dispatch: a second \
         file-mutating agent can join this git HEAD, and git reports nothing when it does. Start \
         the daemon (`tm start`) to restore the guard. Allowing the dispatch: an absent daemon is \
         a degraded mode this path accepts deliberately, unlike a daemon that is running and did \
         not answer, which denies."
    );
}

/// Build the deny message for a claim a running daemon left unanswered (#5923).
///
/// Why: [`deny_reason`] names the sibling already in the tree, and here there is
/// no name to give — the point is precisely that nobody could say. A reader
/// handed that message would go looking for an agent that may not exist.
/// What: names the failure, the directory, and the two remedies that need no
/// daemon answer — declare isolation (an isolated dispatch never reaches this
/// route at all) or serialize — plus how to check the daemon.
/// Test: `unanswered_deny_reason_names_the_failure_and_offers_isolation`.
fn unanswered_deny_reason(agent: &str, cwd: &Path, detail: &str) -> String {
    format!(
        "Concurrent shared-worktree dispatch denied (#5923): the daemon did not answer this \
         guard's claim for {} — {detail}. That answer is the only thing that can say whether \
         another file-mutating agent is already writing in this directory, so admitting this \
         {agent} dispatch would put a second agent on one git HEAD with the check that exists to \
         stop it never having run. A timeout or an error from a running daemon is correlated with \
         load, which is when a concurrent dispatch is most likely, so this denies rather than \
         assuming the tree is free. Re-dispatch this agent with `isolation: \"worktree\"` — an \
         isolated dispatch never needs this answer and is never blocked by it. If isolation is \
         unavailable here, serialize instead: dispatch one file-mutating agent at a time. If the \
         daemon is unhealthy, `tm doctor` reports it and `tm restart` clears it.",
        cwd.display()
    )
}

/// Who is already writing in `cwd`, asked without claiming it (ADR-0048
/// decision 10).
///
/// Why: the HEAD-moving Bash rule needs the same directory-keyed answer the
/// dispatch guard needs — `DaemonState::live_shared_tree_writers` is keyed by
/// DIRECTORY rather than by session precisely so a writer another session put
/// in this checkout is visible — but it must not take the directory: a `git
/// pull` is not a dispatch, and recording one as a delegation would occupy a
/// tree nothing will ever release. It goes through the same route rather than a
/// second one so both callers read one answer built one way.
/// What: POSTs the Bash call's own payload to the shared-tree route. The route
/// re-derives eligibility from `tool` and the agent name, and `Bash` is not a
/// dispatch tool, so `claim_shared_tree_dispatch` is handed `eligible = false`
/// and its record closure never runs — the call is a pure read by construction,
/// not by the caller's promise. Every failure arm of [`post_shared_tree`]
/// returns an empty vec, which the caller reads as ALLOW.
/// Test: `shared_tree_dispatch_route_answers_a_bash_query_without_claiming` in
/// `crate::daemon::delegation_routes` pins the no-claim half daemon-side;
/// `pm_guard_allows_a_pull_when_the_daemon_is_unreachable` in
/// `tests/tm_hook_pm_guard.rs` pins the fail-open half.
pub(crate) async fn live_shared_tree_writers(
    url: &str,
    session_id: &str,
    cwd: &Path,
    payload: &Value,
) -> Vec<String> {
    // #5923: the fail-closed arm is the CLAIM path's alone. This one gates
    // `git merge`/`git rebase`/a docs commit, where denying on a daemon that
    // did not answer would block ordinary git work on the operator's own
    // checkout — a far wider blast radius than one dispatch, and not the
    // failure #5923 reported.
    post_shared_tree(url, session_id, cwd, payload, SHARED_TREE_ROUTE)
        .await
        .answered()
        .as_ref()
        .map(writers_in)
        .unwrap_or_default()
}

/// Who is writing in any of `dirs`, asked without claiming (#5769).
///
/// Why: the HEAD-move rule keys its query by directory, but the directory a
/// delegation record carries and the directory the command runs in are resolved
/// by different code — `tm hook` stamps a record from its own process
/// `current_dir()`, while the guard resolves the command's target through `cd`
/// and `git -C`. `cd crates/foo && git pull` from a checkout root therefore
/// queried `/repo/crates/foo` against records written at `/repo` and matched
/// nothing, allowing the move. Both directories name the same HEAD, so both are
/// asked.
/// What: queries each distinct directory in order and STOPS at the first
/// non-empty answer — the deny needs one positive answer, not a complete
/// census, so the common deny path still costs one round trip. Every failure arm
/// of [`post_shared_tree`] contributes nothing, so an unreachable daemon still
/// answers "nobody here".
/// Test: `head_move_query_asks_the_checkout_root_and_the_command_directory` in
/// `tests/tm_hook_pm_guard.rs`.
pub(crate) async fn live_shared_tree_writers_in(
    url: &str,
    session_id: &str,
    dirs: &[&Path],
    payload: &Value,
) -> Vec<String> {
    let mut asked: Vec<&Path> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if asked.contains(dir) {
            continue;
        }
        asked.push(dir);
        let live = live_shared_tree_writers(url, session_id, dir, payload).await;
        if !live.is_empty() {
            return live;
        }
    }
    Vec::new()
}

/// Record the isolation the guard just granted, and learn who holds the
/// checkout (#5769).
///
/// Why: two problems with one shape. The daemon's `matcher: "*"` tracker records
/// this dispatch from the ORIGINAL payload, so a granted writer stays recorded
/// as writing unisolated in the main checkout and ADR-0048 decision 10 then
/// denies a `git merge` or `git rebase` there on a phantom for six hours (it
/// denied `git pull` too, until ADR-0053). And the #4480 concurrency
/// check no longer runs on a granted dispatch at all, because the grant returns
/// first — so if the harness does not apply `updatedInput`, a second unisolated
/// writer that used to be denied is now admitted. Both need the daemon, and both
/// need it BEFORE the grant is emitted, so they are one call.
///
/// It posts to [`GRANTED_WORKTREE_ROUTE`] rather than the sibling one because
/// that route re-derives eligibility with `shares_the_callers_tree`, which the
/// rewritten input makes false — the record closure would never run. Posting the
/// ORIGINAL input there instead would be worse than useless: eligibility would
/// be true, an empty answer would CLAIM the directory, and the phantom this
/// exists to remove would be recorded by the very call meant to correct it.
/// What: `Some(reason)` (DENY) when the daemon names another live writer in this
/// checkout; `None` (emit the grant) when it does not, in which case the daemon
/// has recorded the granted isolation inside the same critical section that
/// produced the answer. Every daemon failure answers empty, so a down daemon
/// grants — the fail-open direction the sibling guard commits to.
///
/// **An empty answer is not proof the grant was recorded, so it is checked
/// rather than assumed.** Reading only the writer list would make every way the
/// daemon can decline to record — an unparseable body, a 404 from a daemon
/// older than this route, an eligibility test this binary and that daemon
/// disagree on — indistinguishable from "the checkout is free". The grant would
/// be emitted, nothing recorded, and the phantom this whole path exists to
/// remove would come back silently. So the parsed body is kept and
/// [`eligibility_diverged`] is asked before returning ALLOW, which is the same
/// question and the same treatment of a missing `claimed` the sibling path has
/// used since #5324.
/// Test: `pm_guard_denies_a_granted_dispatch_beside_a_live_writer`,
/// `pm_guard_grants_a_worktree_to_a_writer_in_a_main_checkout`,
/// `pm_guard_warns_when_a_granted_worktree_is_not_recorded`.
pub(crate) async fn evaluate_granted_worktree(
    url: &str,
    session_id: &str,
    cwd: &Path,
    payload: &Value,
    tool_input: Option<&Value>,
    updated_input: &Value,
) -> Option<String> {
    let mut granted = payload.clone();
    granted["tool_input"] = updated_input.clone();
    // #5923: this path keeps its fail-open contract. The grant it is deciding
    // gives the dispatch a tree of ITS OWN, so an unanswered claim admits an
    // isolated writer rather than a second one on the shared HEAD — the
    // opposite of the claim path's exposure.
    let body = post_shared_tree(url, session_id, cwd, &granted, GRANTED_WORKTREE_ROUTE)
        .await
        .answered();
    let live = body.as_ref().map(writers_in).unwrap_or_default();
    if live.is_empty() {
        warn_on_unrecorded_grant(body.as_ref(), &live, cwd);
        return None;
    }
    Some(granted_deny_reason(
        dispatch_agent(tool_input).unwrap_or("this"),
        cwd,
        &live,
    ))
}

/// Warn when a granted worktree was not recorded against the checkout (#5769).
///
/// Why: the grant is emitted on an empty answer, and this path fails open at
/// every step — so "nobody is here" and "I could not reach the daemon" and "the
/// daemon refused to record" all arrive as the same empty vec. Only one of the
/// three leaves the delegation record uncorrected, and that is the state the
/// HEAD-move rule then denies a `git merge` or `git rebase` on for six hours.
/// The one signal
/// separating them is `claimed`, and discarding it discards the signal.
/// What: one stderr line, which Claude Code surfaces without it reaching the
/// hook's stdout verdict. It does NOT deny: a daemon that predates this route
/// answers 404, and denying on that would fail CLOSED on version skew — every
/// dispatch from a main checkout blocked by an old daemon.
/// Test: `pm_guard_warns_when_a_granted_worktree_is_not_recorded`.
fn warn_on_unrecorded_grant(body: Option<&Value>, live: &[String], cwd: &Path) {
    // No body at all is the unreachable-daemon arm, already covered by this
    // module's fail-open contract; the interesting case is a daemon that
    // answered and still recorded nothing.
    let Some(body) = body else {
        return;
    };
    if !eligibility_diverged(body, live) {
        return;
    }
    eprintln!(
        "tm hook --pm-guard: granted a worktree in {} but the daemon recorded nothing (#5769). \
         The dispatch's delegation record therefore still reads as unisolated, so this agent will \
         be named as writing in this checkout and `git merge`/`git rebase` here will be denied \
         until the record \
         goes stale. The usual cause is a running daemon older than the `tm` on PATH, built \
         before the granted-worktree route existed — restart the daemon (`tm restart`) to clear \
         it. Granting anyway: this path fails open by design.",
        cwd.display()
    );
}

/// Build the deny message for a granted dispatch the checkout is not free for.
///
/// Why: [`deny_reason`]'s remedy is "re-dispatch with `isolation: \"worktree\"`",
/// which reads as self-contradictory here — the guard had already built exactly
/// that rewrite and then declined to emit it. The reason this path denies is
/// different from #4480's: the isolation is available, but the guard cannot rely
/// on the harness applying its `updatedInput` rewrite, and while another writer
/// holds the checkout an unapplied rewrite is the reported harm rather than a
/// hypothetical one.
///
/// The reorder this text belongs to also widens what a stale record blocks. A
/// record nothing ever closed used to block only an unisolated dispatch;
/// it now blocks every dispatch of a writer — and `Unknown` is a writer — from
/// this checkout, for the six hours of `RUNNING_STALE_AFTER_SECS`. The two
/// operator escape hatches still lift it, so that is friction rather than a
/// lockout, and the message names the possibility so a reader can recognise it.
/// What: names ADR-0048, the sibling the daemon reports, the directory, and the
/// three ways forward — dispatch with explicit isolation, serialize, or report a
/// record believed stale.
/// Test: `granted_deny_reason_does_not_offer_the_isolation_it_already_built`.
fn granted_deny_reason(agent: &str, cwd: &Path, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    format!(
        "Dispatch denied in a shared main checkout (ADR-0048): {} is a project's main checkout, \
         and the daemon's delegation records name {} as running there with no worktree of its \
         own — possibly dispatched by a different session standing in the same directory. This \
         {agent} dispatch was granted a worktree of its own, but that grant is a rewrite of the \
         dispatch's arguments and this guard cannot confirm the harness applied it; if it did \
         not, a second file-mutating agent joins the same git HEAD, which is the reported \
         failure — a commit landing on another workstream's branch, with no error at any step. \
         Re-issue this dispatch with `isolation: \"worktree\"` declared explicitly, which needs \
         no rewrite to be applied. If isolation is unavailable here, serialize instead: dispatch \
         one file-mutating agent at a time. If you believe that record is stale — the agent \
         finished without its stop signal reaching the daemon — say so rather than retrying, \
         since nothing here can tell a finished agent from a running one.",
        cwd.display(),
        names.join(", ")
    )
}

/// The route that answers and claims for an unisolated dispatch (#4480).
const SHARED_TREE_ROUTE: &str = "shared-tree-dispatch";

/// The route that answers and records the isolation the guard granted (#5769).
const GRANTED_WORKTREE_ROUTE: &str = "granted-worktree";

/// What one shared-tree POST came back with (#5923).
///
/// Why: this used to be `Option<Value>`, which gave five different failures one
/// value, and [`claim_shared_tree`]'s caller reads a missing answer as "nobody
/// else is here". A refused connection and a request that timed out then
/// produced the same verdict, though only one of them says anything about who
/// is writing in this directory: nothing is listening, versus a daemon that IS
/// listening and may already hold another writer's record. The second arm
/// allowed the dispatch, and the 2 s budget is reachable on an idle machine, so
/// the guard switched itself off under exactly the load that makes two
/// concurrent dispatches likely.
/// What: three arms, split by what the failure says about the DAEMON rather
/// than by where in the call it happened. [`Self::Unavailable`] means no daemon
/// — or no such route — can answer this call at all, which the guard treats as
/// a degraded mode and says so on stderr. [`Self::Unanswered`] means a daemon is
/// there and did not give a usable answer, which leaves the question open and
/// denies at the claim path.
/// Test: `claim_is_unknown_when_the_daemon_times_out`,
/// `claim_is_unknown_when_the_daemon_answers_500`,
/// `claim_is_unknown_when_the_body_does_not_parse`,
/// `claim_shared_tree_is_empty_when_the_daemon_is_unreachable`.
enum SharedTreeReply {
    /// The daemon answered and its body parsed.
    Answered(Value),
    /// No daemon, or no such route, can answer this call — the guard cannot
    /// function here at all, and no retry inside this hook call would change
    /// that.
    Unavailable(String),
    /// A daemon is there and did not give a usable answer.
    Unanswered(String),
}

impl SharedTreeReply {
    /// The parsed body, discarding which failure arm produced its absence.
    ///
    /// Why: the read-only callers ([`live_shared_tree_writers`]) and the grant
    /// path ([`evaluate_granted_worktree`]) keep the pre-#5923 fail-open
    /// contract, and reading them all through one accessor keeps that a stated
    /// choice at each call site rather than a shape the type quietly allows.
    /// What: `Some` only for [`Self::Answered`].
    /// Test: `pm_guard_allows_a_merge_when_the_daemon_is_unreachable` in
    /// `tests/tm_hook_pm_guard.rs`.
    fn answered(self) -> Option<Value> {
        match self {
            Self::Answered(body) => Some(body),
            Self::Unavailable(_) | Self::Unanswered(_) => None,
        }
    }
}

/// POST one call to the shared-tree route and classify what came back.
///
/// Why: split from [`claim_shared_tree`] so the claiming and the read-only
/// callers share one wire contract — the endpoint, the payload projection, and
/// the timeout bounds are the parts a second copy would drift on.
/// What: an answered body, or which KIND of failure stopped it (see
/// [`SharedTreeReply`]). Sent under the same tight connect/total bounds
/// `pm_guard`'s audit POSTs use (500 ms / 2 s), because this call sits inside a
/// `PreToolUse` budget. A 404 is read as route-absence rather than as a daemon
/// error: the route arrived in #5324, and a running daemon older than the `tm`
/// on PATH is the ordinary way to meet one — denying on that would block every
/// unisolated dispatch on version skew.
/// Test: `claim_shared_tree_is_empty_when_the_daemon_is_unreachable`,
/// `claim_shared_tree_is_empty_without_a_session_id`,
/// `claim_is_unknown_when_the_daemon_answers_500`,
/// `claim_is_unknown_when_the_body_does_not_parse`,
/// `claim_is_empty_when_the_daemon_has_no_such_route`.
async fn post_shared_tree(
    url: &str,
    session_id: &str,
    cwd: &Path,
    payload: &Value,
    route: &str,
) -> SharedTreeReply {
    if session_id.is_empty() {
        return SharedTreeReply::Unavailable(
            "the hook payload carries no session id, so no session's delegations can be addressed"
                .to_string(),
        );
    }
    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        // #5923: a client that cannot be built never reached the network, and
        // no retry in this process would change that — the guard is off, not
        // uncertain.
        Err(error) => {
            return SharedTreeReply::Unavailable(format!(
                "the guard's HTTP client could not be built: {error}"
            ));
        }
    };
    let endpoint = format!("{url}/api/v1/sessions/{session_id}/delegations/{route}");
    let mut forwarded = build_hook_payload(&cwd.display().to_string(), Some(payload), None);
    project_dispatch_input(&mut forwarded);
    let body = serde_json::json!({ "payload": forwarded });
    let response = match client.post(&endpoint).json(&body).send().await {
        Ok(response) => response,
        Err(error) => return classify_transport_failure(&endpoint, &error),
    };
    let status = response.status();
    let response = match response.error_for_status() {
        Ok(response) => response,
        // #5923: 404 is version skew — a daemon built before this route
        // existed. Every other non-2xx is a daemon that HAS the route and
        // failed to serve it, which says nothing about who is writing here.
        Err(_) if status == reqwest::StatusCode::NOT_FOUND => {
            return SharedTreeReply::Unavailable(format!(
                "the daemon has no {route} route (404), so it is older than this `tm`"
            ));
        }
        Err(error) => {
            return SharedTreeReply::Unanswered(format!("the daemon answered {status}: {error}"));
        }
    };
    match response.json::<Value>().await {
        Ok(body) => SharedTreeReply::Answered(body),
        Err(error) => {
            SharedTreeReply::Unanswered(format!("the daemon's answer did not parse: {error}"))
        }
    }
}

/// Which arm a transport failure belongs to (#5923).
///
/// Why: this is the split the reported bug turns on. A refused connection
/// answers the question on its own — nothing is listening, so no delegation can
/// be registered through this daemon at all, and the guard is structurally off.
/// A timeout does not: the daemon accepted the connection, so a writer may
/// already be recorded and the answer simply did not arrive in time.
/// What: `Unavailable` only for a connect-phase failure that is NOT a timeout.
/// reqwest reports a refused connection and a connect TIMEOUT through the same
/// `is_connect`, so the timeout is excluded explicitly: on loopback a connect
/// that times out means a listener whose accept queue is saturated, which is a
/// daemon under load rather than an absent one. Everything else — the total
/// request timeout, a body error, a redirect failure — leaves the question
/// open.
/// Test: `claim_shared_tree_is_empty_when_the_daemon_is_unreachable` (refused),
/// `claim_is_unknown_when_the_daemon_times_out` (accepted then silent).
fn classify_transport_failure(endpoint: &str, error: &reqwest::Error) -> SharedTreeReply {
    if error.is_connect() && !error.is_timeout() {
        return SharedTreeReply::Unavailable(format!("nothing is listening at {endpoint}"));
    }
    SharedTreeReply::Unanswered(format!(
        "the request to {endpoint} did not complete: {error}"
    ))
}

/// The live writers named in a shared-tree answer.
fn writers_in(body: &Value) -> Vec<String> {
    body.get("agents")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("agent").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Warn when the daemon answered "nobody here" but claimed nothing (#5324).
///
/// Why: this combination is always a disagreement, never a normal outcome. The
/// guard does not call this route at all unless it has already classified the
/// dispatch as tree-sharing, so an empty answer means the daemon declined to
/// claim a directory the guard believes needs claiming — and it re-derives
/// eligibility from its own `core::bundle::ALL`. A running daemon built before
/// an agent was added to that table sees an agent it does not know, classifies
/// it ineligible, and claims nothing. Both dispatches are then admitted with the
/// guard silently disabled, and `claimed` is the only evidence anything is
/// wrong. Discarding it discards the one signal.
/// What: one stderr line — `pm_guard` writes to stderr, which Claude Code
/// surfaces without it reaching the hook's stdout JSON verdict. It does NOT
/// deny: denying here would fail CLOSED on version skew, the exact opposite of
/// the degradation this whole path is built for, and a stale daemon would then
/// block every dispatch instead of merely failing to guard it.
/// Test: `eligibility_divergence_is_an_empty_answer_that_claimed_nothing` and
/// siblings.
fn warn_on_eligibility_divergence(body: &Value, live: &[String]) {
    if !eligibility_diverged(body, live) {
        return;
    }
    eprintln!(
        "tm hook --pm-guard: the daemon reported no live writers but claimed nothing (#5324). \
         The guard classified this dispatch as sharing the working tree and the daemon did not, \
         so concurrent shared-worktree dispatch is NOT being enforced for this agent. The usual \
         cause is a running daemon older than the `tm` on PATH, built before this agent was \
         added to its bundled table — restart the daemon (`tm restart`) to clear it. Allowing \
         the dispatch: this path fails open by design."
    );
}

/// Did the daemon answer "nobody here" while claiming nothing?
///
/// Why: split from the `eprintln!` so the condition is assertable without
/// capturing stderr — the decision is the part worth pinning, not the plumbing.
/// What: true only when the answer is EMPTY and `claimed` is not `true`. A
/// missing `claimed` counts as not-claimed, which is the conservative read: an
/// older daemon that never sends the field is exactly the skew this warns about.
/// A non-empty answer is never divergence — the daemon deliberately claims
/// nothing when it is about to deny.
/// Test: `eligibility_divergence_is_an_empty_answer_that_claimed_nothing`.
fn eligibility_diverged(body: &Value, live: &[String]) -> bool {
    let claimed = body
        .get("claimed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    live.is_empty() && !claimed
}

/// Resolve the full verdict for one `PreToolUse` call: `Some(reason)` denies.
///
/// Why: the one entry point `pm_guard` calls, so the ordering that keeps the
/// daemon off the hot path — classify first, ask second — lives here rather
/// than being re-derived at the call site.
/// What: returns `None` immediately unless [`dispatch_shares_the_tree`] and a
/// resolvable [`dispatch_cwd`]; only then claims the tree through the daemon and
/// runs [`evaluate_shared_tree_dispatch`] on the answer, or denies outright when
/// a running daemon left the claim unanswered (#5923).
/// Test: the pure half is covered exhaustively below; the network half's
/// fail-open is `claim_shared_tree_is_empty_when_the_daemon_is_unreachable` and
/// its fail-closed arms are `claim_is_unknown_when_the_daemon_times_out` and
/// siblings, and the end-to-end path runs through the real binary in
/// `tests/tm_hook_pm_guard.rs`.
pub(crate) async fn evaluate(
    url: &str,
    payload: &Value,
    tool_name: &str,
    tool_input: Option<&Value>,
    session_id: &str,
) -> Option<String> {
    evaluate_with_cwd(
        url,
        dispatch_cwd(payload),
        payload,
        tool_name,
        tool_input,
        session_id,
    )
    .await
}

/// [`evaluate`] with the working directory already resolved.
///
/// Why: lets a test drive the `cwd = None` arm — the fail-open branch for a
/// working directory that cannot be resolved — without deleting the process's
/// own directory, which is not portable. See [`resolve_dispatch_cwd`].
/// What: identical to [`evaluate`] except `cwd` is supplied; `None` returns
/// `None` (ALLOW) before any daemon call.
/// Test: `evaluate_allows_when_the_cwd_cannot_be_resolved`.
pub(crate) async fn evaluate_with_cwd(
    url: &str,
    cwd: Option<PathBuf>,
    payload: &Value,
    tool_name: &str,
    tool_input: Option<&Value>,
    session_id: &str,
) -> Option<String> {
    if !dispatch_shares_the_tree(tool_name, tool_input) {
        return None;
    }
    let cwd = cwd?;
    match claim_shared_tree(url, session_id, &cwd, payload).await {
        SharedTreeClaim::Writers(live) => {
            evaluate_shared_tree_dispatch(tool_name, tool_input, &cwd, &live)
        }
        // #5923: a running daemon that did not answer leaves the question open,
        // and admitting on an open question is the fail-open this closes.
        SharedTreeClaim::Unknown(detail) => Some(unanswered_deny_reason(
            dispatch_agent(tool_input).unwrap_or("this"),
            &cwd,
            &detail,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::agent::SUBAGENT_DISPATCH_TOOLS;

    fn input(agent: &str, isolation: Option<&str>) -> Value {
        match isolation {
            Some(i) => serde_json::json!({"subagent_type": agent, "isolation": i}),
            None => serde_json::json!({"subagent_type": agent}),
        }
    }

    #[test]
    fn denies_a_second_concurrent_unisolated_engineer() {
        // The documented incident: one engineer already running in the shared
        // tree, a second dispatched into it with no isolation.
        for tool in SUBAGENT_DISPATCH_TOOLS {
            let reason = evaluate_shared_tree_dispatch(
                tool,
                Some(&input("rust-engineer", None)),
                Path::new("/repo"),
                &["python-engineer".to_string()],
            )
            .expect("a second unisolated engineer must be denied");
            // The message must name the sibling, the directory, and the remedy.
            assert!(reason.contains("python-engineer"), "{reason}");
            assert!(reason.contains("/repo"), "{reason}");
            assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
        }
    }

    #[test]
    fn deny_reason_offers_only_remedies_that_always_work() {
        // `RUNNING_STALE_AFTER_SECS` is six hours, so a crashed subagent that
        // never emits `SubagentStop` holds its directory for that whole window.
        // Telling the PM to wait for it would be advice to wait for something
        // that may never arrive; declaring isolation works immediately.
        //
        // #5649: serialize joins isolation as a second offered remedy, because
        // the incident showed isolation can itself be unavailable. Serializing
        // constrains only FUTURE dispatches and so needs nothing from the agent
        // already running — waiting stays banned for the reason above.
        let reason = deny_reason(
            "rust-engineer",
            Path::new("/repo"),
            &["python-engineer".to_string()],
        );
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
        assert!(
            reason.contains("serialize"),
            "the deny must offer the serialize fallback for when isolation is unavailable: \
             {reason}"
        );
        for banned in ["wait for", "wait on", "wait until", "waiting for"] {
            assert!(
                !reason.contains(banned),
                "the deny must not advise waiting on an agent that may never report \
                 (found {banned:?}): {reason}"
            );
        }
    }

    #[test]
    fn granted_deny_reason_does_not_offer_the_isolation_it_already_built() {
        // #5769: this path denies a dispatch the guard had ALREADY rewritten to
        // carry `isolation: "worktree"`. Reusing #4480's text told the reader to
        // do the thing the guard had just done and declined to emit, which reads
        // as arbitrary and gets retried identically.
        let reason = granted_deny_reason(
            "rust-engineer",
            Path::new("/repo/main"),
            &["python-engineer".to_string(), "python-engineer".to_string()],
        );
        assert!(reason.contains("ADR-0048"), "{reason}");
        assert!(reason.contains("/repo/main"), "{reason}");
        // The sibling is named once, and attributed rather than asserted.
        assert_eq!(reason.matches("python-engineer").count(), 1, "{reason}");
        assert!(
            reason.contains("the daemon's delegation records name"),
            "{reason}"
        );
        // It must say WHY a grant is not enough here — the rewrite may not be
        // applied — rather than offering the grant back as the remedy.
        assert!(
            reason.contains("cannot confirm the harness applied it"),
            "{reason}"
        );
        assert!(reason.contains("declared explicitly"), "{reason}");
        assert!(reason.contains("serialize"), "{reason}");
        // A stale record is the friction case the reorder widened; naming it is
        // what lets a reader recognise it instead of retrying.
        assert!(reason.contains("stale"), "{reason}");
        for banned in ["wait for", "wait on", "wait until", "waiting for"] {
            assert!(!reason.contains(banned), "found {banned:?}: {reason}");
        }
    }

    #[test]
    fn allows_the_first_dispatch() {
        // Nobody else is in the tree: this is the overwhelmingly common case
        // and it must never be denied.
        assert_eq!(
            evaluate_shared_tree_dispatch(
                "Agent",
                Some(&input("rust-engineer", None)),
                Path::new("/repo"),
                &[],
            ),
            None
        );
    }

    #[test]
    fn allows_an_isolated_dispatch() {
        // Declaring isolation is the remedy the deny asks for; it must work.
        for mode in ["worktree", "remote"] {
            assert_eq!(
                evaluate_shared_tree_dispatch(
                    "Agent",
                    Some(&input("rust-engineer", Some(mode))),
                    Path::new("/repo"),
                    &["python-engineer".to_string()],
                ),
                None,
                "isolation={mode} must be allowed"
            );
        }
    }

    #[test]
    fn allows_a_read_only_agent() {
        // Research/review dispatches share a cwd harmlessly and the PM fans
        // them out constantly. #5650 moved `qa` out of this list — it writes
        // test files — while `code-critic` stays, because it only reads.
        for agent in ["research", "code-critic", "code-analyzer"] {
            assert_eq!(
                evaluate_shared_tree_dispatch(
                    "Agent",
                    Some(&input(agent, None)),
                    Path::new("/repo"),
                    &["rust-engineer".to_string()],
                ),
                None,
                "{agent} must be allowed alongside a running engineer"
            );
        }
    }

    #[test]
    fn denies_a_file_writing_non_engineer_alongside_an_engineer() {
        // #5650: these wrote into the engineer's tree with no deny at all.
        for agent in ["documentation", "version-control", "qa", "web-qa", "api-qa"] {
            let reason = evaluate_shared_tree_dispatch(
                "Agent",
                Some(&input(agent, None)),
                Path::new("/repo"),
                &["rust-engineer".to_string()],
            )
            .unwrap_or_else(|| panic!("{agent} writes files and must be denied"));
            assert!(reason.contains("rust-engineer"), "{reason}");
        }
    }

    #[test]
    fn allows_when_the_agent_is_unknown() {
        // A custom or project-local agent is INDETERMINATE, and indeterminate
        // resolves to ALLOW — the fail-open direction this module commits to.
        for agent in ["some-project-agent", ""] {
            assert_eq!(
                evaluate_shared_tree_dispatch(
                    "Agent",
                    Some(&input(agent, None)),
                    Path::new("/repo"),
                    &["rust-engineer".to_string()],
                ),
                None
            );
        }
        // An Agent call carrying no input at all is equally indeterminate.
        assert_eq!(
            evaluate_shared_tree_dispatch(
                "Agent",
                None,
                Path::new("/repo"),
                &["rust-engineer".to_string()]
            ),
            None
        );
    }

    #[test]
    fn allows_every_non_dispatch_tool() {
        // The PM keeps its whole working surface; only this one dispatch shape
        // is gated.
        for tool in [
            "Read",
            "Edit",
            "Write",
            "Bash",
            "SendMessage",
            "agent",
            "task",
        ] {
            assert_eq!(
                evaluate_shared_tree_dispatch(
                    tool,
                    Some(&input("rust-engineer", None)),
                    Path::new("/repo"),
                    &["python-engineer".to_string()],
                ),
                None,
                "{tool} must never be denied by this guard"
            );
        }
    }

    #[test]
    fn deny_reason_dedupes_concurrent_siblings() {
        // Two concurrent `rust-engineer`s are the realistic shape; the message
        // must read as one name, not a repeated list.
        let reason = deny_reason(
            "rust-engineer",
            Path::new("/repo"),
            &["rust-engineer".to_string(), "rust-engineer".to_string()],
        );
        assert_eq!(
            reason.matches("rust-engineer is already").count(),
            1,
            "{reason}"
        );
    }

    #[test]
    fn resolve_dispatch_cwd_falls_back_to_the_payload() {
        // The process directory is the primary source; the payload covers the
        // case where `current_dir()` failed.
        let payload = serde_json::json!({"cwd": "/from/payload"});
        assert_eq!(
            resolve_dispatch_cwd(None, &payload),
            Some(PathBuf::from("/from/payload"))
        );
        assert_eq!(
            resolve_dispatch_cwd(Some(PathBuf::from("/from/process")), &payload),
            Some(PathBuf::from("/from/process"))
        );
    }

    #[test]
    fn resolve_dispatch_cwd_is_none_when_nothing_resolves() {
        // Neither source available — the indeterminate case the guard must
        // fail open on rather than comparing against a guessed directory.
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"cwd": ""}),
            serde_json::json!({"cwd": 7}),
        ] {
            assert_eq!(resolve_dispatch_cwd(None, &payload), None, "{payload}");
        }
    }

    /// A one-shot mock answering with one live unisolated engineer.
    ///
    /// Why: pointing the cwd test at an UNREACHABLE daemon would let it pass
    /// for the wrong reason — the unreachable-daemon branch also allows, so the
    /// test would stay green even if the cwd branch were deleted. A daemon that
    /// WOULD produce a deny makes the assertion causal: only the cwd
    /// short-circuit can keep the verdict `None`.
    /// What: binds an ephemeral port, serves one request, returns the base URL.
    fn spawn_denying_mock() -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf);
                let body = r#"{"agents":[{"agent":"python-engineer","count":1}],"total":1}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes());
            }
        });
        url
    }

    #[tokio::test]
    async fn evaluate_allows_when_the_cwd_cannot_be_resolved() {
        // Declared fail-open branch: with no resolvable working directory there
        // is nothing to compare against, so the dispatch proceeds. The daemon
        // here WOULD deny — it reports a live unisolated engineer — so a `None`
        // verdict can only come from the cwd short-circuit firing first.
        let url = spawn_denying_mock();
        let verdict = evaluate_with_cwd(
            &url,
            None,
            &serde_json::json!({}),
            "Agent",
            Some(&input("rust-engineer", None)),
            "11111111-1111-1111-1111-111111111111",
        )
        .await;
        assert_eq!(
            verdict, None,
            "an unresolvable cwd must ALLOW even when the daemon would deny"
        );
    }

    #[test]
    fn dispatch_cwd_prefers_the_process_directory() {
        // The daemon records a delegation's cwd from `tm hook`'s own process
        // directory; reading a different source here would make every
        // comparison miss and the guard a silent no-op.
        let payload = serde_json::json!({"cwd": "/some/other/place"});
        assert_eq!(dispatch_cwd(&payload), std::env::current_dir().ok());
    }

    /// The writers a claim reported, panicking if the claim was unanswered.
    ///
    /// Why: the ALLOW-arm tests assert on the writer list, and an `Unknown`
    /// there is a different failure than an empty list — one denies, the other
    /// allows — so it must be loud rather than compared as equal.
    fn writers_of(claim: SharedTreeClaim) -> Vec<String> {
        match claim {
            SharedTreeClaim::Writers(live) => live,
            SharedTreeClaim::Unknown(detail) => {
                panic!("expected an answered claim, got Unknown({detail})")
            }
        }
    }

    /// A one-shot mock that ACCEPTS the connection and never answers (#5923).
    ///
    /// Why: this is the arm the reported bug rides in on, and it is the one a
    /// refused port cannot stand in for — the guard's whole new distinction is
    /// between a socket nobody is listening on and a daemon that took the
    /// connection and went quiet. Only a real accepted-then-silent listener
    /// exercises it.
    /// What: binds an ephemeral port, accepts one connection, reads the request
    /// and holds the socket open past the client's 2 s total budget. The
    /// detached thread ends with the test binary.
    fn spawn_silent_mock() -> String {
        use std::io::Read;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf);
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
        url
    }

    /// A one-shot mock answering an arbitrary status line and body.
    ///
    /// Why: [`spawn_denying_mock`] always answers 200 with a well-formed body,
    /// so it cannot drive the non-2xx or unparseable-body arms.
    fn spawn_mock_answering(status_line: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes());
            }
        });
        url
    }

    /// Claim against `url` with a fixed session, directory and payload.
    async fn claim_against(url: &str) -> SharedTreeClaim {
        claim_shared_tree(
            url,
            "11111111-1111-1111-1111-111111111111",
            Path::new("/repo"),
            &serde_json::json!({"tool_use_id": "toolu_X"}),
        )
        .await
    }

    #[tokio::test]
    async fn claim_shared_tree_is_empty_when_the_daemon_is_unreachable() {
        // Declared fail-open branch, kept by #5923: nothing is LISTENING, so no
        // delegation can be registered through this daemon at all and denying
        // would block every dispatch on a machine with no daemon running. The
        // reply must be an answered-shaped empty list, never `Unknown`.
        assert!(writers_of(claim_against("http://127.0.0.1:1").await).is_empty());
    }

    #[tokio::test]
    async fn claim_shared_tree_is_empty_without_a_session_id() {
        // A hook payload with no `session_id` cannot address a session's
        // delegations at all. Fail open before dialling anything — an unroutable
        // URL would cost a real (bounded) wait if this branch were removed.
        let started = std::time::Instant::now();
        let claim = claim_shared_tree(
            "http://127.0.0.1:1",
            "",
            Path::new("/repo"),
            &serde_json::json!({}),
        )
        .await;
        assert!(writers_of(claim).is_empty());
        assert!(started.elapsed() < std::time::Duration::from_millis(400));
    }

    #[tokio::test]
    async fn claim_is_empty_when_the_daemon_has_no_such_route() {
        // #5923 splits non-2xx by what it says about the daemon, and 404 is the
        // version-skew shape: the route arrived in #5324, so a running daemon
        // older than this `tm` answers 404 for every dispatch. Denying there
        // would fail CLOSED on version skew and block them all.
        let url = spawn_mock_answering("404 Not Found", r#"{"error":"no such route"}"#);
        assert!(writers_of(claim_against(&url).await).is_empty());
    }

    #[tokio::test]
    async fn claim_is_unknown_when_the_daemon_times_out() {
        // The reported bug (#5923). A daemon that accepted the connection and
        // did not answer inside the 2 s budget may well have another writer
        // recorded, so the claim must report that it does not know. Before the
        // fix this returned an empty list, which the policy reads as ALLOW.
        let url = spawn_silent_mock();
        let started = std::time::Instant::now();
        let claim = claim_against(&url).await;
        assert!(
            matches!(claim, SharedTreeClaim::Unknown(_)),
            "a daemon that never answered must not read as an empty tree"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the claim must stay inside its own bounded budget"
        );
    }

    #[tokio::test]
    async fn claim_is_unknown_when_the_daemon_answers_500() {
        // A daemon that HAS the route and failed to serve it says nothing about
        // who is writing here — unlike the 404 above, which says the route is
        // not there at all.
        let url = spawn_mock_answering("500 Internal Server Error", r#"{"error":"boom"}"#);
        assert!(matches!(
            claim_against(&url).await,
            SharedTreeClaim::Unknown(_)
        ));
    }

    #[tokio::test]
    async fn claim_is_unknown_when_the_body_does_not_parse() {
        // 200 with a body that is not JSON: the daemon answered and the answer
        // is unusable, which leaves the question open rather than settling it.
        let url = spawn_mock_answering("200 OK", "not json at all");
        assert!(matches!(
            claim_against(&url).await,
            SharedTreeClaim::Unknown(_)
        ));
    }

    #[tokio::test]
    async fn claim_is_answered_when_the_body_parses_with_an_unexpected_shape() {
        // The boundary of the arm above: well-formed JSON the guard cannot read
        // writers out of is still an ANSWER. `warn_on_eligibility_divergence`
        // already covers that case, and treating it as unanswered would deny
        // every dispatch against a daemon whose reply shape drifted.
        let url = spawn_mock_answering("200 OK", r#"{"unexpected":"shape"}"#);
        assert!(writers_of(claim_against(&url).await).is_empty());
    }

    #[tokio::test]
    async fn evaluate_denies_when_a_running_daemon_does_not_answer() {
        // The verdict half of `claim_is_unknown_when_the_daemon_times_out`: the
        // guard's own entry point must turn an unanswered claim into a DENY,
        // and the message must name #5923 and the remedy that needs no daemon.
        let url = spawn_silent_mock();
        let reason = evaluate_with_cwd(
            &url,
            Some(PathBuf::from("/repo")),
            &serde_json::json!({"tool_use_id": "toolu_X"}),
            "Agent",
            Some(&input("rust-engineer", None)),
            "11111111-1111-1111-1111-111111111111",
        )
        .await
        .expect("an unanswered claim must deny");
        assert!(reason.contains("#5923"), "{reason}");
        assert!(reason.contains("isolation"), "{reason}");
    }

    #[tokio::test]
    async fn evaluate_allows_when_no_daemon_is_listening() {
        // The other side of the same entry point: a refused connection stays an
        // ALLOW. This is the arm a fix that denied on every failure would have
        // broken, turning a machine with no daemon into one that cannot
        // dispatch at all.
        let verdict = evaluate_with_cwd(
            "http://127.0.0.1:1",
            Some(PathBuf::from("/repo")),
            &serde_json::json!({"tool_use_id": "toolu_X"}),
            "Agent",
            Some(&input("rust-engineer", None)),
            "11111111-1111-1111-1111-111111111111",
        )
        .await;
        assert_eq!(verdict, None);
    }

    #[test]
    fn unanswered_deny_reason_names_the_failure_and_offers_isolation() {
        // The reader of this message must not go looking for a sibling agent:
        // the point is that nobody could say whether one exists. It has to name
        // what failed, and offer the remedy that works with no daemon at all.
        let reason = unanswered_deny_reason(
            "rust-engineer",
            Path::new("/repo"),
            "the daemon answered 500 Internal Server Error",
        );
        assert!(reason.contains("#5923"), "{reason}");
        assert!(reason.contains("/repo"), "{reason}");
        assert!(reason.contains("500 Internal Server Error"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
        assert!(reason.contains("serialize"), "{reason}");
    }

    #[test]
    fn claim_payload_carries_only_the_fields_the_daemon_reads() {
        // #5324: the dispatch prompt must not travel to the daemon. Forwarding
        // it verbatim puts an unbounded body under axum's 2 MB limit, and a 413
        // is an empty answer — the dispatch admitted, unclaimed, guard silently
        // off for exactly the biggest dispatches. Projecting removes that arm
        // rather than testing it.
        let mut forwarded = serde_json::json!({
            "cwd": "/repo",
            "tool": "Agent",
            "input": {
                "subagent_type": "rust-engineer",
                "isolation": "worktree",
                "description": "short label",
                "prompt": "x".repeat(4096),
                "extra": {"nested": true},
            },
        });
        project_dispatch_input(&mut forwarded);

        let input = forwarded.get("input").expect("input survives");
        let keys: Vec<&String> = input
            .as_object()
            .expect("input stays an object")
            .keys()
            .collect();
        assert_eq!(
            keys,
            vec!["description", "isolation", "subagent_type"],
            "only the three fields the route and the tracker read may be sent"
        );
        // The three that remain must be untouched — the record the daemon
        // writes has to stay byte-identical to the tracker's own.
        assert_eq!(input["subagent_type"], "rust-engineer");
        assert_eq!(input["isolation"], "worktree");
        assert_eq!(input["description"], "short label");
        // Sibling keys outside `input` are not this function's business.
        assert_eq!(forwarded["tool"], "Agent");
        assert_eq!(forwarded["cwd"], "/repo");
    }

    #[test]
    fn claim_payload_projection_leaves_a_non_object_input_alone() {
        // Nothing to narrow, and inventing a shape here could only make the
        // daemon's classification disagree with the guard's. Absent stays
        // absent; a non-object stays whatever it was.
        let mut absent = serde_json::json!({"cwd": "/repo", "tool": "Agent"});
        project_dispatch_input(&mut absent);
        assert!(absent.get("input").is_none(), "absent input stays absent");

        let mut scalar = serde_json::json!({"cwd": "/repo", "input": "not-an-object"});
        project_dispatch_input(&mut scalar);
        assert_eq!(scalar["input"], "not-an-object");
    }

    #[test]
    fn eligibility_divergence_is_an_empty_answer_that_claimed_nothing() {
        // #5324: the guard never asks unless it already classified the dispatch
        // as tree-sharing, so "nobody here" AND "claimed nothing" is always the
        // daemon disagreeing about eligibility — the one observable symptom of
        // a daemon older than the `tm` on PATH. It must warn, and must NOT deny:
        // denying would fail closed on version skew.
        let empty: Vec<String> = Vec::new();
        assert!(
            eligibility_diverged(&serde_json::json!({"agents": [], "claimed": false}), &empty),
            "empty answer that claimed nothing is divergence"
        );
        assert!(
            eligibility_diverged(&serde_json::json!({"agents": []}), &empty),
            "a daemon too old to send `claimed` reads as not-claimed"
        );
        assert!(
            !eligibility_diverged(&serde_json::json!({"agents": [], "claimed": true}), &empty),
            "the ordinary first dispatch claims, and is silent"
        );
        assert!(
            !eligibility_diverged(
                &serde_json::json!({"claimed": false}),
                &["rust-engineer".to_string()]
            ),
            "a non-empty answer is a deny, where claiming nothing is correct"
        );
    }

    #[tokio::test]
    async fn evaluate_never_calls_the_daemon_for_a_non_dispatch_tool() {
        // An unroutable URL would cost a real (bounded) wait if it were dialled;
        // returning instantly proves the classify-first ordering holds.
        let payload = serde_json::json!({"cwd": "/repo"});
        let started = std::time::Instant::now();
        let verdict = evaluate(
            "http://127.0.0.1:1",
            &payload,
            "Read",
            Some(&input("rust-engineer", None)),
            "11111111-1111-1111-1111-111111111111",
        )
        .await;
        assert_eq!(verdict, None);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(400),
            "the daemon must not be dialled for a non-dispatch tool"
        );
    }
}
