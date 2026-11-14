//! LLM events and usage record around the Bedrock tool loop (#10980).
//!
//! Why: the Bedrock branch of `chat_with_tools_gated` returned before any
//! `LlmRequested`/`LlmResponded` event or `usage.jsonl` row, so Bedrock tool
//! turns had no model identity or token counts (parity map row 11, M1-T4).
//! What: [`traced_bedrock_call`] wraps the Bedrock call the same way the
//! OpenRouter turn loop wraps `dispatch_turn`.
//! Test: `tests::bedrock_call_writes_usage_row_with_exact_model_id`.

use std::future::Future;
use std::path::Path;

use anyhow::Result;

use crate::llm::events::{dispatch_usage_record, emit_llm_requested, emit_llm_responded};
use crate::perf::TokenUsage;

/// Run one Bedrock tool loop with paired LLM events and one usage row.
///
/// Why: Bedrock's own loop aggregates usage across its turns and returns it
/// once; recording it here gives the Costs tab a row per dispatch.
/// What: emits `LlmRequested` for `model_id`, awaits `call`, emits
/// `LlmResponded` with the returned tokens, then appends a `UsageRecord`
/// (runner `bedrock`, model = the exact Bedrock id) to
/// `<project_dir>/.trusty-agents/state/usage.jsonl`. The append is awaited,
/// not detached, so the row exists when this returns. A failed call
/// propagates its error and writes no row, as the OpenRouter loop does.
/// Test: `tests::bedrock_call_writes_usage_row_with_exact_model_id`.
pub(super) async fn traced_bedrock_call<F, Fut>(
    model_id: &str,
    task_prefix: &str,
    project_dir: &Path,
    call: F,
) -> Result<(String, TokenUsage)>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(String, TokenUsage)>>,
{
    let started = emit_llm_requested(model_id, None);
    let (content, usage) = call().await?;
    let duration_ms = started.elapsed().as_millis() as u64;
    emit_llm_responded(
        model_id,
        started,
        Some(usage.completion_tokens),
        Some(usage.prompt_tokens),
    );
    let record = dispatch_usage_record(
        model_id,
        "bedrock",
        usage.prompt_tokens,
        usage.completion_tokens,
        duration_ms,
        task_prefix,
    );
    crate::usage::append_usage(project_dir, &record).await;
    Ok((content, usage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{self, Event};

    /// Why: #10980 closure — a closure stands in for `bedrock::chat_with_tools`
    /// and the `usage.jsonl` row must carry the exact id, `bedrock` and tokens.
    /// What: runs the wrapper against a tempdir, reads the row back, and
    /// checks the `LlmResponded` event for the same model id.
    /// Test: itself.
    #[tokio::test]
    async fn bedrock_call_writes_usage_row_with_exact_model_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let model_id = "us.anthropic.claude-opus-test-10980-v1:0";
        let mut rx = events::subscribe();

        let (content, usage) =
            traced_bedrock_call(model_id, "plan the beta", dir.path(), || async {
                Ok(("done".to_string(), TokenUsage::new(1234, 56, 0, 0)))
            })
            .await
            .expect("traced call");
        assert_eq!(content, "done");
        assert_eq!(usage.prompt_tokens, 1234);

        let log = dir.path().join(".trusty-agents/state/usage.jsonl");
        let text = std::fs::read_to_string(&log).expect("usage.jsonl written");
        let rows: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).expect("jsonl row"))
            .collect();
        assert_eq!(rows.len(), 1, "one row per Bedrock dispatch: {text}");
        assert_eq!(rows[0]["model"], model_id);
        assert_eq!(rows[0]["runner"], "bedrock");
        assert_eq!(rows[0]["input_tokens"], 1234);
        assert_eq!(rows[0]["output_tokens"], 56);
        assert_eq!(rows[0]["task_prefix"], "plan the beta");

        let mut saw_responded = false;
        while let Ok(ev) = rx.try_recv() {
            if let Event::LlmResponded {
                model,
                completion_tokens,
                ..
            } = ev
                && model == model_id
            {
                assert_eq!(completion_tokens, Some(56));
                saw_responded = true;
            }
        }
        assert!(saw_responded, "LlmResponded not published for {model_id}");
    }
}
