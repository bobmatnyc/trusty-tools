//! Transient-error classification and per-file retry with backoff (#206/#231).
//!
//! Why: Anthropic and OpenRouter return transient 5xx / 429 errors during
//! brief overload windows that resolve within seconds. Aborting an entire
//! workflow run on such an error discards all already-completed files. This
//! module classifies errors and retries per-file agent dispatch with
//! exponential backoff plus optional model elevation.
//! What: `is_retryable` inspects an error string for transient signals;
//! `run_wave_file_with_retry` drives the retry + elevation loop around a single
//! per-file agent invocation.
//! Test: `is_retryable_classifies_5xx`, `is_retryable_rejects_4xx` below; the
//! retry/elevation integration paths are covered in `executor`'s test module.

use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};

/// #206: Classify an error as retryable (transient API/network fault) or fatal.
///
/// Why: Anthropic and OpenRouter return transient 5xx / 429 errors during
/// brief overload windows that resolve within seconds. Aborting an entire
/// workflow run on such an error discards all already-completed files and
/// forces the operator to restart from scratch. Distinguishing transient
/// from fatal errors lets the wave loop retry safely without masking real
/// failures (bad request, auth error, missing config).
///
/// What: Inspects the stringified error for well-known transient signal
/// phrases. Any HTTP 4xx that is NOT 429 is NOT retryable -- it would fail
/// identically on retry. Fatal logic errors (empty output, missing TOML)
/// are also non-retryable.
///
/// Test: `is_retryable_classifies_5xx`, `is_retryable_rejects_4xx`.
pub(crate) fn is_retryable(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    // Explicit HTTP status codes embedded in error strings.
    if msg.contains("status 500")
        || msg.contains("status 502")
        || msg.contains("status 503")
        || msg.contains("status 529")
        || msg.contains("status 429")
        || msg.contains("http 500")
        || msg.contains("http 502")
        || msg.contains("http 503")
        || msg.contains("http 529")
        || msg.contains("http 429")
        || msg.contains("error 500")
        || msg.contains("error 502")
        || msg.contains("error 503")
        || msg.contains("error 529")
        || msg.contains("error 429")
    {
        return true;
    }
    // Textual signals from Anthropic / OpenRouter error bodies.
    msg.contains("internal server error")
        || msg.contains("overloaded")
        || msg.contains("service unavailable")
        || msg.contains("bad gateway")
        || msg.contains("too many requests")
        || msg.contains("rate limit")
        || msg.contains("timeout")
        || msg.contains("timed out")
        || msg.contains("connection reset")
        || msg.contains("connection refused")
        || msg.contains("broken pipe")
}

/// #206: Invoke a single per-file wave agent with exponential backoff retry.
///
/// Why: Transient Anthropic 5xx / 429 errors during a wave cause the entire
/// workflow to abort, discarding completed files. Retrying with backoff gives
/// the API time to recover without operator intervention.
///
/// What: Calls `runner.run_with_context` up to `MAX_WAVE_RETRIES + 1` times.
/// On a retryable error it waits `BASE_DELAY_MS * 2^attempt` ms before the
/// next attempt and logs a WARN line. On a non-retryable (fatal) error it
/// returns immediately. After all retries are exhausted it returns the last
/// error.
///
/// The retry is placed at the *per-file agent dispatch* level rather than
/// inside the LLM client so it covers all runner paths (subprocess,
/// in-process, claude-code) uniformly and avoids billing surprises from
/// partial LLM responses being retried at the HTTP level.
///
/// Test: `wave_loop_retries_on_transient_error`,
///       `wave_loop_does_not_retry_fatal_error`.
pub(crate) async fn run_wave_file_with_retry(
    runner: &dyn AgentRunner,
    agent_name: &str,
    task: &str,
    ctx: &RunContext,
    elevation_threshold: Option<u32>,
    elevation_model: Option<&str>,
) -> anyhow::Result<AgentOutput> {
    const MAX_WAVE_RETRIES: u32 = 2;
    const BASE_DELAY_MS: u64 = 2_000;

    let mut last_err: Option<anyhow::Error> = None;
    let mut total_attempts: u32 = 0;
    for attempt in 0..=MAX_WAVE_RETRIES {
        total_attempts = attempt + 1;
        match runner.run_with_context(agent_name, task, ctx).await {
            Ok(output) => return Ok(output),
            Err(e) if is_retryable(&e) && attempt < MAX_WAVE_RETRIES => {
                let delay_ms = BASE_DELAY_MS * (1u64 << attempt);
                tracing::warn!(
                    attempt = attempt + 1,
                    max_retries = MAX_WAVE_RETRIES,
                    delay_ms,
                    agent = %agent_name,
                    error = %e,
                    "wave-loop: transient error, retrying after backoff"
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                last_err = Some(e);
            }
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }

    // #231: Model elevation — after exhausting normal retries on the base
    // model, optionally make ONE more attempt on the elevation model. The
    // threshold is the number of failures required to trigger elevation;
    // because we always run up to MAX_WAVE_RETRIES + 1 attempts on the base
    // model, any threshold <= total_attempts triggers when both fields are set.
    if let (Some(threshold), Some(elev_model)) = (elevation_threshold, elevation_model)
        && total_attempts >= threshold
    {
        tracing::info!(
            agent = %agent_name,
            from_model = ?ctx.model,
            to_model = %elev_model,
            attempts = total_attempts,
            threshold,
            "wave-loop: model elevation triggered after repeated failures"
        );
        let mut elevated_ctx = ctx.clone();
        elevated_ctx.model = Some(elev_model.to_string());
        return runner
            .run_with_context(agent_name, task, &elevated_ctx)
            .await;
    }

    // No elevation configured (or threshold not met) -- return the last error.
    Err(last_err.expect("retry loop exited without capturing an error"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_retryable` must return true for textual 5xx / 429 / timeout signals.
    ///
    /// Why: The retry gate depends on string matching against the error message;
    /// these cases cover the most common Anthropic / OpenRouter transient errors.
    /// Test: assert true for each retryable string, false for a 400 / auth error.
    #[test]
    fn is_retryable_classifies_5xx() {
        let cases = [
            "API returned status 500: internal server error",
            "HTTP 502 bad gateway",
            "error 503 service unavailable",
            "status 529 overloaded",
            "HTTP 429 too many requests",
            "rate limit exceeded",
            "connection timed out after 30s",
            "connection reset by peer",
            "internal server error from upstream",
            "service unavailable, please retry",
            "overloaded — try again shortly",
            "broken pipe",
        ];
        for msg in &cases {
            let err = anyhow::anyhow!("{}", msg);
            assert!(is_retryable(&err), "expected retryable=true for: {msg}");
        }
    }

    /// `is_retryable` must return false for 4xx (non-429) and logic errors.
    ///
    /// Why: These errors will not resolve on retry; retrying them wastes time
    /// and could mask misconfigured agents or bad task prompts.
    /// Test: assert false for 400, 401, 403, 404, empty output, missing TOML.
    #[test]
    fn is_retryable_rejects_4xx() {
        let cases = [
            "status 400 bad request",
            "HTTP 401 unauthorized",
            "error 403 forbidden",
            "404 not found",
            "agent produced empty output",
            "failed to read agent config: no such file",
        ];
        for msg in &cases {
            let err = anyhow::anyhow!("{}", msg);
            assert!(!is_retryable(&err), "expected retryable=false for: {msg}");
        }
    }
}
