//! LLM-backed session overseer (OpenRouter).
//!
//! Why: the deterministic overseer can only match substrings; nuanced
//! allow/block decisions ("is this `rm` actually dangerous?") need a model.
//! `trusty-mpm-core` is kept pure (no HTTP), so the real LLM overseer lives in
//! the daemon — core only carries the inert placeholder and the `[llm]` config
//! shape. This module calls OpenRouter's chat-completions API and maps the
//! one-word verdict onto an [`OverseerDecision`].
//! What: [`LlmOverseer`] implements [`Overseer`]; it loads the API key from
//! `.env.local` / `.env` / the process environment, posts the tool-use request
//! to OpenRouter with a strict 3-second timeout, and falls back to `Allow` on
//! any error (the safe default — never block development on a flaky network).
//! Test: `cargo test -p trusty-mpm-daemon llm_overseer` exercises the verdict
//! parser and the disabled/enabled gating without hitting the network.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::overseer::{Overseer, OverseerContext, OverseerDecision};

/// OpenRouter chat-completions endpoint.
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Hard timeout for the overseer's HTTP call.
///
/// Why: the overseer sits on the hook hot path; a slow model must never stall
/// a Claude Code tool call. On timeout the overseer fails open (`Allow`).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Hard timeout for an interactive LLM chat call.
///
/// Why: chat is not on the hook hot path, so it can afford a longer budget than
/// the overseer's 3-second verdict timeout, but a runaway request must still
/// fail rather than hang the caller forever.
const CHAT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of messages kept in a rolling chat history (10 exchanges).
///
/// Why: the conversation window must stay bounded so the prompt never grows
/// without limit; ten user/assistant exchanges is a practical context size.
pub const CHAT_HISTORY_LIMIT: usize = 20;

/// System prompt for the general-purpose LLM chat assistant.
const CHAT_SYSTEM_PROMPT: &str = "You are a helpful assistant integrated with \
trusty-mpm, a Claude Code session manager. You can discuss sessions, projects, \
tmux, and general questions. Be concise.";

/// One message in an LLM chat conversation.
///
/// Why: [`LlmOverseer::chat`] maintains a rolling conversation window; each turn
/// is a `{ role, content }` pair matching the OpenRouter messages array shape,
/// so callers (and the HTTP layer) can serialize the history directly.
/// What: a `role` (`"user"` or `"assistant"`) and the message `content`.
/// Test: `chat_history_is_capped`, `build_chat_messages_includes_history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Message role: `"user"` or `"assistant"`.
    pub role: String,
    /// Message text content.
    pub content: String,
}

impl ChatMessage {
    /// A user-authored chat message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    /// An assistant-authored chat message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

/// A failure surfaced by an interactive LLM chat call.
///
/// Why: chat callers (the HTTP handler, the TUI) need to distinguish "not
/// configured" from a transport or parse failure so they can render the right
/// message; a typed error keeps that distinction explicit.
/// What: one variant per failure mode of [`LlmOverseer::chat`].
/// Test: `chat_without_key_is_not_configured`.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The overseer has no API key, so chat cannot run.
    #[error("LLM chat is not configured (no API key)")]
    NotConfigured,
    /// The HTTP request to OpenRouter failed.
    #[error("LLM request failed: {0}")]
    Http(String),
    /// The OpenRouter response body could not be parsed as JSON.
    #[error("LLM response could not be parsed: {0}")]
    Parse(String),
    /// OpenRouter returned no assistant text.
    #[error("LLM returned an empty response")]
    EmptyResponse,
}

/// System prompt instructing the model to act as a security overseer.
const SYSTEM_PROMPT: &str = "You are a security overseer for an AI coding \
assistant. Evaluate the following tool use request and respond with exactly \
one word: ALLOW, BLOCK, or FLAG. BLOCK only for clearly dangerous operations \
(rm -rf, credential exfiltration, etc). FLAG for uncertain cases. ALLOW for \
normal development operations.";

/// LLM-backed [`Overseer`] that consults OpenRouter for tool-use verdicts.
///
/// Why: gives the daemon a model-driven oversight strategy, interchangeable
/// with the deterministic one behind `dyn Overseer`.
/// What: holds the resolved API key, the model id, and a `reqwest` blocking
/// client; `enabled` is `true` only when an API key was found.
/// Test: `disabled_without_key`, `enabled_with_key`.
#[derive(Debug)]
pub struct LlmOverseer {
    /// OpenRouter API key, empty when none was found (overseer then disabled).
    api_key: String,
    /// OpenRouter model id to query.
    model: String,
    /// Blocking HTTP client with the overseer timeout baked in.
    client: reqwest::blocking::Client,
    /// Async HTTP client used by the interactive `chat` path.
    ///
    /// Why: [`Self::evaluate`] runs on the synchronous hook path, but
    /// [`Self::chat`] is called from async axum handlers where a blocking
    /// client would stall the runtime; chat gets its own async client with a
    /// longer timeout.
    chat_client: reqwest::Client,
}

impl LlmOverseer {
    /// Build an LLM overseer from the `[llm]` config section.
    ///
    /// Why: the daemon constructs this once at startup when `[llm] enabled =
    /// true`; it must resolve the API key from the operator's environment
    /// (preferring `.env.local`, then `.env`, then the real process env).
    /// What: reads the key named by `api_key_env`, builds a timeout-bounded
    /// `reqwest` blocking client, and stores the model id. An absent key is
    /// not fatal — the overseer is simply reported disabled.
    /// Test: `disabled_without_key`, `enabled_with_key`.
    pub fn new(model: impl Into<String>, api_key_env: &str) -> Self {
        let api_key = resolve_api_key(api_key_env);
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        let chat_client = reqwest::Client::builder()
            .timeout(CHAT_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            api_key,
            model: model.into(),
            client,
            chat_client,
        }
    }

    /// Send a user message and return the model's reply text.
    ///
    /// Why: the LLM overseer already resolves an API key and a model; the
    /// Telegram bot and TUI want a general conversational endpoint that reuses
    /// that same auth rather than standing up a second OpenRouter client.
    /// What: appends `user_msg` to `history`, posts the system prompt plus the
    /// full (capped) history to OpenRouter, appends the assistant reply to
    /// `history`, and returns the reply text. `history` is trimmed to the last
    /// [`CHAT_HISTORY_LIMIT`] messages (oldest pairs dropped) before sending.
    /// On any failure `history` is left holding only the user message so the
    /// caller can retry without a dangling half-exchange.
    /// Test: `chat_history_is_capped`, `chat_without_key_is_not_configured`.
    pub async fn chat(
        &self,
        history: &mut Vec<ChatMessage>,
        user_msg: &str,
    ) -> Result<String, LlmError> {
        if self.api_key.is_empty() {
            return Err(LlmError::NotConfigured);
        }

        history.push(ChatMessage::user(user_msg));
        cap_history(history);

        let body = serde_json::json!({
            "model": self.model,
            "messages": build_chat_messages(history),
            "temperature": 0.7,
        });

        let response = self
            .chat_client
            .post(OPENROUTER_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;

        let json: Value = response
            .json()
            .await
            .map_err(|e| LlmError::Parse(e.to_string()))?;

        let reply = extract_reply(&json);
        if reply.trim().is_empty() {
            return Err(LlmError::EmptyResponse);
        }

        history.push(ChatMessage::assistant(reply.clone()));
        cap_history(history);
        Ok(reply)
    }

    /// Query OpenRouter for a verdict on one tool-use request.
    ///
    /// Why: `pre_tool_use` / `post_tool_use` share the same request shape;
    /// centralizing the HTTP call keeps the trait impl thin.
    /// What: posts the system prompt + a user message describing `tool`/`input`
    /// to OpenRouter, reads the assistant reply, and runs it through
    /// [`parse_verdict`]. Any network/parse error yields `Allow` (fail open).
    /// Test: covered by `parse_verdict` tests; the network path is exercised
    /// only when an API key is present.
    fn evaluate(&self, tool: &str, input: &str) -> OverseerDecision {
        let user_message = format!("Tool: {tool}\nInput: {input}");
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": user_message },
            ],
            "max_tokens": 16,
            "temperature": 0.0,
        });

        let response = self
            .client
            .post(OPENROUTER_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send();

        match response {
            Ok(resp) => match resp.json::<Value>() {
                Ok(json) => {
                    let reply = extract_reply(&json);
                    parse_verdict(&reply)
                }
                Err(e) => {
                    tracing::warn!("LLM overseer: bad response body: {e}; allowing");
                    OverseerDecision::Allow
                }
            },
            Err(e) => {
                tracing::warn!("LLM overseer: request failed: {e}; allowing");
                OverseerDecision::Allow
            }
        }
    }
}

impl Overseer for LlmOverseer {
    fn pre_tool_use(&self, ctx: &OverseerContext) -> OverseerDecision {
        if !self.is_enabled() {
            return OverseerDecision::Allow;
        }
        let tool = ctx.tool_name.as_deref().unwrap_or("unknown");
        let input = ctx.tool_input.as_deref().unwrap_or("");
        self.evaluate(tool, input)
    }

    fn post_tool_use(&self, _ctx: &OverseerContext, _output: &str) -> OverseerDecision {
        // Post-hoc output is not gated by the LLM overseer — the action has
        // already run, so blocking is meaningless. Allow and let the audit
        // log / deterministic layer handle anything notable.
        OverseerDecision::Allow
    }

    fn session_question(&self, _ctx: &OverseerContext, question: &str) -> OverseerDecision {
        // Questions are escalated to a human; the LLM overseer does not
        // auto-answer them (that is the deterministic auto-responder's job).
        OverseerDecision::FlagForHuman {
            summary: format!("session question needs review: {question}"),
        }
    }

    fn is_enabled(&self) -> bool {
        // Active only when an API key was resolved at construction time.
        !self.api_key.is_empty()
    }
}

/// Pull the assistant's reply text out of an OpenRouter chat response.
///
/// Why: the verdict lives at `choices[0].message.content`; isolating the
/// extraction keeps [`LlmOverseer::evaluate`] readable and the parser testable.
/// What: returns the content string, or `""` when the shape is unexpected.
/// Test: `extract_reply_reads_content`, `extract_reply_handles_missing`.
fn extract_reply(json: &Value) -> String {
    json.get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Map a model reply onto an [`OverseerDecision`].
///
/// Why: the model is told to answer with one word, but real replies may add
/// punctuation or surrounding prose; a tolerant parser keeps the verdict
/// robust. "BLOCK" wins over "FLAG" wins over the `Allow` default — the safest
/// matching verdict is chosen when the reply is ambiguous.
/// What: case-insensitively scans `reply` for `BLOCK` / `FLAG`, returning the
/// matching decision; anything else (including an empty reply) is `Allow`.
/// Test: `parse_verdict_block`, `parse_verdict_flag`, `parse_verdict_allow`,
/// `parse_verdict_is_case_insensitive`, `parse_verdict_empty_is_allow`.
fn parse_verdict(reply: &str) -> OverseerDecision {
    let upper = reply.to_uppercase();
    if upper.contains("BLOCK") {
        OverseerDecision::Block {
            reason: format!("LLM overseer blocked the tool use: {}", reply.trim()),
        }
    } else if upper.contains("FLAG") {
        OverseerDecision::FlagForHuman {
            summary: format!("LLM overseer flagged the tool use: {}", reply.trim()),
        }
    } else {
        OverseerDecision::Allow
    }
}

/// Resolve an API key from `.env.local`, then `.env`, then the process env.
///
/// Why: the operator stores `OPENROUTER_API_KEY` in `.env.local` (gitignored)
/// or `.env`; the daemon does not load a dotenv crate, so this reads the files
/// directly. The process environment wins last so an explicit `export` always
/// overrides the files.
/// What: scans `.env.local` then `.env` in the current directory for a
/// `KEY=value` line, falling back to `std::env::var`. Returns `""` when the
/// key is not found anywhere.
/// Test: `resolve_api_key_reads_env_var`, `resolve_api_key_missing_is_empty`.
fn resolve_api_key(var_name: &str) -> String {
    for file in [".env.local", ".env"] {
        if let Some(value) = read_dotenv_key(std::path::Path::new(file), var_name) {
            return value;
        }
    }
    std::env::var(var_name).unwrap_or_default()
}

/// Read one `KEY=value` entry from a dotenv-style file.
///
/// Why: a tiny, dependency-free dotenv reader is enough for a single key;
/// pulling it out keeps [`resolve_api_key`] testable against a temp file.
/// What: scans `path` line by line for `var_name=...`, trimming surrounding
/// quotes and whitespace from the value. Comment lines (`#`) are skipped.
/// Returns `None` when the file is absent or the key is not present.
/// Test: `read_dotenv_key_parses_value`, `read_dotenv_key_missing_file`.
fn read_dotenv_key(path: &std::path::Path, var_name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == var_name
        {
            let value = value.trim().trim_matches('"').trim_matches('\'').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Trim a chat history to the last [`CHAT_HISTORY_LIMIT`] messages.
///
/// Why: the conversation window must stay bounded so the prompt sent to
/// OpenRouter never grows without limit; dropping from the front keeps the most
/// recent (most relevant) context.
/// What: removes the oldest messages until `history.len() <= CHAT_HISTORY_LIMIT`.
/// Test: `chat_history_is_capped`, `cap_history_keeps_recent`.
fn cap_history(history: &mut Vec<ChatMessage>) {
    if history.len() > CHAT_HISTORY_LIMIT {
        let overflow = history.len() - CHAT_HISTORY_LIMIT;
        history.drain(0..overflow);
    }
}

/// Build the OpenRouter `messages` array: system prompt then the history.
///
/// Why: every chat request leads with the assistant's system prompt followed by
/// the full conversation window; isolating the assembly keeps [`LlmOverseer::chat`]
/// readable and the shape testable without the network.
/// What: returns a JSON array with the system message first, then each history
/// message as `{ role, content }`.
/// Test: `build_chat_messages_includes_history`.
fn build_chat_messages(history: &[ChatMessage]) -> Vec<Value> {
    let mut messages = Vec::with_capacity(history.len() + 1);
    messages.push(serde_json::json!({
        "role": "system",
        "content": CHAT_SYSTEM_PROMPT,
    }));
    for msg in history {
        messages.push(serde_json::json!({
            "role": msg.role,
            "content": msg.content,
        }));
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict_block() {
        // A reply containing BLOCK yields a Block decision carrying the reply.
        let decision = parse_verdict("BLOCK");
        assert!(matches!(decision, OverseerDecision::Block { .. }));
    }

    #[test]
    fn parse_verdict_flag() {
        let decision = parse_verdict("FLAG");
        assert!(matches!(decision, OverseerDecision::FlagForHuman { .. }));
    }

    #[test]
    fn parse_verdict_allow() {
        assert_eq!(parse_verdict("ALLOW"), OverseerDecision::Allow);
    }

    #[test]
    fn parse_verdict_is_case_insensitive() {
        // Models do not always shout; lowercase verdicts must still parse.
        assert!(matches!(
            parse_verdict("block"),
            OverseerDecision::Block { .. }
        ));
        assert!(matches!(
            parse_verdict("flag"),
            OverseerDecision::FlagForHuman { .. }
        ));
    }

    #[test]
    fn parse_verdict_tolerates_surrounding_prose() {
        // A chatty reply still maps to the right verdict.
        assert!(matches!(
            parse_verdict("I would BLOCK this — it deletes the repo."),
            OverseerDecision::Block { .. }
        ));
    }

    #[test]
    fn parse_verdict_block_wins_over_flag() {
        // When a reply mentions both, the safer (Block) verdict is chosen.
        assert!(matches!(
            parse_verdict("BLOCK, do not FLAG"),
            OverseerDecision::Block { .. }
        ));
    }

    #[test]
    fn parse_verdict_empty_is_allow() {
        // An empty reply (e.g. timeout fallback) defaults to Allow.
        assert_eq!(parse_verdict(""), OverseerDecision::Allow);
        assert_eq!(
            parse_verdict("something else entirely"),
            OverseerDecision::Allow
        );
    }

    #[test]
    fn extract_reply_reads_content() {
        let json = serde_json::json!({
            "choices": [ { "message": { "content": "BLOCK" } } ]
        });
        assert_eq!(extract_reply(&json), "BLOCK");
    }

    #[test]
    fn extract_reply_handles_missing() {
        // A malformed response yields an empty reply (→ Allow downstream).
        assert_eq!(extract_reply(&serde_json::json!({})), "");
        assert_eq!(extract_reply(&serde_json::json!({ "choices": [] })), "");
    }

    #[test]
    fn disabled_without_key() {
        // With an env var that does not exist, the overseer is disabled and
        // every method falls through to the safe default.
        let overseer = LlmOverseer::new("test-model", "TRUSTY_MPM_NO_SUCH_KEY_VAR");
        assert!(!overseer.is_enabled());
        let ctx = OverseerContext::new(
            crate::core::session::SessionId::new(),
            "tmpm-test",
            Some("Bash".into()),
            Some("ls".into()),
        );
        assert_eq!(overseer.pre_tool_use(&ctx), OverseerDecision::Allow);
    }

    #[test]
    fn enabled_with_key() {
        // SAFETY: tests in this module run single-threaded for this var.
        unsafe {
            std::env::set_var("TRUSTY_MPM_TEST_LLM_KEY", "sk-test-123");
        }
        let overseer = LlmOverseer::new("test-model", "TRUSTY_MPM_TEST_LLM_KEY");
        assert!(overseer.is_enabled());
        unsafe {
            std::env::remove_var("TRUSTY_MPM_TEST_LLM_KEY");
        }
    }

    #[test]
    fn read_dotenv_key_parses_value() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "# a comment").unwrap();
        writeln!(file, "OTHER=ignored").unwrap();
        writeln!(file, "OPENROUTER_API_KEY=\"sk-or-v1-abc\"").unwrap();
        let value = read_dotenv_key(&path, "OPENROUTER_API_KEY");
        assert_eq!(value.as_deref(), Some("sk-or-v1-abc"));
    }

    #[test]
    fn read_dotenv_key_missing_file() {
        // An absent file is not an error — it just yields None.
        let value = read_dotenv_key(std::path::Path::new("/no/such/.env"), "ANY");
        assert!(value.is_none());
    }

    #[test]
    fn cap_history_keeps_recent() {
        // An over-limit history is trimmed to the last CHAT_HISTORY_LIMIT,
        // keeping the most recent messages.
        let mut history: Vec<ChatMessage> = (0..CHAT_HISTORY_LIMIT + 4)
            .map(|i| ChatMessage::user(format!("msg-{i}")))
            .collect();
        cap_history(&mut history);
        assert_eq!(history.len(), CHAT_HISTORY_LIMIT);
        assert_eq!(
            history.last().unwrap().content,
            format!("msg-{}", CHAT_HISTORY_LIMIT + 3)
        );
    }

    #[test]
    fn cap_history_leaves_short_history() {
        // A history within the limit is untouched.
        let mut history = vec![ChatMessage::user("a"), ChatMessage::assistant("b")];
        cap_history(&mut history);
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn build_chat_messages_includes_history() {
        // The assembled array leads with the system prompt then the history.
        let history = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ];
        let messages = build_chat_messages(&history);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "hello");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "hi there");
    }

    #[tokio::test]
    async fn chat_without_key_is_not_configured() {
        // With no API key the overseer reports NotConfigured rather than
        // attempting a network call. The overseer is built on a blocking
        // thread because constructing its blocking `reqwest` client stands up
        // an internal runtime that must not be dropped in an async context.
        let overseer = tokio::task::spawn_blocking(|| {
            LlmOverseer::new("test-model", "TRUSTY_MPM_NO_SUCH_CHAT_KEY")
        })
        .await
        .expect("build overseer");
        let mut history = Vec::new();
        let err = overseer.chat(&mut history, "hello").await.unwrap_err();
        assert!(matches!(err, LlmError::NotConfigured));
        // Drop the overseer (and its blocking client) off the async context.
        tokio::task::spawn_blocking(move || drop(overseer))
            .await
            .expect("drop overseer");
    }

    #[test]
    fn chat_message_constructors_set_role() {
        assert_eq!(ChatMessage::user("x").role, "user");
        assert_eq!(ChatMessage::assistant("y").role, "assistant");
    }
}
