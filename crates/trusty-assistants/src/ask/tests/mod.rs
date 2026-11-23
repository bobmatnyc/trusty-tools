//! Tests for the ask engine (#11650).
//!
//! Why: every test runs offline — a scripted `InferenceAdapter`, a temp
//! `servers.toml` through `McpToolSet::load_at`, and the crate's process-start
//! HOME sandbox. No MCP server is spawned and no network call is made.
//! What: the shared fixtures; the tests live in the submodules.
//! Test: this module IS the test surface.

mod config_tests;
mod engine_tests;
mod tools_tests;
mod turn_log_tests;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use trusty_code::llm::{
    AssistantMessage, ChatChoice, ChatRequest, ChatResponse, FunctionCall, InferenceAdapter,
    InferenceError, ToolCall,
};
use trusty_common::inference::{ProviderCapabilities, ProviderId, capabilities};

use crate::assistants::{AssistantHome, AssistantInstanceId};

/// An adapter that replays queued replies and records every request.
pub(super) struct Scripted {
    replies: Mutex<VecDeque<ChatResponse>>,
    seen: Mutex<Vec<ChatRequest>>,
    on_call: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Scripted {
    pub(super) fn new(replies: Vec<ChatResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            on_call: None,
        }
    }

    /// Run `hook` inside every `chat` call, after the request is recorded.
    pub(super) fn on_call(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_call = Some(Box::new(hook));
        self
    }

    pub(super) fn requests(&self) -> Vec<ChatRequest> {
        self.seen.lock().expect("lock").clone()
    }
}

#[async_trait]
impl InferenceAdapter for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        capabilities(ProviderId::OpenRouter)
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        self.seen.lock().expect("lock").push(request.clone());
        if let Some(hook) = &self.on_call {
            hook();
        }
        self.replies
            .lock()
            .expect("lock")
            .pop_front()
            .ok_or_else(|| InferenceError::Provider("script exhausted".into()))
    }
}

fn response(message: AssistantMessage, finish: &str) -> ChatResponse {
    ChatResponse {
        id: "scripted".into(),
        model: String::new(),
        choices: vec![ChatChoice {
            message,
            finish_reason: Some(finish.into()),
        }],
        usage: Default::default(),
    }
}

/// A final text reply.
pub(super) fn text(body: &str) -> ChatResponse {
    response(
        AssistantMessage {
            content: Some(body.into()),
            tool_calls: vec![],
        },
        "stop",
    )
}

/// A reply calling `name` with `args`.
pub(super) fn tool_call(name: &str, args: serde_json::Value) -> ChatResponse {
    response(
        AssistantMessage {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: args.to_string(),
                },
            }],
        },
        "tool_calls",
    )
}

/// A provisioned-looking home for `izzie` with instructions, under a temp root.
pub(super) fn instance(tmp: &tempfile::TempDir) -> AssistantHome {
    let id = AssistantInstanceId::new("izzie").expect("valid id");
    let home = AssistantHome::under(tmp.path().join("trusty-agents"), id);
    home.ensure().expect("ensure");
    std::fs::write(
        home.instructions_path(),
        "You are Izzie, the owner's assistant. INSTRUCTIONS-MARKER.\n",
    )
    .expect("write instructions");
    home
}

/// A temp `servers.toml` declaring no servers.
pub(super) fn empty_servers(tmp: &tempfile::TempDir) -> PathBuf {
    let path = tmp.path().join("servers.toml");
    std::fs::write(&path, "").expect("write servers.toml");
    path
}

/// The name of every tool in a registry, sorted.
pub(super) fn tool_names(registry: &trusty_code::tools::ToolRegistry) -> Vec<String> {
    let mut names: Vec<String> = registry
        .schemas()
        .iter()
        .filter_map(|s| s["function"]["name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    names
}
