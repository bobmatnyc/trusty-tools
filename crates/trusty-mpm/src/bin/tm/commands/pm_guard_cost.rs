//! `tm hook --pm-guard` — per-subagent context ceiling (issue #4837).
//!
//! Why: the policy half of this guard lives in
//! [`trusty_mpm::core::agent_cost`] and explains the cost model. This module is
//! the I/O half: it answers "how many tokens is the agent making this call
//! actually carrying?" from the `PreToolUse` payload, which is the only place
//! the guard can see it. Claude Code's `PreToolUse` payload carries **no token
//! counts** — the empirically-confirmed field set is `session_id`,
//! `transcript_path`, `cwd`, `prompt_id`, `permission_mode`, `hook_event_name`,
//! `tool_name`, `tool_input`, `tool_use_id`, plus `agent_id` inside a subagent
//! (see [`super::hook_payload`]'s capture notes). The counts live one hop away,
//! in the transcript that payload points at, which is why this module resolves
//! a path rather than reading a field.
//!
//! What: [`resolve_agent_transcript`] finds the calling *subagent's* own
//! transcript; [`evaluate_agent_cost`] tail-reads it under a hard timeout and
//! classifies the result via [`evaluate_cost`]; [`is_persistence_escape`]
//! answers whether a stopped agent's tool call is one of the few that stay
//! permitted so it can save and report its work.
//!
//! **Fails OPEN at every step**, per the policy module's asymmetry note: not a
//! subagent, no resolvable transcript, an unreadable or truncated file, a tail
//! with no usage record, or a disabled/zeroed config all yield
//! [`BudgetStatus::Ok`]. The PM is never a subagent and so is never evaluated
//! at all — a bug here cannot halt orchestration.
//!
//! Test: `resolves_*`/`fails_open_*` below cover path resolution and the
//! fail-open matrix; the threshold policy is pinned in
//! [`trusty_mpm::core::agent_cost`]'s own suite.

use std::path::{Path, PathBuf};

use trusty_mpm::core::agent_cost::{AgentCostConfig, BudgetStatus, evaluate_cost};

/// Cap on transcript bytes read on the FIRST tail pass.
///
/// Why: the newest `usage` block sits at the very end of the JSONL, so a small
/// window is usually sufficient — and bounding it is what keeps the guard's
/// cost constant on the multi-hundred-megabyte transcripts it exists to catch.
/// 64 KiB spans several assistant turns in the common case and is the cheap
/// path this guard takes on nearly every call.
/// What: byte count passed to [`super::misc::read_transcript_tail`] first; when
/// it yields no usage record the read is retried once at
/// [`RETRY_TRANSCRIPT_TAIL`].
/// Test: `retries_with_a_larger_tail_when_64k_holds_no_usage_record`; the
/// truncated-line tolerance it relies on is pinned by
/// `core::agent_cost::tolerates_a_truncated_leading_line`.
const MAX_TRANSCRIPT_TAIL: u64 = 64 * 1024;

/// Cap on transcript bytes read on the RETRY pass.
///
/// Why (#4837 review, MEDIUM): 64 KiB alone degrades on exactly the transcripts
/// this guard exists to catch. Measured on a working machine, 1 of the 12
/// largest subagent transcripts carried no complete `usage` record in its final
/// 64 KiB — a single oversized tool result at the tail is enough to push the
/// newest assistant turn out of the window — and a missing record fails OPEN,
/// so coverage silently drops off at the top of the distribution. Retrying once
/// at 16x restores it without making the common case pay: the second read
/// happens only when the first found nothing, and it is still a bounded
/// constant, so the guard's cost stays independent of transcript size.
/// What: byte count for the second [`super::misc::read_transcript_tail`] call.
/// 1 MiB spans roughly twenty maximum-size tool results; beyond that the file
/// is pathological in a way this guard is not the right place to diagnose, and
/// the fail-open answer is correct again.
/// Test: `retries_with_a_larger_tail_when_64k_holds_no_usage_record`,
/// `still_fails_open_when_even_the_larger_tail_has_no_record`.
const RETRY_TRANSCRIPT_TAIL: u64 = 1024 * 1024;

/// Hard ceiling on the whole cost evaluation.
///
/// Why: `PreToolUse` is configured with a 5-second timeout and runs before
/// EVERY tool call, so the guard must be invisible in the common case. 200 ms
/// is far above a 64 KiB tail read from page cache and far below the point at
/// which a user would notice; blowing it fails open rather than stalling.
/// What: timeout wrapped around the tail read.
/// Test: `fails_open_when_the_transcript_is_missing` covers the failure branch
/// this timeout shares.
const EVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// Locate the transcript of the subagent issuing this `PreToolUse` call.
///
/// Why: this is the whole difficulty of #4837 on the observation side. The
/// guard must measure the *subagent's* context, and reading the parent's
/// transcript instead would charge a cheap agent for its PM's history — a
/// false stop, the failure mode this design refuses. Claude Code has changed
/// which path field it emits between releases (`SubagentStop` carries the
/// subagent's transcript as `agent_transcript_path` while `transcript_path` is
/// the *parent's*), so rather than pin one field and silently no-op if it
/// moves, this tries three shapes and accepts only a path that exists on disk.
/// What, in order: (1) an explicit `agent_transcript_path`; (2) a
/// `transcript_path` that already sits under a `subagents/` directory, i.e.
/// the field already points at the subagent; (3) the documented layout
/// `<dir>/<parent-stem>/subagents/agent-<agent_id>.jsonl` derived from
/// `transcript_path` + `agent_id` — confirmed against a live transcript tree
/// on 2026-08-04. Returns `None` when no candidate resolves to an existing
/// file, which is the FAIL-OPEN answer.
/// Test: `resolves_explicit_agent_transcript_path`,
/// `resolves_a_transcript_path_already_under_subagents`,
/// `derives_the_subagent_path_from_agent_id`,
/// `fails_open_without_any_transcript_field`.
pub(crate) fn resolve_agent_transcript(payload: &serde_json::Value) -> Option<PathBuf> {
    let field = |k: &str| {
        payload
            .get(k)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
    };

    // 1. The field that names it outright.
    if let Some(p) = field("agent_transcript_path") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }

    let parent = field("transcript_path")?;
    let parent = Path::new(parent);

    // 2. Already the subagent's own transcript.
    if parent
        .parent()
        .is_some_and(|d| d.file_name().is_some_and(|n| n == "subagents"))
        && parent.is_file()
    {
        return Some(parent.to_path_buf());
    }

    // 3. Derive it from the documented layout.
    let agent_id = field("agent_id")?;
    let stem = parent.file_stem()?;
    let derived = parent
        .parent()?
        .join(stem)
        .join("subagents")
        .join(format!("agent-{agent_id}.jsonl"));
    derived.is_file().then_some(derived)
}

/// Measure and classify the calling subagent's context spend.
///
/// Why: split from [`resolve_agent_transcript`] so the (I/O-bound, timeout-
/// wrapped) read is separable from the (pure, exhaustively-tested) path logic.
/// Called only after the caller has been positively identified as a subagent,
/// so the PM path pays neither the config load nor the file read.
/// What: resolves the transcript, tail-reads it under [`EVAL_TIMEOUT`] —
/// [`MAX_TRANSCRIPT_TAIL`] first, retried once at [`RETRY_TRANSCRIPT_TAIL`]
/// when that window holds no complete usage record — extracts the newest
/// context size, and returns `(status, context_tokens)`. Any failure returns
/// `(`[`BudgetStatus::Ok`]`, 0)` — fail open.
/// Test: `fails_open_when_the_transcript_is_missing`,
/// `reports_exceeded_for_an_over_ceiling_transcript`,
/// `respects_a_disabled_config`,
/// `retries_with_a_larger_tail_when_64k_holds_no_usage_record`,
/// `still_fails_open_when_even_the_larger_tail_has_no_record`.
pub(crate) async fn evaluate_agent_cost(
    payload: &serde_json::Value,
    config: &AgentCostConfig,
) -> (BudgetStatus, u64) {
    if !config.enabled {
        return (BudgetStatus::Ok, 0);
    }
    let Some(path) = resolve_agent_transcript(payload) else {
        return (BudgetStatus::Ok, 0);
    };
    let Some(tokens) = tokio::time::timeout(EVAL_TIMEOUT, read_latest_context(&path))
        .await
        .ok()
        .flatten()
    else {
        return (BudgetStatus::Ok, 0);
    };
    (evaluate_cost(tokens, config), tokens)
}

/// Two-pass tail read yielding the newest context size, or `None`.
///
/// Why (#4837 review, MEDIUM): split out of [`evaluate_agent_cost`] so the
/// whole retry sits inside the single [`EVAL_TIMEOUT`] — the guard's latency
/// budget is per-call, not per-read, so a slow disk cannot turn the retry into
/// two timeouts. Ordering matters: the cheap 64 KiB read is tried first and
/// the 1 MiB read happens only when it found nothing, which is rare.
/// What: reads [`MAX_TRANSCRIPT_TAIL`] bytes and parses; on no record, reads
/// [`RETRY_TRANSCRIPT_TAIL`] bytes and parses again. `None` — fail open — when
/// both come up empty or the file is unreadable.
/// Test: `retries_with_a_larger_tail_when_64k_holds_no_usage_record`,
/// `still_fails_open_when_even_the_larger_tail_has_no_record`.
async fn read_latest_context(path: &Path) -> Option<u64> {
    use trusty_mpm::core::agent_cost::latest_context_tokens;

    for bytes in [MAX_TRANSCRIPT_TAIL, RETRY_TRANSCRIPT_TAIL] {
        let jsonl = super::misc::read_transcript_tail(path, bytes).await?;
        if let Some(tokens) = latest_context_tokens(&jsonl) {
            return Some(tokens);
        }
        // A short file cannot hide anything in a bigger window — the first
        // read already covered it, so skip the pointless second pass.
        if (jsonl.len() as u64) < bytes {
            return None;
        }
    }
    None
}

/// Whether this tool call is the stopped agent's escape hatch.
///
/// Why (#4837 review, BLOCK 1(b)): the `Exceeded` arm denied every tool, so an
/// agent that had produced a correct fix could not commit it, push it, or
/// report it — the deny text pointed at a channel the same deny had closed.
/// Traced against a real case: the #4841 engineer reached 434k while producing
/// a correct fix and would have been stranded. A guard that strands work is
/// worse than the overrun it prevents, so the stop keeps a narrow allowlist
/// open. An allowlist (rather than a one-shot grace budget) is the right shape
/// because `PreToolUse` is stateless: a grace *count* would need durable
/// per-agent state that has to be created, expired, and reasoned about on
/// every call, and it would let the agent spend its grace on anything at all.
/// Naming the tools instead makes the escape hatch exactly as wide as
/// "persist and report" and no wider, with nothing to keep.
/// What: `true` for [`is_persistence_tool`] (`SendMessage`), and for `Bash`
/// when [`command_is_persistence_only`] proves every segment of its command is
/// an allowlisted git call. Everything else is `false` → denied.
/// Test: `escape_hatch_permits_send_message_and_git_persistence`,
/// `escape_hatch_denies_work_tools`.
pub(crate) fn is_persistence_escape(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> bool {
    use trusty_mpm::core::agent_cost::is_persistence_tool;

    if is_persistence_tool(tool_name) {
        return true;
    }
    if tool_name != "Bash" {
        return false;
    }
    let command = tool_input
        .and_then(|v| v.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    super::pm_guard_bash::command_is_persistence_only(command)
}

/// Claim the right to show this subagent its cost warning, once.
///
/// Why (#4837 review, HIGH): the warning now reaches the agent through
/// `hookSpecificOutput.additionalContext`, which is the fix — but emitting it
/// on EVERY call while the agent sits in the warn band would re-send the same
/// ~90 tokens for the rest of that agent's life, spending context to complain
/// about context. Since the shipped default has no hard stop, the warn band is
/// unbounded and that spam would be unbounded with it. A single filesystem
/// marker per agent turns the notice into what it is meant to be: one nudge at
/// the moment the threshold is crossed. `create_new` makes the claim atomic, so
/// concurrent tool calls from the same agent cannot both win it.
/// What: `true` the first time it is called for a given `agent_id` (falling
/// back to `session_id`), `false` afterwards. Any I/O failure returns `true` —
/// failing toward informing the agent, since a missed nudge is worse than a
/// duplicated one. Markers live in the OS temp dir, which is reaped for us; the
/// only cost of losing them early is one extra nudge.
/// Test: `warn_notice_is_claimed_once_per_agent`.
pub(crate) fn claim_warn_notice(payload: &serde_json::Value) -> bool {
    let key = ["agent_id", "session_id"]
        .iter()
        .find_map(|k| {
            payload
                .get(*k)
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("unknown");
    let key: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    let dir = std::env::temp_dir().join("trusty-mpm-agent-cost");
    if std::fs::create_dir_all(&dir).is_err() {
        return true;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(key))
    {
        Ok(_) => true,
        // Already claimed — this agent has been told.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::agent_cost::stop_reason;

    /// Build a transcript tree matching Claude Code's real layout and return
    /// `(parent_transcript, subagent_transcript)`.
    fn transcript_tree(dir: &Path, agent_id: &str, context_tokens: u64) -> (PathBuf, PathBuf) {
        let parent = dir.join("5b4e60d2-d5d9-4ec5-927f-4fb9198a296d.jsonl");
        std::fs::write(&parent, "{}\n").expect("write parent");
        let sub_dir = dir
            .join("5b4e60d2-d5d9-4ec5-927f-4fb9198a296d")
            .join("subagents");
        std::fs::create_dir_all(&sub_dir).expect("mkdir");
        let sub = sub_dir.join(format!("agent-{agent_id}.jsonl"));
        let line = serde_json::json!({
            "type": "assistant",
            "message": { "usage": {
                "input_tokens": 8,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": context_tokens - 8,
                "output_tokens": 100
            }}
        });
        std::fs::write(&sub, format!("{line}\n")).expect("write sub");
        (parent, sub)
    }

    #[test]
    fn derives_the_subagent_path_from_agent_id() {
        // The load-bearing case: PreToolUse inside a subagent carries the
        // PARENT's transcript_path plus an agent_id, and the subagent's own
        // transcript must be reached from those two.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, sub) = transcript_tree(tmp.path(), "a1d57cf5a7f59b877", 100_000);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "a1d57cf5a7f59b877",
        });
        assert_eq!(resolve_agent_transcript(&payload).as_ref(), Some(&sub));
    }

    #[test]
    fn resolves_explicit_agent_transcript_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, sub) = transcript_tree(tmp.path(), "abc123", 100_000);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_transcript_path": sub.to_str().expect("utf8"),
            "agent_id": "abc123",
        });
        assert_eq!(resolve_agent_transcript(&payload).as_ref(), Some(&sub));
    }

    #[test]
    fn resolves_a_transcript_path_already_under_subagents() {
        // If a future Claude Code release points transcript_path straight at
        // the subagent, the guard must still work with no agent_id at all.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (_, sub) = transcript_tree(tmp.path(), "abc123", 100_000);
        let payload = serde_json::json!({
            "transcript_path": sub.to_str().expect("utf8"),
        });
        assert_eq!(resolve_agent_transcript(&payload).as_ref(), Some(&sub));
    }

    #[test]
    fn fails_open_without_any_transcript_field() {
        // Every indeterminate payload shape must resolve to None (→ ALLOW).
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"tool_name": "Bash"}),
            serde_json::json!({"transcript_path": ""}),
            // A parent transcript with no agent_id: this is the PM, and the PM
            // must never be measured against a subagent ceiling.
            serde_json::json!({"transcript_path": "/tmp/nope/parent.jsonl"}),
            // agent_id present but the derived file does not exist.
            serde_json::json!({
                "transcript_path": "/tmp/nope/parent.jsonl",
                "agent_id": "ghost",
            }),
        ] {
            assert_eq!(
                resolve_agent_transcript(&payload),
                None,
                "expected fail-open for {payload}"
            );
        }
    }

    #[tokio::test]
    async fn fails_open_when_the_transcript_is_missing() {
        // The core safety property: a broken counter allows the work.
        let payload = serde_json::json!({
            "transcript_path": "/tmp/definitely-not-here/p.jsonl",
            "agent_id": "ghost",
        });
        let (status, tokens) = evaluate_agent_cost(&payload, &AgentCostConfig::default()).await;
        assert_eq!(status, BudgetStatus::Ok);
        assert_eq!(tokens, 0);
    }

    /// A config with the hard stop opted in. The shipped default is warn-only
    /// (#4837 review BLOCK 1(a)), so stop-path tests must ask for a stop.
    fn opted_in_stop() -> AgentCostConfig {
        AgentCostConfig {
            enabled: true,
            max_tokens: 400_000,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn reports_exceeded_for_an_over_ceiling_transcript() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, _) = transcript_tree(tmp.path(), "big", 622_200);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "big",
        });
        let (status, tokens) = evaluate_agent_cost(&payload, &opted_in_stop()).await;
        assert_eq!(status, BudgetStatus::Exceeded);
        assert_eq!(tokens, 622_200);
        // And the reason handed back must carry the measured number.
        assert!(stop_reason(tokens, 400_000).contains("622200"));
    }

    #[tokio::test]
    async fn default_config_only_warns_on_the_same_transcript() {
        // BLOCK 1(a) end to end: the identical 622.2k transcript that the
        // opted-in ceiling stops must merely WARN under what actually ships.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, _) = transcript_tree(tmp.path(), "big", 622_200);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "big",
        });
        let (status, tokens) = evaluate_agent_cost(&payload, &AgentCostConfig::default()).await;
        assert_eq!(status, BudgetStatus::Warning);
        assert_eq!(tokens, 622_200);
    }

    #[tokio::test]
    async fn respects_a_disabled_config() {
        // Config override reaches the I/O path too, not just the classifier —
        // a disabled guard must not even touch the filesystem.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, _) = transcript_tree(tmp.path(), "big", 900_000);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "big",
        });
        let disabled = AgentCostConfig {
            enabled: false,
            ..Default::default()
        };
        assert_eq!(
            evaluate_agent_cost(&payload, &disabled).await,
            (BudgetStatus::Ok, 0)
        );
    }

    #[tokio::test]
    async fn allows_a_healthy_agent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, _) = transcript_tree(tmp.path(), "small", 71_540);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "small",
        });
        let (status, tokens) = evaluate_agent_cost(&payload, &AgentCostConfig::default()).await;
        assert_eq!(status, BudgetStatus::Ok);
        assert_eq!(tokens, 71_540);
    }

    // ── #4837 review MEDIUM: the 64 KiB window misses the largest transcripts ──

    /// One JSONL line of `bytes` of filler carrying no `usage` block — a stand-in
    /// for the oversized tool result that pushes the newest assistant turn out
    /// of a 64 KiB window.
    fn filler_line(bytes: usize) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type": "user", "pad": "x".repeat(bytes)})
        )
    }

    #[tokio::test]
    async fn retries_with_a_larger_tail_when_64k_holds_no_usage_record() {
        // Measured: 1 of the 12 largest subagent transcripts on this machine
        // had no complete usage record in its final 64 KiB, so the guard failed
        // open on exactly the transcripts it exists to catch.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, sub) = transcript_tree(tmp.path(), "huge", 500_000);
        // Bury the usage record behind more than 64 KiB of unparseable tail.
        let mut jsonl = std::fs::read_to_string(&sub).expect("read");
        jsonl.push_str(&filler_line(100 * 1024));
        std::fs::write(&sub, &jsonl).expect("write");
        assert!(
            jsonl.len() as u64 > MAX_TRANSCRIPT_TAIL,
            "the fixture must actually exceed the first window"
        );

        // The first pass alone finds nothing — this is the bug being fixed.
        let first = super::super::misc::read_transcript_tail(&sub, MAX_TRANSCRIPT_TAIL)
            .await
            .expect("tail read");
        assert_eq!(
            trusty_mpm::core::agent_cost::latest_context_tokens(&first),
            None,
            "fixture invalid: 64 KiB must NOT contain a usage record"
        );

        // The retry recovers it, and the guard classifies normally.
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "huge",
        });
        let (status, tokens) = evaluate_agent_cost(&payload, &opted_in_stop()).await;
        assert_eq!(tokens, 500_000, "the larger window must find the record");
        assert_eq!(status, BudgetStatus::Exceeded);
    }

    #[tokio::test]
    async fn still_fails_open_when_even_the_larger_tail_has_no_record() {
        // Growing the window must not weaken the fail-open contract: a
        // transcript with no usage record anywhere still ALLOWS.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, sub) = transcript_tree(tmp.path(), "norec", 500_000);
        std::fs::write(&sub, filler_line(100 * 1024)).expect("write");
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "norec",
        });
        assert_eq!(
            evaluate_agent_cost(&payload, &opted_in_stop()).await,
            (BudgetStatus::Ok, 0)
        );
    }

    // ── #4837 review BLOCK 1(b): the stop must never strand finished work ──

    #[test]
    fn escape_hatch_permits_send_message_and_git_persistence() {
        // The #4841 engineer reached 434k holding a correct fix. Under the
        // first cut it could not have committed, pushed, or reported it.
        assert!(is_persistence_escape("SendMessage", None));
        for command in [
            "git add -A",
            "git commit -m 'fix: the thing'",
            "git push origin HEAD",
            "git -C /repo/wt commit -m x",
            "git add -A && git commit -m x && git push",
        ] {
            assert!(
                is_persistence_escape("Bash", Some(&serde_json::json!({"command": command}))),
                "a stopped agent must still be able to run {command:?}"
            );
        }
    }

    #[test]
    fn escape_hatch_denies_work_tools() {
        // The hatch is exactly "persist and report" wide. Anything that lets
        // the agent keep working would make the stop decorative.
        for tool in ["Write", "Edit", "Read", "Grep", "WebFetch", "Task", "Agent"] {
            assert!(
                !is_persistence_escape(tool, Some(&serde_json::json!({}))),
                "{tool} must stay denied past the ceiling"
            );
        }
        // Bash with no command, or with work smuggled behind an allowed verb.
        assert!(!is_persistence_escape("Bash", None));
        for command in [
            "cargo test",
            "git commit -m x && cargo test",
            "git checkout main",
        ] {
            assert!(
                !is_persistence_escape("Bash", Some(&serde_json::json!({"command": command}))),
                "{command:?} must stay denied past the ceiling"
            );
        }
    }

    #[test]
    fn warn_notice_is_claimed_once_per_agent() {
        // The nudge must not be re-sent on every tool call — that would spend
        // context complaining about context.
        let id = format!("test-{}", std::process::id());
        let payload = serde_json::json!({"agent_id": id});
        let marker = std::env::temp_dir().join("trusty-mpm-agent-cost").join(&id);
        let _ = std::fs::remove_file(&marker);

        assert!(claim_warn_notice(&payload), "first call must claim");
        assert!(!claim_warn_notice(&payload), "second call must not");
        assert!(!claim_warn_notice(&payload), "and neither must the third");

        // A different agent gets its own nudge.
        let other = serde_json::json!({"agent_id": format!("{id}-other")});
        assert!(claim_warn_notice(&other));

        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(
            std::env::temp_dir()
                .join("trusty-mpm-agent-cost")
                .join(format!("{id}-other")),
        );
    }
}
