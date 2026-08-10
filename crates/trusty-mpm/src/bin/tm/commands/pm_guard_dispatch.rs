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
//! 1. the tool is a [`SUBAGENT_DISPATCH_TOOLS`] member;
//! 2. the caller is the PM, not a subagent (a subagent's dispatch is already
//!    denied outright by [`super::pm_guard_fanout`], so this never reaches it);
//! 3. the dispatch names a bundled engineer-tier agent
//!    ([`agent_mutates_files`]) and declares no isolation
//!    ([`isolation_separates_working_tree`]);
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
//! **Known gap — this is check-then-decide, with no reservation step.** The
//! daemon is queried and the verdict computed from the answer; nothing claims
//! the directory in between. Two dispatches that both query before either's
//! `on_dispatch` recording lands both see an empty set and are both ALLOWED.
//! `pm_guard_admits_both_dispatches_that_query_before_either_is_recorded`
//! (`tests/tm_hook_pm_guard.rs`) pins that behaviour deterministically, by
//! blocking the mock daemon's replies until both queries have arrived.
//!
//! Whether the harness ever produces that interleaving is NOT established here.
//! Claude Code's hooks reference states that multiple handlers matching ONE
//! event run in parallel, and says nothing about `PreToolUse` across
//! simultaneous `tool_use` blocks; nothing in this repo can observe it. So the
//! guard is reliable for a dispatch issued after a sibling is already recorded,
//! and indeterminate for two issued in the same turn. Closing that needs a
//! reservation step — a record-and-answer that is atomic in the daemon — which
//! is a design change, deliberately not made here (#4480).
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
//! (`live_shared_tree_writers_is_empty_when_the_daemon_is_unreachable`),
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
/// What: a single-paragraph `permissionDecisionReason`.
/// Test: `denies_a_second_concurrent_unisolated_engineer`,
/// `deny_reason_offers_only_the_remedy_that_always_works`.
fn deny_reason(agent: &str, cwd: &Path, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let running = names.join(", ");
    format!(
        "Concurrent shared-worktree dispatch denied (#4480): {running} is already running in \
         {} without a worktree of its own, and this {agent} dispatch would put a second \
         file-mutating agent on the same git HEAD. Git does not catch this — a `git checkout -b` \
         refuses only when a tracked file differs between both branches AND has an uncommitted \
         change, so untracked files and edits the two branches agree on transfer onto the wrong \
         branch silently, with no error at any step. Re-dispatch this agent with \
         `isolation: \"worktree\"` so it gets its own tree.",
        cwd.display()
    )
}

/// Classify one dispatch against the set of agents already writing in this tree.
///
/// Why: kept pure — it takes the daemon's answer as a slice rather than fetching
/// it — so the whole policy is exhaustively unit-testable with no network, and
/// the I/O lives in [`live_shared_tree_writers`] next door.
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

/// Ask the daemon which agents are already writing unisolated in `cwd`.
///
/// Why: see the module doc — liveness is the daemon's to answer, and it is the
/// only process that resolves it from real terminal signals.
/// What: `GET <url>/api/v1/sessions/{session}/delegations/shared-tree-writers`
/// with the caller's own `tool_use_id` excluded, under the same tight
/// connect/total bounds `pm_guard`'s audit POSTs use (500 ms / 2 s) — this call
/// sits inside a `PreToolUse` budget, so a slow daemon must cost a bounded wait
/// and nothing more. EVERY failure — client build, transport, non-2xx, malformed
/// body — returns an empty vec, which the pure policy above reads as ALLOW.
/// Test: `live_shared_tree_writers_is_empty_when_the_daemon_is_unreachable`.
pub(crate) async fn live_shared_tree_writers(
    url: &str,
    session_id: &str,
    cwd: &Path,
    exclude_tool_use_id: Option<&str>,
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
    let endpoint = format!("{url}/api/v1/sessions/{session_id}/delegations/shared-tree-writers");
    let mut query: Vec<(&str, String)> = vec![("cwd", cwd.display().to_string())];
    if let Some(id) = exclude_tool_use_id {
        query.push(("exclude_tool_use_id", id.to_string()));
    }
    let Ok(response) = client.get(&endpoint).query(&query).send().await else {
        return Vec::new();
    };
    let Ok(response) = response.error_for_status() else {
        return Vec::new();
    };
    let Ok(body) = response.json::<Value>().await else {
        return Vec::new();
    };
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

/// Resolve the full verdict for one `PreToolUse` call: `Some(reason)` denies.
///
/// Why: the one entry point `pm_guard` calls, so the ordering that keeps the
/// daemon off the hot path — classify first, ask second — lives here rather
/// than being re-derived at the call site.
/// What: returns `None` immediately unless [`dispatch_shares_the_tree`] and a
/// resolvable [`dispatch_cwd`]; only then queries the daemon and runs
/// [`evaluate_shared_tree_dispatch`].
/// Test: the pure half is covered exhaustively below; the network half's
/// fail-open is `live_shared_tree_writers_is_empty_when_the_daemon_is_unreachable`,
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
    let tool_use_id = payload
        .get("tool_use_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let live = live_shared_tree_writers(url, session_id, &cwd, tool_use_id).await;
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
    fn deny_reason_offers_only_the_remedy_that_always_works() {
        // `RUNNING_STALE_AFTER_SECS` is six hours, so a crashed subagent that
        // never emits `SubagentStop` holds its directory for that whole window.
        // Telling the PM to wait for it would be advice to wait for something
        // that may never arrive; declaring isolation works immediately.
        let reason = deny_reason(
            "rust-engineer",
            Path::new("/repo"),
            &["python-engineer".to_string()],
        );
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
        assert!(
            !reason.contains("wait for"),
            "the deny must not advise waiting on an agent that may never report: {reason}"
        );
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
        // them out constantly.
        for agent in ["research", "code-critic", "qa"] {
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
    async fn live_shared_tree_writers_is_empty_when_the_daemon_is_unreachable() {
        // Fail-open on the network path: an unroutable daemon must resolve to
        // "nobody else is here", never hang and never deny.
        let live = live_shared_tree_writers(
            "http://127.0.0.1:1",
            "11111111-1111-1111-1111-111111111111",
            Path::new("/repo"),
            Some("toolu_X"),
        )
        .await;
        assert!(live.is_empty());
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
