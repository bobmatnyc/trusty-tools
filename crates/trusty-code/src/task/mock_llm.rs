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

/// The `TCODE_MOCK_LLM` value that selects [`FanoutEchoLlmClient`] (DOC-39
/// AC-13).
///
/// Why: [`EchoLlmClient`]'s fixed script delegates to `python-engineer`
/// exactly ONCE, which cannot exercise "two delegations to the SAME
/// `agent_name`" — the acceptance proof AC-13 requires. A separate opt-in
/// value keeps every existing `MOCK_LLM_ECHO` consumer (and its fixed
/// 4-response script) byte-identical; only a test that explicitly asks for
/// the fan-out shape sees it.
pub const MOCK_LLM_ECHO_FANOUT: &str = "echo-fanout";

/// The `TCODE_MOCK_LLM` value that selects [`RecallEchoLlmClient`] (DOC-39
/// Slice C).
///
/// Why: neither [`EchoLlmClient`] nor [`FanoutEchoLlmClient`]'s scripts ever
/// call `recall_session` — Slice C's e2e proof (recalled TEXT + `run_id`
/// reaching `Event::MemoryRecalled` for a HELD-BACK result) needs the PM to
/// issue that call against a mock trusty-memory backend, so this is its own
/// opt-in value, same pattern as [`MOCK_LLM_ECHO_FANOUT`].
pub const MOCK_LLM_ECHO_RECALL: &str = "echo-recall";

/// The `TCODE_MOCK_LLM` value that selects [`SearchEchoLlmClient`] (DOC-39
/// Slice B).
///
/// Why: neither [`EchoLlmClient`] nor [`FanoutEchoLlmClient`]'s scripts ever
/// call `search_code` — the mandatory real-wire e2e proof that
/// `Event::SearchPerformed.hits` carries real per-hit path/score data needs
/// a script that does.
pub const MOCK_LLM_ECHO_SEARCH: &str = "echo-search";

/// Build the `Arc<dyn LlmClientTrait>` `task.run` executions share.
///
/// Why: the single seam that decides "real model or offline mock" — kept
/// here (not inlined at each call site) so the decision is made exactly
/// once and is easy to find.
/// What: if [`MOCK_LLM_ENV`] is set to [`MOCK_LLM_ECHO`], returns an
/// [`EchoLlmClient`]; if set to [`MOCK_LLM_ECHO_FANOUT`] (DOC-39 AC-13),
/// returns a [`FanoutEchoLlmClient`]; if set to [`MOCK_LLM_ECHO_SEARCH`]
/// (DOC-39 Slice B), returns a [`SearchEchoLlmClient`]. Otherwise builds a
/// real `DispatchingLlmClient` — routing
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
    match std::env::var(MOCK_LLM_ENV).ok().as_deref() {
        Some(MOCK_LLM_ECHO) => return Ok(Arc::new(EchoLlmClient::new())),
        Some(MOCK_LLM_ECHO_FANOUT) => return Ok(Arc::new(FanoutEchoLlmClient::new())),
        Some(MOCK_LLM_ECHO_RECALL) => return Ok(Arc::new(RecallEchoLlmClient::new())),
        Some(MOCK_LLM_ECHO_SEARCH) => return Ok(Arc::new(SearchEchoLlmClient::new())),
        _ => {}
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

/// A deterministic, scripted `LlmClientTrait` exercising a FAN-OUT to two
/// concurrently-delegated `python-engineer` sub-agents (DOC-39 AC-13's
/// acceptance proof).
///
/// Why: [`EchoLlmClient`]'s fixed script delegates to `python-engineer`
/// exactly once, so it cannot prove the fix this ticket ships — that TWO
/// delegations to the SAME `agent_name` mint DISTINCT `agent_id`s. This
/// client scripts the PM issuing both `delegate_to_agent` calls in ONE
/// assistant turn (a genuine tool-calls fan-out); `agent_loop::dispatch_all`
/// still dispatches them one after another (there is no concurrent-execution
/// primitive in the loop itself), but each dispatch re-enters
/// `runner::in_process::InProcessAgentRunner::run_pipeline` — the ONE
/// production call site that mints a fresh UUID v4 per spawn — so the
/// two engineer sub-loops still get their OWN `agent_id` even though they run
/// sequentially. That is exactly what AC-13 requires: `agent_name` alone
/// cannot distinguish the two, `agent_id` can.
/// What: an atomic cursor over a fixed 6-response script:
///
/// 1. PM's ONE assistant turn issues TWO `delegate_to_agent` tool calls, both
///    targeting `python-engineer` (`call-delegate-a`, `call-delegate-b`).
/// 2. Engineer A calls `bash(command="echo hello-from-engineer-a")`.
/// 3. Engineer A stops with final text.
/// 4. Engineer B calls `bash(command="echo hello-from-engineer-b")`.
/// 5. Engineer B stops with final text.
/// 6. PM stops with final text.
///
/// Running past the script's end returns an `LlmError` rather than panicking.
/// Test: `task::mock_llm::tests::fanout_script_drives_two_delegations_to_the_same_agent`;
/// exercised end-to-end (as a real subprocess) by `tests/agent_id_e2e.rs`.
pub struct FanoutEchoLlmClient {
    cursor: AtomicUsize,
}

impl FanoutEchoLlmClient {
    /// Construct a fresh client at the start of its script.
    pub fn new() -> Self {
        Self {
            cursor: AtomicUsize::new(0),
        }
    }
}

impl Default for FanoutEchoLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClientTrait for FanoutEchoLlmClient {
    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        let fixture = match idx {
            0 => fanout_delegate_response(),
            1 => bash_response_named("call-bash-a", "echo hello-from-engineer-a"),
            2 => stop_response("engineer A: done"),
            3 => bash_response_named("call-bash-b", "echo hello-from-engineer-b"),
            4 => stop_response("engineer B: done"),
            5 => stop_response("pm: fan-out complete"),
            _ => {
                return Err(LlmError::MissingConfig(format!(
                    "FanoutEchoLlmClient script exhausted at call {idx}"
                )));
            }
        };
        serde_json::from_value(fixture).map_err(|e| {
            LlmError::MissingConfig(format!(
                "FanoutEchoLlmClient: invalid scripted fixture: {e}"
            ))
        })
    }
}

/// Turn 1 fixture: the PM's ONE assistant turn fans out to `python-engineer`
/// TWICE — the two-tool-calls-in-one-turn shape `FanoutEchoLlmClient` needs.
fn fanout_delegate_response() -> Value {
    json!({
        "id": "mock-fanout-delegate",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call-delegate-a",
                        "type": "function",
                        "function": {
                            "name": "delegate_to_agent",
                            "arguments": json!({"agent_name": "python-engineer", "task": "task A"}).to_string()
                        }
                    },
                    {
                        "id": "call-delegate-b",
                        "type": "function",
                        "function": {
                            "name": "delegate_to_agent",
                            "arguments": json!({"agent_name": "python-engineer", "task": "task B"}).to_string()
                        }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 50, "completion_tokens": 20, "total_tokens": 70}
    })
}

/// Like [`bash_response`] but with a caller-chosen `call_id`/command, so the
/// fan-out script can distinguish engineer A's and engineer B's `bash` calls.
fn bash_response_named(call_id: &str, command: &str) -> Value {
    json!({
        "id": "mock-bash",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": json!({"command": command}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}
    })
}

/// A deterministic, scripted `LlmClientTrait` exercising the PM calling
/// `recall_session` (DOC-39 Slice C's e2e proof).
///
/// Why: [`EchoLlmClient`] and [`FanoutEchoLlmClient`]'s scripts never call
/// `recall_session`, so neither can drive the wire proof that a HELD-BACK
/// recall result's actual TEXT (not just its score) reaches
/// `Event::MemoryRecalled`. This script has the PM call `recall_session`
/// exactly once, then stop — `tests/recall_content_e2e.rs` pairs it with a
/// mock trusty-memory backend (`TRUSTY_MEMORY_URL` override) that returns one
/// huge, high-scored result and one small, lower-scored result so the tool's
/// own token budget drops the second one whole (mirroring
/// `tools::recall_session`'s own budget tests) — the held-back case this
/// slice must prove survives onto the wire.
/// What: an atomic cursor over a fixed 2-response script:
///
/// 1. PM calls `recall_session(query="pkce oauth flow")`.
/// 2. PM stops with final text.
///
/// Running past the script's end returns an `LlmError` rather than panicking.
/// Test: `task::mock_llm::tests::recall_script_drives_a_single_recall_call`;
/// exercised end-to-end (as a real subprocess) by
/// `tests/recall_content_e2e.rs`.
pub struct RecallEchoLlmClient {
    cursor: AtomicUsize,
}

impl RecallEchoLlmClient {
    /// Construct a fresh client at the start of its script.
    pub fn new() -> Self {
        Self {
            cursor: AtomicUsize::new(0),
        }
    }
}

impl Default for RecallEchoLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClientTrait for RecallEchoLlmClient {
    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        let fixture = match idx {
            0 => recall_response(),
            1 => stop_response("pm: recalled what I needed"),
            _ => {
                return Err(LlmError::MissingConfig(format!(
                    "RecallEchoLlmClient script exhausted at call {idx}"
                )));
            }
        };
        serde_json::from_value(fixture).map_err(|e| {
            LlmError::MissingConfig(format!(
                "RecallEchoLlmClient: invalid scripted fixture: {e}"
            ))
        })
    }
}

/// Turn 1 fixture: the PM calls `recall_session` once.
fn recall_response() -> Value {
    json!({
        "id": "mock-recall",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-recall",
                    "type": "function",
                    "function": {
                        "name": "recall_session",
                        "arguments": json!({"query": "pkce oauth flow"}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 30, "completion_tokens": 10, "total_tokens": 40}
    })
}

/// A deterministic, scripted `LlmClientTrait` exercising the engineer's
/// `search_code` tool (DOC-39 Slice B's mandatory real-wire e2e proof that
/// `Event::SearchPerformed.hits` carries real per-hit path/score data, not
/// just a count).
///
/// Why: [`EchoLlmClient`]'s script only ever calls `bash`, so it cannot drive
/// `search_code` at all. This client's fixed script is otherwise identical
/// in shape (delegate once, one tool call, two stops) so it composes with
/// the same daemon-driving harness `tests/task_e2e.rs` already uses.
/// What: an atomic cursor over a fixed 4-response script:
///
/// 1. PM calls `delegate_to_agent(python-engineer, ...)`.
/// 2. Engineer calls `search_code(query="where does auth live", mode="semantic")`.
/// 3. Engineer stops with final text.
/// 4. PM stops with final text.
///
/// Running past the script's end returns an `LlmError` rather than
/// panicking, mirroring [`EchoLlmClient`].
/// Test: `task::mock_llm::tests::search_script_drives_full_delegation_flow`;
/// exercised end-to-end (as a real subprocess, against a fake trusty-search
/// MCP binary) by `tests/search_hits_e2e.rs`.
pub struct SearchEchoLlmClient {
    cursor: AtomicUsize,
}

impl SearchEchoLlmClient {
    /// Construct a fresh client at the start of its script.
    pub fn new() -> Self {
        Self {
            cursor: AtomicUsize::new(0),
        }
    }
}

impl Default for SearchEchoLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClientTrait for SearchEchoLlmClient {
    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        let fixture = match idx {
            0 => delegate_response(),
            1 => search_code_response(),
            2 => stop_response("engineer: found it"),
            3 => stop_response("pm: task complete"),
            _ => {
                return Err(LlmError::MissingConfig(format!(
                    "SearchEchoLlmClient script exhausted at call {idx}"
                )));
            }
        };
        serde_json::from_value(fixture).map_err(|e| {
            LlmError::MissingConfig(format!(
                "SearchEchoLlmClient: invalid scripted fixture: {e}"
            ))
        })
    }
}

/// Turn 2 fixture (search variant): the engineer calls `search_code` instead
/// of `bash` — the whole reason [`SearchEchoLlmClient`] exists.
fn search_code_response() -> Value {
    json!({
        "id": "mock-search",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-search",
                    "type": "function",
                    "function": {
                        "name": "search_code",
                        "arguments": json!({"query": "where does auth live", "mode": "semantic"}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}
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

    /// (DOC-39 AC-13) The fan-out script must drive exactly the
    /// two-delegate -> bash A -> stop A -> bash B -> stop B -> pm-stop shape
    /// `tests/agent_id_e2e.rs` relies on.
    #[tokio::test]
    async fn fanout_script_drives_two_delegations_to_the_same_agent() {
        let client = FanoutEchoLlmClient::new();
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
        let calls = turn1.first_tool_calls();
        assert_eq!(calls.len(), 2, "the PM must fan out to TWO delegations");
        assert!(calls.iter().all(|c| c.function.name == "delegate_to_agent"));
        assert!(
            calls
                .iter()
                .all(|c| c.function.arguments.contains("python-engineer")),
            "both delegations must target the SAME agent_name"
        );

        let turn2 = client.chat(&req).await.expect("turn 2 (engineer A bash)");
        assert_eq!(turn2.first_tool_calls()[0].function.name, "bash");

        let turn3 = client.chat(&req).await.expect("turn 3 (engineer A stop)");
        assert!(turn3.first_tool_calls().is_empty());

        let turn4 = client.chat(&req).await.expect("turn 4 (engineer B bash)");
        assert_eq!(turn4.first_tool_calls()[0].function.name, "bash");

        let turn5 = client.chat(&req).await.expect("turn 5 (engineer B stop)");
        assert!(turn5.first_tool_calls().is_empty());

        let turn6 = client.chat(&req).await.expect("turn 6 (pm stop)");
        assert!(turn6.first_tool_calls().is_empty());

        let err = client.chat(&req).await;
        assert!(err.is_err(), "the script must not silently repeat");
    }

    /// (DOC-39 Slice C) The recall script must drive exactly the
    /// recall -> stop shape `tests/recall_content_e2e.rs` relies on.
    #[tokio::test]
    async fn recall_script_drives_a_single_recall_call() {
        let client = RecallEchoLlmClient::new();
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
        let calls = turn1.first_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "recall_session");
        assert!(calls[0].function.arguments.contains("pkce"));

        let turn2 = client.chat(&req).await.expect("turn 2 (pm stop)");
        assert!(turn2.first_tool_calls().is_empty());

        let err = client.chat(&req).await;
        assert!(err.is_err(), "the script must not silently repeat");
    }

    /// (DOC-39 Slice B) The search script must drive exactly the
    /// delegate -> search_code -> stop -> stop shape
    /// `tests/search_hits_e2e.rs` relies on.
    #[tokio::test]
    async fn search_script_drives_full_delegation_flow() {
        let client = SearchEchoLlmClient::new();
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
        assert_eq!(turn2.first_tool_calls()[0].function.name, "search_code");

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
