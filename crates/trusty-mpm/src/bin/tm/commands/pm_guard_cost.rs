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
//! classifies the result via [`evaluate_cost`].
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

/// Cap on transcript bytes read from the tail.
///
/// Why: the newest `usage` block sits at the very end of the JSONL, so a small
/// window is sufficient — and bounding it is what keeps the guard's cost
/// constant on the multi-hundred-megabyte transcripts it exists to catch. 64
/// KiB comfortably spans several assistant turns even with large tool results
/// interleaved, so a usage record is found on the first read in practice.
/// What: byte count passed to [`super::misc::read_transcript_tail`].
/// Test: exercised by `evaluate_agent_cost` callers; the truncated-line
/// tolerance it relies on is pinned by
/// `core::agent_cost::tolerates_a_truncated_leading_line`.
const MAX_TRANSCRIPT_TAIL: u64 = 64 * 1024;

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
/// What: resolves the transcript, tail-reads [`MAX_TRANSCRIPT_TAIL`] bytes
/// under [`EVAL_TIMEOUT`], extracts the newest context size, and returns
/// `(status, context_tokens)`. Any failure returns
/// `(`[`BudgetStatus::Ok`]`, 0)` — fail open.
/// Test: `fails_open_when_the_transcript_is_missing`,
/// `reports_exceeded_for_an_over_ceiling_transcript`,
/// `respects_a_disabled_config`.
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
    let tail = tokio::time::timeout(
        EVAL_TIMEOUT,
        super::misc::read_transcript_tail(&path, MAX_TRANSCRIPT_TAIL),
    )
    .await;
    let Ok(Some(jsonl)) = tail else {
        return (BudgetStatus::Ok, 0);
    };
    let Some(tokens) = trusty_mpm::core::agent_cost::latest_context_tokens(&jsonl) else {
        return (BudgetStatus::Ok, 0);
    };
    (evaluate_cost(tokens, config), tokens)
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

    #[tokio::test]
    async fn reports_exceeded_for_an_over_ceiling_transcript() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (parent, _) = transcript_tree(tmp.path(), "big", 622_200);
        let payload = serde_json::json!({
            "transcript_path": parent.to_str().expect("utf8"),
            "agent_id": "big",
        });
        let (status, tokens) = evaluate_agent_cost(&payload, &AgentCostConfig::default()).await;
        assert_eq!(status, BudgetStatus::Exceeded);
        assert_eq!(tokens, 622_200);
        // And the reason handed back must carry the measured number.
        assert!(stop_reason(tokens, 400_000).contains("622200"));
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
}
