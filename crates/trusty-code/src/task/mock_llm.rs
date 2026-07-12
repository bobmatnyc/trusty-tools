//! Deterministic, offline "echo" LLM client for #2056's mandatory offline
//! testability (no live model, no API key).
//!
//! Why: task execution needs a live LLM in production, but the e2e/CI suite
//! must exercise the FULL flow (create -> attach -> run -> live tool events
//! -> done) without one. Setting `TCODE_MOCK_LLM=echo` (checked by
//! [`build_llm_client`]) swaps this deterministic client in for the real
//! `LlmClient` — the daemon still starts and serves `ping`/`health`/
//! `session.*` fine with neither set; only `task.run` needs one or the
//! other.
//! What: [`EchoLlmClient`] replays a FIXED script exercising exactly the
//! shape #2056 wires: the PM delegates once to `python-engineer`, which
//! runs one `bash` tool call then stops, after which the PM sees the result
//! and stops too. This is production code (not `#[cfg(test)]`) because the
//! REAL `tcode` binary, spawned as a subprocess in `tests/task_e2e.rs`, must
//! be able to construct it with no test harness in scope.
//! Test: `task::mock_llm::tests::*`; exercised end-to-end (as a real
//! subprocess) by `tests/task_e2e.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::jsonrpc::RpcError;
use crate::llm::{ChatRequest, ChatResponse, DispatchingLlmClient, LlmClientTrait, LlmError};

/// Environment variable selecting the mock LLM. Set to [`MOCK_LLM_ECHO`] to
/// enable [`EchoLlmClient`] instead of a real OpenRouter client.
pub const MOCK_LLM_ENV: &str = "TCODE_MOCK_LLM";

/// The `TCODE_MOCK_LLM` value that selects [`EchoLlmClient`].
pub const MOCK_LLM_ECHO: &str = "echo";

/// Build the `Arc<dyn LlmClientTrait>` `task.run` executions share.
///
/// Why: the single seam that decides "real model or offline mock" — kept
/// here (not inlined at each call site) so the decision is made exactly
/// once and is easy to find.
/// What: if [`MOCK_LLM_ENV`] is set to [`MOCK_LLM_ECHO`], returns an
/// [`EchoLlmClient`]. Otherwise builds a real `DispatchingLlmClient` — routing
/// `bedrock/*` slugs to AWS Bedrock, `fireworks/*` to Fireworks, and everything
/// else to OpenRouter (#1021 phase 1; #2406). Construction touches no
/// credentials: the OpenAI-dialect providers resolve their key lazily via the
/// shared env > `.env.local` > store chain at first use, and Bedrock uses the
/// AWS credential chain — so a pure-Bedrock `task.run` is never blocked by a key
/// it will never use (#2245). A missing key only surfaces (via
/// `DispatchingLlmClient::chat` → an actionable error) when a model that needs
/// it is actually dispatched.
/// Test: `task::mock_llm::tests::mock_env_selects_echo_client`,
/// `task::mock_llm::tests::real_client_builds_without_openrouter_key`.
pub fn build_llm_client() -> Result<Arc<dyn LlmClientTrait>, RpcError> {
    if std::env::var(MOCK_LLM_ENV).ok().as_deref() == Some(MOCK_LLM_ECHO) {
        return Ok(Arc::new(EchoLlmClient::new()));
    }
    Ok(Arc::new(DispatchingLlmClient::new()))
}

/// A deterministic, scripted `LlmClientTrait` for offline task-execution
/// testing (#2056).
///
/// Why: exercises the SAME PM -> engineer -> tool -> engineer-stop ->
/// PM-stop shape `run_task`'s own offline tests use, but as production code
/// so the real daemon binary can drive it with no network access at all.
/// What: an atomic cursor over a fixed 4-response script:
///
/// 1. PM calls `delegate_to_agent(python-engineer, ...)`.
/// 2. Engineer calls `bash(command="echo hello-from-mock-engineer")`.
/// 3. Engineer stops with final text.
/// 4. PM stops with final text.
///
/// Running past the script's end returns an `LlmError` rather than
/// panicking, so a bug that adds an unexpected turn fails loudly instead of
/// hanging.
/// Test: `task::mock_llm::tests::script_drives_full_delegation_flow`.
pub struct EchoLlmClient {
    cursor: AtomicUsize,
}

impl EchoLlmClient {
    /// Construct a fresh client at the start of its script.
    pub fn new() -> Self {
        Self {
            cursor: AtomicUsize::new(0),
        }
    }
}

impl Default for EchoLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClientTrait for EchoLlmClient {
    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        let fixture = match idx {
            0 => delegate_response(),
            1 => bash_response(),
            2 => stop_response("engineer: echoed hello-from-mock-engineer"),
            3 => stop_response("pm: task complete"),
            _ => {
                return Err(LlmError::MissingConfig(format!(
                    "EchoLlmClient script exhausted at call {idx}"
                )));
            }
        };
        serde_json::from_value(fixture).map_err(|e| {
            LlmError::MissingConfig(format!("EchoLlmClient: invalid scripted fixture: {e}"))
        })
    }
}

/// Turn 1 fixture: the PM delegates to `python-engineer`.
fn delegate_response() -> Value {
    json!({
        "id": "mock-delegate",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-delegate",
                    "type": "function",
                    "function": {
                        "name": "delegate_to_agent",
                        "arguments": json!({"agent_name": "python-engineer", "task": "echo a greeting"}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 40, "completion_tokens": 10, "total_tokens": 50}
    })
}

/// Turn 2 fixture: the engineer runs a single, known, side-effect-free `bash`
/// command.
fn bash_response() -> Value {
    json!({
        "id": "mock-bash",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-bash",
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": json!({"command": "echo hello-from-mock-engineer"}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}
    })
}

/// A no-tool-call final-answer fixture (turns 3 and 4).
fn stop_response(text: &str) -> Value {
    json!({
        "id": "mock-stop",
        "choices": [{
            "message": {"role": "assistant", "content": text, "tool_calls": []},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

/// Serializes every test in this crate that sets/reads the process-wide
/// [`MOCK_LLM_ENV`] var — `cargo test` runs tests in parallel within one
/// binary, and an unguarded `set_var`/`remove_var` pair would race across
/// modules (both here and in `task::protocol::tests`/`serve::tests`). A
/// `tokio::sync::Mutex` (not `std::sync::Mutex`) because every caller holds
/// the guard across an `.await` (the router dispatch the env var must stay
/// set for) — clippy's `await_holding_lock` correctly flags a std mutex held
/// that way.
#[cfg(test)]
pub(crate) static MOCK_LLM_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// The script must drive exactly the delegate -> bash -> stop -> stop
    /// shape #2056's executor relies on.
    #[tokio::test]
    async fn script_drives_full_delegation_flow() {
        let client = EchoLlmClient::new();
        let req = ChatRequest {
            model: "mock".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
        };

        let turn1 = client.chat(&req).await.expect("turn 1");
        assert_eq!(
            turn1.first_tool_calls()[0].function.name,
            "delegate_to_agent"
        );

        let turn2 = client.chat(&req).await.expect("turn 2");
        assert_eq!(turn2.first_tool_calls()[0].function.name, "bash");

        let turn3 = client.chat(&req).await.expect("turn 3");
        assert!(turn3.first_tool_calls().is_empty());

        let turn4 = client.chat(&req).await.expect("turn 4");
        assert!(turn4.first_tool_calls().is_empty());

        let err = client.chat(&req).await;
        assert!(err.is_err(), "the script must not silently repeat");
    }

    /// `build_llm_client` must select the echo client when the env var is set.
    #[tokio::test]
    async fn mock_env_selects_echo_client() {
        let _guard = MOCK_LLM_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `MOCK_LLM_ENV_LOCK`.
        unsafe {
            std::env::set_var(MOCK_LLM_ENV, MOCK_LLM_ECHO);
        }
        let result = build_llm_client();
        unsafe {
            std::env::remove_var(MOCK_LLM_ENV);
        }
        assert!(result.is_ok(), "echo mode must never fail to construct");
    }

    /// `build_llm_client` must succeed even when `OPENROUTER_API_KEY` is
    /// unset and mock mode is off — the real daemon `task.run` path a
    /// pure-Bedrock run takes (#2245).
    ///
    /// Why: This is the exact function the default (non-`--legacy-in-process`)
    /// `tcode run-task` path calls inside the daemon; pinning the fix here,
    /// not just in `main.rs`, covers both entry points the smoke test's root
    /// cause named.
    /// What: Locks out `mock_env_selects_echo_client` (both mutate process
    /// env), unsets `OPENROUTER_API_KEY`, asserts `Ok`.
    /// Test: this test.
    #[tokio::test]
    async fn real_client_builds_without_openrouter_key() {
        let _guard = MOCK_LLM_ENV_LOCK.lock().await;
        let prev = std::env::var("OPENROUTER_API_KEY").ok();
        // SAFETY: test-only env mutation; serialized by `MOCK_LLM_ENV_LOCK`
        // (every test touching process-wide LLM-selection env vars in this
        // module takes that lock, so this is safe alongside them too).
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
        let result = build_llm_client();
        if let Some(key) = prev {
            // SAFETY: see above.
            unsafe {
                std::env::set_var("OPENROUTER_API_KEY", key);
            }
        }
        assert!(
            result.is_ok(),
            "expected Ok when OPENROUTER_API_KEY is unset, got err: {:?}",
            result.err()
        );
    }
}
