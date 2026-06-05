//! `delegate_to_agent` tool — the PM's primary tool for dispatching to sub-agents.
//!
//! Why: Keeps the delegation schema and its executor colocated so the PM's tool
//! registry can register a single type that owns both. Pre-flight validation of
//! `agent_name` against the on-disk agent config directory prevents the LLM from
//! hallucinating sub-agent names and crashing the subprocess runner with a
//! confusing IO error (#204 equivalent).
//! What: `DelegateToAgentTool` wraps an `AgentRunner` and (optionally) an agent
//! config directory. `execute()` parses `{agent_name, task}`, validates the agent
//! config exists, and hands off to the runner. On miss, returns a helpful error
//! listing available agents.
//! Test: `unknown_agent_returns_helpful_error` builds a tool pointed at a tempdir
//! containing `engineer.toml` only, calls `execute({"agent_name":"ghost",...})`,
//! and asserts the error names the unknown agent and lists `engineer`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{AgentRunner, ToolExecutor, ToolResult};

/// Tool executor that delegates a task to a named sub-agent.
///
/// Why: Encapsulates the `delegate_to_agent` tool so the PM loop registers a
/// single `Arc<dyn ToolExecutor>` rather than branching on the tool name inline.
/// What: Holds an `AgentRunner` for subprocess/in-process dispatch and an
/// optional `config_dir` for pre-flight agent name validation.
/// Test: `unknown_agent_returns_helpful_error`, `known_agent_reaches_runner`,
/// `no_config_dir_skips_validation`.
pub struct DelegateToAgentTool {
    runner: Arc<dyn AgentRunner>,
    /// Directory holding `<agent>.toml` files. When `Some`, `execute()` rejects
    /// calls whose `agent_name` does not have a matching TOML. When `None`,
    /// validation is skipped (legacy / test mode).
    config_dir: Option<PathBuf>,
}

impl DelegateToAgentTool {
    /// Construct with an injected `AgentRunner`.
    ///
    /// Why: Lets tests substitute an in-process mock runner without touching
    /// production subprocess code.
    /// What: Stores `runner`; no pre-flight name validation until `with_config_dir`
    /// is also called.
    /// Test: `DelegateToAgentTool::new(Arc::new(MockRunner))` compiles and
    /// yields a tool whose `name()` is `delegate_to_agent`.
    pub fn new(runner: Arc<dyn AgentRunner>) -> Self {
        Self {
            runner,
            config_dir: None,
        }
    }

    /// Attach an agent config directory for pre-flight `agent_name` validation.
    ///
    /// Why: When the LLM hallucinates an agent name, spawning the subprocess
    /// fails with a generic IO error. Validating up front returns a structured
    /// `ToolResult::err` listing available agents so the LLM can self-correct.
    /// What: Stores `dir`. Files matching `<dir>/<agent_name>.toml` are
    /// considered valid. Missing dir is treated as "no agents available".
    /// Test: `unknown_agent_returns_helpful_error`.
    pub fn with_config_dir(mut self, dir: PathBuf) -> Self {
        self.config_dir = Some(dir);
        self
    }

    /// List agent names discoverable in `config_dir`, if any.
    ///
    /// Why: Builds the "available agents" hint in error messages so the LLM
    /// gets immediate, structured feedback when it invents a name.
    /// What: Reads `<config_dir>/*.toml` and returns each file stem. Returns
    /// `None` when no `config_dir` was attached.
    /// Test: Indirect via `unknown_agent_returns_helpful_error`.
    fn available_agents(&self) -> Option<Vec<String>> {
        let dir = self.config_dir.as_ref()?;
        let entries = std::fs::read_dir(dir).ok()?;
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();
        names.sort();
        Some(names)
    }
}

#[async_trait]
impl ToolExecutor for DelegateToAgentTool {
    fn name(&self) -> &str {
        "delegate_to_agent"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "delegate_to_agent",
                "description": "Delegate a task to a specialized sub-agent. Use this for any implementation work (writing code, running analysis, etc.). The sub-agent will be spawned and its result returned to you. NOTE: agent_name must be an actual sub-agent (e.g. 'engineer', 'python-engineer', 'qa-agent'); native tools like search_code, web_search are NOT agent names — call them directly.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "agent_name": {
                            "type": "string",
                            "description": "Short name of the sub-agent. Must match an existing agent config; native tools are not agent names."
                        },
                        "task": {
                            "type": "string",
                            "description": "Concrete task description for the sub-agent."
                        }
                    },
                    "required": ["agent_name", "task"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(agent_name) = args.get("agent_name").and_then(Value::as_str) else {
            return ToolResult::err("delegate_to_agent: missing 'agent_name'");
        };
        let Some(task) = args.get("task").and_then(Value::as_str) else {
            return ToolResult::err("delegate_to_agent: missing 'task'");
        };

        // Guard against path traversal: the LLM supplies agent_name which is
        // joined into a filesystem path. Reject any name that is not strictly
        // [a-zA-Z0-9_-] before the path join so a crafted value like
        // "../../etc/passwd" cannot escape the agents config directory.
        if agent_name.is_empty()
            || !agent_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return ToolResult::err(format!(
                "Invalid agent name '{agent_name}': \
                 agent names must be non-empty and contain only [a-zA-Z0-9_-]"
            ));
        }

        // Pre-flight validation: if a config_dir was attached, verify the agent
        // config exists before spawning. Converts a generic IO error into a
        // structured tool error the LLM can act on.
        if let Some(dir) = &self.config_dir {
            let agent_toml = dir.join(format!("{agent_name}.toml"));
            if !agent_toml.exists() {
                let available = self.available_agents().unwrap_or_default();
                let available_str = if available.is_empty() {
                    "(none discovered)".to_string()
                } else {
                    available.join(", ")
                };
                return ToolResult::err(format!(
                    "Unknown agent '{agent_name}'. Available agents: {available_str}. \
                     Note: native tools (search_code, web_search, etc.) are NOT agent \
                     names — call them directly as tools instead of via delegate_to_agent."
                ));
            }
        }

        match self.runner.run(agent_name, task).await {
            Ok(out) => ToolResult::ok(out.content),
            Err(e) => ToolResult::err(format!("sub-agent '{agent_name}' failed: {e:#}")),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;

    use super::DelegateToAgentTool;
    use crate::tools::traits::{AgentOutput, AgentRunner, ToolExecutor};

    /// Recording mock runner.
    ///
    /// Why: Tests need to verify the runner was (or was not) invoked.
    /// What: Records `(agent_name, task)` pairs in a Mutex-guarded Vec.
    /// Test: `known_agent_reaches_runner`, `no_config_dir_skips_validation`.
    struct RecordingRunner {
        invoked: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl AgentRunner for RecordingRunner {
        async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
            self.invoked
                .lock()
                .expect("lock poisoned")
                .push((agent_name.to_string(), task.to_string()));
            Ok(AgentOutput::from_content("ok"))
        }
    }

    /// An unknown agent name returns a structured error listing available agents,
    /// and the runner is NOT invoked.
    ///
    /// Why: Guards against hallucinated agent names causing confusing IO errors.
    /// What: Calls `execute({"agent_name":"ghost",...})` with a tempdir
    /// containing only `engineer.toml`; expects error with "ghost" and "engineer".
    /// Test: This test.
    #[tokio::test]
    async fn unknown_agent_returns_helpful_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname = \"engineer\"\n",
        )
        .expect("write");
        std::fs::write(
            tmp.path().join("qa-agent.toml"),
            "[agent]\nname = \"qa-agent\"\n",
        )
        .expect("write");

        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        let tool =
            DelegateToAgentTool::new(runner.clone()).with_config_dir(tmp.path().to_path_buf());

        let result = tool
            .execute(json!({"agent_name": "ghost", "task": "do something"}))
            .await;

        assert!(result.is_error(), "must reject unknown agent");
        let msg = result.content();
        assert!(
            msg.contains("Unknown agent 'ghost'"),
            "error must name the unknown agent, got: {msg}"
        );
        assert!(
            msg.contains("engineer") && msg.contains("qa-agent"),
            "error must list available agents, got: {msg}"
        );
        assert!(
            msg.contains("native tools"),
            "error must clarify native-vs-agent, got: {msg}"
        );
        assert!(
            runner.invoked.lock().expect("lock").is_empty(),
            "runner must not be called when validation fails"
        );
    }

    /// A valid agent name passes validation and reaches the runner.
    ///
    /// Why: Verify the happy-path dispatch contract.
    /// What: Register `engineer.toml`, dispatch with `agent_name="engineer"`.
    /// Test: This test.
    #[tokio::test]
    async fn known_agent_reaches_runner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname = \"engineer\"\n",
        )
        .expect("write");

        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        let tool =
            DelegateToAgentTool::new(runner.clone()).with_config_dir(tmp.path().to_path_buf());

        let result = tool
            .execute(json!({"agent_name": "engineer", "task": "do the thing"}))
            .await;

        assert!(
            !result.is_error(),
            "valid agent should succeed: {}",
            result.content()
        );
        let invoked = runner.invoked.lock().expect("lock");
        assert_eq!(invoked.len(), 1, "runner should be called exactly once");
        assert_eq!(invoked[0].0, "engineer");
    }

    /// Without `with_config_dir`, validation is skipped — the runner is invoked.
    ///
    /// Why: Preserves backward compatibility with callers that don't need
    /// pre-flight validation (e.g. integration tests).
    /// What: Construct tool without config_dir; any agent name reaches the runner.
    /// Test: This test.
    #[tokio::test]
    async fn no_config_dir_skips_validation() {
        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        let tool = DelegateToAgentTool::new(runner.clone());

        let result = tool
            .execute(json!({"agent_name": "anything-goes", "task": "do it"}))
            .await;

        assert!(!result.is_error(), "legacy mode should bypass validation");
        assert_eq!(runner.invoked.lock().expect("lock").len(), 1);
    }

    /// Missing `agent_name` field returns a structured error.
    ///
    /// Why: Guard against malformed LLM function call arguments.
    /// What: `execute({})` without `agent_name` key.
    /// Test: This test.
    #[tokio::test]
    async fn missing_agent_name_returns_error() {
        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        let tool = DelegateToAgentTool::new(runner);
        let result = tool.execute(json!({"task": "something"})).await;
        assert!(result.is_error());
        assert!(result.content().contains("missing 'agent_name'"));
    }

    /// A path-traversal agent name is rejected before any filesystem access.
    ///
    /// Why: The LLM supplies `agent_name` which is joined into a filesystem path.
    /// A crafted name like `../../etc/passwd` must be caught before the join to
    /// prevent escaping the agents config directory.
    /// What: Calls `execute` with a traversal string; asserts error and runner
    /// not invoked.
    /// Test: This test.
    #[tokio::test]
    async fn path_traversal_agent_name_is_rejected() {
        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        // No config_dir — traversal guard fires before config check.
        let tool = DelegateToAgentTool::new(runner.clone());

        for bad_name in &[
            "../../etc/passwd",
            "../sibling",
            "agent/sub",
            "agent name",
            "",
        ] {
            let result = tool
                .execute(json!({"agent_name": bad_name, "task": "do it"}))
                .await;
            assert!(
                result.is_error(),
                "traversal/invalid name '{bad_name}' must be rejected"
            );
            assert!(
                result.content().contains("Invalid agent name"),
                "error must describe the problem, got: {}",
                result.content()
            );
        }
        assert!(
            runner.invoked.lock().expect("lock").is_empty(),
            "runner must never be called for invalid names"
        );
    }

    /// A valid agent name passes the sanitisation guard and reaches the runner.
    ///
    /// Why: Confirms the allowlist does not over-reject legitimate names.
    /// What: `execute({"agent_name":"engineer",...})` without config_dir; runner
    /// is called once.
    /// Test: This test.
    #[tokio::test]
    async fn valid_agent_name_passes_sanitization() {
        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        let tool = DelegateToAgentTool::new(runner.clone());

        for good_name in &["engineer", "qa-agent", "python_engineer", "rust-2024"] {
            let result = tool
                .execute(json!({"agent_name": good_name, "task": "do it"}))
                .await;
            assert!(
                !result.is_error(),
                "valid name '{good_name}' must be accepted: {}",
                result.content()
            );
        }
        assert_eq!(
            runner.invoked.lock().expect("lock").len(),
            4,
            "runner must be called for each valid name"
        );
    }
}
