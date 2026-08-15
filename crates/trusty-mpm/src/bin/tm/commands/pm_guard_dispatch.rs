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
//! **This guard FAILS OPEN at every step.** A down, slow, or unreachable daemon
//! answers "nobody else is here" and the dispatch proceeds; so does an
//! unresolvable cwd, an unrecognised agent name, an untyped dispatch, and any
//! malformed response. The asymmetry is deliberate and matches
//! [`super::pm_guard_fanout`]: a false DENY lands on the PM and halts every
//! dispatch in the system, while a false ALLOW merely reproduces the behaviour
//! that shipped before this module.
//!
//! Cost: the daemon call is made only after the dispatch itself is classified as
//! a shared-tree writer, so a research, review, or QA dispatch — and every
//! non-dispatch tool call, which is essentially all traffic — pays nothing.
//!
//! Test: `denies_a_second_concurrent_unisolated_engineer`,
//! `allows_the_first_dispatch`, `allows_an_isolated_dispatch`,
//! `allows_a_read_only_agent`, `allows_when_the_agent_is_unknown`,
//! `allows_every_non_dispatch_tool` below. The six declared fail-open branches
//! each have an error-arm test: unreachable daemon
//! (`claim_shared_tree_is_empty_when_the_daemon_is_unreachable`),
//! malformed response (`pm_guard_allows_when_the_daemon_answer_is_malformed`),
//! unresolvable cwd (`evaluate_allows_when_the_cwd_cannot_be_resolved`),
//! unknown agent (`allows_when_the_agent_is_unknown`), untyped dispatch (same),
//! and non-dispatch tool (`allows_every_non_dispatch_tool`).

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
/// so a slow daemon must cost a bounded wait and nothing more. EVERY failure —
/// client build, transport, non-2xx, malformed body — returns an empty vec,
/// which the pure policy above reads as ALLOW; nothing is claimed, and the
/// verdict degrades to exactly the behaviour that shipped before #4480.
/// Test: `claim_shared_tree_is_empty_when_the_daemon_is_unreachable`.
pub(crate) async fn claim_shared_tree(
    url: &str,
    session_id: &str,
    cwd: &Path,
    payload: &Value,
) -> Vec<String> {
    if session_id.is_empty() {
        return Vec::new();
    }
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return Vec::new();
    };
    let endpoint = format!("{url}/api/v1/sessions/{session_id}/delegations/shared-tree-dispatch");
    let mut forwarded = build_hook_payload(&cwd.display().to_string(), Some(payload), None);
    project_dispatch_input(&mut forwarded);
    let body = serde_json::json!({ "payload": forwarded });
    let Ok(response) = client.post(&endpoint).json(&body).send().await else {
        return Vec::new();
    };
    let Ok(response) = response.error_for_status() else {
        return Vec::new();
    };
    let Ok(body) = response.json::<Value>().await else {
        return Vec::new();
    };
    let live: Vec<String> = body
        .get("agents")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("agent").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    warn_on_eligibility_divergence(&body, &live);
    live
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
/// runs [`evaluate_shared_tree_dispatch`] on the answer.
/// Test: the pure half is covered exhaustively below; the network half's
/// fail-open is `claim_shared_tree_is_empty_when_the_daemon_is_unreachable`,
/// and the end-to-end path runs through the real binary in
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
    let live = claim_shared_tree(url, session_id, &cwd, payload).await;
    evaluate_shared_tree_dispatch(tool_name, tool_input, &cwd, &live)
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

    #[tokio::test]
    async fn claim_shared_tree_is_empty_when_the_daemon_is_unreachable() {
        // Fail-open on the network path: an unroutable daemon must resolve to
        // "nobody else is here", never hang and never deny. Nothing is claimed
        // either, so the verdict degrades to the pre-#4480 behaviour rather
        // than to a directory nobody can release.
        let live = claim_shared_tree(
            "http://127.0.0.1:1",
            "11111111-1111-1111-1111-111111111111",
            Path::new("/repo"),
            &serde_json::json!({"tool_use_id": "toolu_X"}),
        )
        .await;
        assert!(live.is_empty());
    }

    #[tokio::test]
    async fn claim_shared_tree_is_empty_without_a_session_id() {
        // A hook payload with no `session_id` cannot address a session's
        // delegations at all. Fail open before dialling anything — an unroutable
        // URL would cost a real (bounded) wait if this branch were removed.
        let started = std::time::Instant::now();
        let live = claim_shared_tree(
            "http://127.0.0.1:1",
            "",
            Path::new("/repo"),
            &serde_json::json!({}),
        )
        .await;
        assert!(live.is_empty());
        assert!(started.elapsed() < std::time::Duration::from_millis(400));
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
