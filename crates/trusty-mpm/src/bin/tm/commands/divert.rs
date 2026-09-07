//! `tm divert bulk-read` — the cheap worker the diversion hook steers to (#6887).
//!
//! Why: when `tm hook --divert-check` blocks an oversized read, this is the
//! command it names. It reads the files itself, asks a CHEAP model the agent's
//! question about them, and prints only the answer — so the session's context
//! carries a paragraph instead of two thousand lines. The raw file content
//! never reaches the session transcript.
//!
//! What: [`run`] dispatches the `bulk-read` action. It routes through
//! [`ProviderRegistry::build`] with [`SmModelTier::Summary`] (the inexpensive
//! tier) so provider selection, prefix routing, and the `auto` credential chain
//! are the registry's existing decisions — there is NO hard-coded Bedrock call
//! and no credential anywhere in this module. On any provider error it prints
//! [`FALLTHROUGH_MARKER`] and exits [`FALLTHROUGH_EXIT`], which the hook's own
//! block reason tells the agent to read as "retry with `offset`/`limit`". That
//! distinguishable signal is the point: a bare non-zero exit would look like a
//! transient failure worth retrying, and the agent would loop.
//!
//! Each SUCCESSFUL round trip posts one [`HookEvent::TokenUsageUpdate`] through
//! the daemon's existing `/hooks` relay. A bare hook block increments nothing —
//! only real worker traffic is counted, because the number is meant to answer
//! "what did diversion save", not "how often did we say no". There is no
//! bespoke counter file; #6873's usage ledger is not merged, and when it lands
//! it can read these events.
//! Test: the `#[cfg(test)]` suite in `divert_tests.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use trusty_mpm::core::sm::config::SmInferenceConfig;
use trusty_mpm::core::sm::providers::{
    ChatMessage, LlmProvider, LlmRequest, ProviderRegistry, SmModelTier,
};

use crate::cli::DivertAction;

/// Stdout/stderr marker meaning "no worker ran; do it yourself".
///
/// Why: the hook's block reason quotes this string verbatim, so the agent has a
/// literal to match rather than a heuristic on exit codes it cannot see. Change
/// it here and `divert_check::block_reason` must change with it — the test
/// `fallthrough_marker_matches_the_hook_reason` pins the pair.
/// What: the exact bytes printed on any provider failure.
/// Test: `bulk_read_answer_signals_fall_through_on_provider_error`.
pub(crate) const FALLTHROUGH_MARKER: &str = "divert: fall-through";

/// Process exit code accompanying [`FALLTHROUGH_MARKER`].
///
/// Why: a caller scripting around this command needs to branch without parsing
/// prose. `3` is unused by `tm`'s other exit codes (75 is daemon-unavailable,
/// 0/1/2 are the ordinary success/failure/usage set).
/// What: `3`.
/// Test: covered by the marker test above.
pub(crate) const FALLTHROUGH_EXIT: i32 = 3;

/// Maximum bytes of file content sent to the worker in one call.
///
/// Why: the worker's context is finite and cheap-tier models are the smallest
/// ones. Truncating explicitly beats a provider-side error that reads as a
/// transport failure and triggers the fall-through path for a reason the
/// operator cannot diagnose.
/// What: 400 KiB, roughly 100k tokens of source — comfortably more than the
/// files a `min_lines` threshold diverts, and bounded.
/// Test: `read_sources_truncates_past_the_budget`.
const MAX_CONTENT_BYTES: usize = 400 * 1024;

/// The worker's answer, or the reason there was none.
///
/// Why: separating the outcome from the printing/exiting is what lets the
/// fall-through branch be tested with a scripted provider and no process.
/// What: `Answered` carries the text plus the telemetry the usage event needs;
/// `FallThrough` carries the provider error's own message.
/// Test: `divert_tests.rs`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BulkReadOutcome {
    /// The worker replied.
    Answered {
        /// The worker's answer, printed to stdout.
        text: String,
        /// Bare model id the worker actually used.
        model: String,
        /// Prompt tokens the worker consumed — the stand-in for what the
        /// session would have spent reading the files itself.
        input_tokens: u32,
        /// Completion tokens, which the session DOES pay for.
        output_tokens: u32,
        /// The worker call's estimated USD cost.
        cost_usd: f64,
    },
    /// No worker ran; the caller must fall back to a bounded read.
    FallThrough {
        /// The provider error, verbatim, for the operator's stderr.
        reason: String,
    },
}

/// Dispatch a `tm divert` action.
///
/// Why: one entry point per command group, mirroring `commands::memory::run`.
/// What: currently only `bulk-read`. Prints the worker's answer on stdout and
/// exits [`FALLTHROUGH_EXIT`] when no worker could run.
/// Test: `cli_parses_divert_bulk_read`.
pub(crate) async fn run(action: DivertAction, url: &str) -> anyhow::Result<()> {
    match action {
        DivertAction::BulkRead {
            files,
            prompt,
            max_tokens,
        } => bulk_read(files, prompt, max_tokens, url).await,
    }
}

/// Answer a question about `files` on the cheap worker model.
///
/// Why: this is the whole point of the feature — the expensive session never
/// sees the bytes.
/// What: reads the files, resolves a [`SmModelTier::Summary`] provider from the
/// environment, asks, prints the answer, and posts one usage event. Any
/// provider error prints [`FALLTHROUGH_MARKER`] to stdout (so the agent, which
/// only sees stdout, can act on it) with the detail on stderr, then exits
/// [`FALLTHROUGH_EXIT`].
/// Test: `bulk_read_answer_returns_the_worker_text`,
/// `bulk_read_answer_signals_fall_through_on_provider_error`.
async fn bulk_read(
    files: Vec<PathBuf>,
    prompt: Option<String>,
    max_tokens: u32,
    url: &str,
) -> anyhow::Result<()> {
    let question = prompt.unwrap_or_else(|| {
        "Summarize these files: what they are for, their public surface, and \
         anything a reader must know before editing them."
            .to_string()
    });
    let content = read_sources(&files)?;

    let outcome = match resolve_worker().await {
        Ok((provider, model)) => {
            bulk_read_answer(provider.as_ref(), &model, &content, &question, max_tokens).await
        }
        Err(reason) => BulkReadOutcome::FallThrough { reason },
    };

    match outcome {
        BulkReadOutcome::Answered {
            text,
            model,
            input_tokens,
            output_tokens,
            cost_usd,
        } => {
            println!("{text}");
            emit_diversion_usage(url, &model, input_tokens, output_tokens, cost_usd).await;
            Ok(())
        }
        BulkReadOutcome::FallThrough { reason } => {
            // stdout, because the agent reading this command's output is the
            // party that must act on it; the detail goes to stderr.
            println!("{FALLTHROUGH_MARKER}");
            eprintln!("divert: no worker answered ({reason})");
            std::process::exit(FALLTHROUGH_EXIT);
        }
    }
}

/// Resolve the worker provider and bare model id from the session environment.
///
/// Why: the resolved (non-secret) `[divert]` config arrives as
/// `TRUSTY_DIVERT_WORKER_*`; credentials arrive separately and are read by
/// [`ProviderRegistry::from_env`] inside THIS process, never written into
/// settings.json or a hook command string.
/// What: builds a synthetic [`SmInferenceConfig`] whose `summary_model` is the
/// configured worker model (empty → the tier default) and whose `provider` is
/// the configured selector, then calls
/// [`ProviderRegistry::build`] with [`SmModelTier::Summary`]. Every failure —
/// unknown provider, a provider/model contradiction, no credentials, a
/// `bedrock/` model in a build without the feature — comes back as the
/// registry's own error string. Nothing here re-implements that routing.
/// Test: `resolve_worker_reports_a_provider_model_contradiction`.
async fn resolve_worker() -> Result<(Arc<dyn LlmProvider>, String), String> {
    let cfg = worker_config(
        std::env::var(trusty_mpm::core::mcp_session_env::DIVERT_WORKER_MODEL_ENV).ok(),
        std::env::var(trusty_mpm::core::mcp_session_env::DIVERT_WORKER_PROVIDER_ENV).ok(),
    );
    ProviderRegistry::from_env()
        .build(&cfg, SmModelTier::Summary)
        .await
        .map(|resolved| (resolved.provider, resolved.model))
        .map_err(|e| e.to_string())
}

/// Build the synthetic inference config the worker call resolves through.
///
/// Why: pure, so the provider/model contradiction case can be asserted without
/// credentials or a network. The registry owns the routing rules; this only
/// states the inputs.
/// What: `summary_model` is `model` when non-empty, else the
/// [`SmInferenceConfig`] default's own summary tier; `provider` is `provider`
/// when non-empty, else `"auto"`.
/// Test: `worker_config_falls_back_to_the_tier_default`,
/// `resolve_worker_reports_a_provider_model_contradiction`.
pub(crate) fn worker_config(model: Option<String>, provider: Option<String>) -> SmInferenceConfig {
    let defaults = SmInferenceConfig::default();
    SmInferenceConfig {
        provider: provider
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "auto".to_string()),
        summary_model: model
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(defaults.summary_model),
        ..defaults
    }
}

/// Ask the worker one question about the gathered content.
///
/// Why: the seam a scripted [`LlmProvider`] plugs into, so the error path is a
/// real tested branch rather than a claim.
/// What: sends one system + one user turn and maps the result onto
/// [`BulkReadOutcome`]. A provider `Err` of ANY kind — degraded, transport,
/// timeout, malformed — becomes `FallThrough`, never a bare failure.
/// Test: `bulk_read_answer_returns_the_worker_text`,
/// `bulk_read_answer_signals_fall_through_on_provider_error`.
pub(crate) async fn bulk_read_answer(
    provider: &dyn LlmProvider,
    model: &str,
    content: &str,
    question: &str,
    max_tokens: u32,
) -> BulkReadOutcome {
    let request = LlmRequest {
        model: model.to_string(),
        system: "You are a file-reading worker for a coding agent. Answer the \
                 question about the supplied files precisely and briefly. Quote \
                 exact identifiers and line-relevant details; never invent \
                 content that is not present."
            .to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: format!("{question}\n\n{content}"),
        }],
        temperature: 0.0,
        max_tokens,
    };
    match provider.complete(request).await {
        Ok(response) => BulkReadOutcome::Answered {
            text: response.text,
            model: response.model,
            input_tokens: response.input_tokens,
            output_tokens: response.output_tokens,
            cost_usd: response.cost_usd,
        },
        Err(e) => BulkReadOutcome::FallThrough {
            reason: e.to_string(),
        },
    }
}

/// Read every requested file into one labelled blob.
///
/// Why: the worker needs to know which bytes came from which file, and the
/// caller needs a hard failure when a named file cannot be read — a silently
/// skipped file would produce an answer about the wrong thing.
/// What: concatenates `=== <path> ===` headers with each file's text, stopping
/// once [`MAX_CONTENT_BYTES`] is reached and noting the truncation inline.
/// Test: `read_sources_labels_each_file`, `read_sources_truncates_past_the_budget`.
fn read_sources(files: &[PathBuf]) -> anyhow::Result<String> {
    let mut out = String::new();
    for file in files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("divert: cannot read {}: {e}", file.display()))?;
        out.push_str("=== ");
        out.push_str(&file.display().to_string());
        out.push_str(" ===\n");
        if out.len() + text.len() > MAX_CONTENT_BYTES {
            let mut cut = MAX_CONTENT_BYTES.saturating_sub(out.len()).min(text.len());
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push_str(&text[..cut]);
            out.push_str("\n… [truncated: content budget reached]\n");
            break;
        }
        out.push_str(&text);
        out.push('\n');
    }
    Ok(out)
}

/// Post one `TokenUsageUpdate` recording a successful diversion.
///
/// Why (#6887, and the design's precondition (a)): #6873's usage ledger is not
/// merged, so the interim counter rides the hook-event path that already
/// exists. Emitting ONLY here — after a real round trip — is what keeps the
/// number honest: a bare hook block spends nothing and must count nothing.
/// What: `POST /hooks` with `event = "TokenUsageUpdate"` and a payload carrying
/// `diversion: true`, the saved-token estimate, and the worker's model and
/// provider. Best-effort: a down daemon must never fail the worker call the
/// user is waiting on.
/// Test: `diversion_usage_payload_carries_the_diversion_marker`.
async fn emit_diversion_usage(
    url: &str,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cost_usd: f64,
) {
    let provider = std::env::var(trusty_mpm::core::mcp_session_env::DIVERT_WORKER_PROVIDER_ENV)
        .unwrap_or_else(|_| "auto".to_string());
    let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    let body = serde_json::json!({
        "session_id": session_id,
        "event": trusty_mpm::core::hook::HookEvent::TokenUsageUpdate,
        "payload": diversion_usage_payload(model, &provider, input_tokens, output_tokens, cost_usd),
    });
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return;
    };
    if let Err(e) = client.post(format!("{url}/hooks")).json(&body).send().await {
        tracing::debug!("divert: usage event not recorded: {e}");
    }
}

/// Build the `TokenUsageUpdate` payload for one diversion.
///
/// Why: pure, so the shape #6873 will later read can be asserted without a
/// daemon.
/// What: `tokens_saved_estimate` is the worker's prompt tokens minus its
/// completion tokens — what the session WOULD have absorbed reading the files
/// itself, less what it now absorbs reading the answer. Saturating, so a
/// worker that answered at length reports zero saved rather than a negative.
/// Test: `diversion_usage_payload_carries_the_diversion_marker`.
pub(crate) fn diversion_usage_payload(
    model: &str,
    provider: &str,
    input_tokens: u32,
    output_tokens: u32,
    cost_usd: f64,
) -> serde_json::Value {
    serde_json::json!({
        "diversion": true,
        "tokens_saved_estimate": input_tokens.saturating_sub(output_tokens),
        "worker_model": model,
        "worker_provider": provider,
        "worker_input_tokens": input_tokens,
        "worker_output_tokens": output_tokens,
        "worker_cost_usd": cost_usd,
    })
}

#[cfg(test)]
#[path = "divert_tests.rs"]
mod tests;
