//! `delegate_to_agent` tool — the PM's primary tool for dispatching to sub-agents.
//!
//! Why: Keeps the delegation schema and its executor colocated so the PM's
//! tool registry can register a single type that owns both. Pre-flight
//! validation of `agent_name` against the on-disk agent config directory
//! prevents the LLM from hallucinating sub-agent names (e.g. inventing
//! `code-searcher` from the `search_code` native tool description, see #204)
//! and crashing the subprocess runner with a confusing IO error.
//! What: `DelegateToAgentTool` wraps an `AgentRunner` and (optionally) an
//! agent config directory. `execute()` parses `{agent_name, task}`, validates
//! the agent TOML exists, and hands off to the runner. On miss, returns a
//! helpful error listing available agents and reminding the LLM that native
//! tools are not agent names.
//! Test: `unknown_agent_returns_helpful_error_with_available_list` builds a
//! tool pointed at a tempdir containing `engineer.toml` only, calls
//! `execute({"agent_name":"code-searcher",...})`, and asserts the error names
//! the unknown agent and lists `engineer`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{AgentRunner, ToolExecutor, ToolResult};

/// Tool executor that delegates a task to a named sub-agent.
pub struct DelegateToAgentTool {
    runner: Arc<dyn AgentRunner>,
    /// Directory holding `<agent>.toml` files. When `Some`, `execute()` will
    /// reject calls whose `agent_name` does not have a matching TOML, with a
    /// helpful "available agents" listing. When `None` (legacy callers /
    /// tests), validation is skipped and the runner is invoked directly.
    config_dir: Option<PathBuf>,
}

impl DelegateToAgentTool {
    /// Construct with an injected `AgentRunner`.
    ///
    /// Why: Lets tests substitute an in-process mock runner without touching
    /// production subprocess code.
    /// What: Stores the `Arc<dyn AgentRunner>` for later dispatch. No
    /// pre-flight name validation is performed unless `with_config_dir` is
    /// also called.
    /// Test: `DelegateToAgentTool::new(Arc::new(MockRunner))` compiles and
    /// yields a tool whose `name()` is `delegate_to_agent`.
    pub fn new(runner: Arc<dyn AgentRunner>) -> Self {
        Self {
            runner,
            config_dir: None,
        }
    }

    /// Attach an agent config directory used for pre-flight `agent_name`
    /// validation.
    ///
    /// Why: When the LLM hallucinates an agent name (e.g. `code-searcher`
    /// from the `search_code` native tool, #204), spawning the subprocess
    /// fails with a generic IO error. Validating up front returns a
    /// structured `ToolResult::err` listing available agents so the LLM can
    /// self-correct on the next turn.
    /// What: Stores `dir`. Files matching `<dir>/<agent_name>.toml` are
    /// considered valid. Missing dir is treated like "no agents available".
    /// Test: `unknown_agent_returns_helpful_error_with_available_list`.
    pub fn with_config_dir(mut self, dir: PathBuf) -> Self {
        self.config_dir = Some(dir);
        self
    }

    /// List the agent names discoverable in `config_dir`, if any.
    ///
    /// Why: Used to build the "available agents" hint in error messages so
    /// the LLM gets immediate, structured feedback when it invents a name.
    /// What: Reads `<config_dir>/*.toml` and returns each file stem. Returns
    /// `None` when no `config_dir` was attached (validation disabled).
    /// Test: Indirect via `unknown_agent_returns_helpful_error_with_available_list`.
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
                "description": "Delegate a task to a specialized sub-agent. Use this for any implementation work (writing code, running analysis, etc.). The sub-agent will be spawned as a subprocess and its result returned to you. NOTE: agent_name must be an actual sub-agent (e.g. 'engineer', 'python-engineer', 'qa-agent'); native tools like search_code, web_search, move_file, create_dir are NOT agent names — call them directly.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "agent_name": {
                            "type": "string",
                            "description": "Short name of the sub-agent (e.g. 'python-engineer'). Must match an existing agent TOML; native tools are not agent names."
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

        // Pre-flight validation (#204): if a config_dir was attached, verify
        // the agent TOML exists before spawning a subprocess. This converts a
        // generic "subprocess failed" IO error into a structured tool error
        // the LLM can act on (with the actual list of valid agent names).
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
                     Note: native tools (search_code, web_search, move_file, create_dir, \
                     memory_store, memory_recall, etc.) are NOT agent names — call them \
                     directly as tools instead of via delegate_to_agent."
                ));
            }
        }

        // Detect coding persona + language idiom skill from the task and
        // agent name. Strips any explicit `[persona]` tag before forwarding so
        // the sub-agent sees a clean task body, then prepends the matching
        // skill bodies as `## Language Conventions` / `## Persona Directive`
        // sections. See `crate::agents::persona` for the detection rules.
        let final_task = crate::agents::persona::assemble_task_with_context(agent_name, task);
        match self.runner.run(agent_name, &final_task).await {
            // PM gets the full content (it may want to inspect code sections).
            Ok(out) => ToolResult::ok(out.content),
            Err(e) => ToolResult::err(format!("sub-agent '{agent_name}' failed: {e:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;

    use super::DelegateToAgentTool;
    use crate::perf::TokenUsage;
    use crate::tools::traits::{AgentOutput, AgentRunner, ToolExecutor};

    /// Mock runner that records whether it was invoked and with what args.
    struct RecordingRunner {
        invoked: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl AgentRunner for RecordingRunner {
        async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
            self.invoked
                .lock()
                .unwrap()
                .push((agent_name.to_string(), task.to_string()));
            Ok(AgentOutput {
                content: "ok".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    /// Asserts that calling `delegate_to_agent` with an unknown agent name
    /// (e.g. the hallucinated `code-searcher` from #204) returns a structured
    /// error listing available agents and clarifying that native tools are
    /// not agent names — and that the runner is NOT invoked.
    #[tokio::test]
    async fn unknown_agent_returns_helpful_error_with_available_list() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed two real agent TOMLs.
        std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname = \"engineer\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("qa-agent.toml"),
            "[agent]\nname = \"qa-agent\"\n",
        )
        .unwrap();

        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        let tool =
            DelegateToAgentTool::new(runner.clone()).with_config_dir(tmp.path().to_path_buf());

        let result = tool
            .execute(json!({
                "agent_name": "code-searcher",
                "task": "find the intent classifier"
            }))
            .await;

        assert!(result.is_error(), "must reject unknown agent");
        let msg = result.content();
        assert!(
            msg.contains("Unknown agent 'code-searcher'"),
            "error must name the unknown agent, got: {msg}"
        );
        assert!(
            msg.contains("engineer") && msg.contains("qa-agent"),
            "error must list available agents, got: {msg}"
        );
        assert!(
            msg.contains("native tools") && msg.contains("search_code"),
            "error must clarify native-vs-agent distinction, got: {msg}"
        );
        // Crucially, the runner must NOT have been invoked — no subprocess spawn.
        assert!(
            runner.invoked.lock().unwrap().is_empty(),
            "runner must not be called when validation fails"
        );
    }

    /// A valid agent name passes validation and reaches the runner.
    #[tokio::test]
    async fn known_agent_reaches_runner() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("engineer.toml"),
            "[agent]\nname = \"engineer\"\n",
        )
        .unwrap();

        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        let tool =
            DelegateToAgentTool::new(runner.clone()).with_config_dir(tmp.path().to_path_buf());

        let result = tool
            .execute(json!({
                "agent_name": "engineer",
                "task": "do the thing"
            }))
            .await;

        assert!(
            !result.is_error(),
            "valid agent should succeed: {}",
            result.content()
        );
        let invoked = runner.invoked.lock().unwrap();
        assert_eq!(invoked.len(), 1, "runner should be called exactly once");
        assert_eq!(invoked[0].0, "engineer");
    }

    /// Without `with_config_dir` (legacy callers), validation is skipped —
    /// the runner is invoked unchanged. This preserves backward compatibility
    /// with `main.rs:1901` and any test double that constructs the tool
    /// without a config dir.
    #[tokio::test]
    async fn no_config_dir_skips_validation() {
        let runner = Arc::new(RecordingRunner {
            invoked: std::sync::Mutex::new(Vec::new()),
        });
        let tool = DelegateToAgentTool::new(runner.clone());

        let result = tool
            .execute(json!({
                "agent_name": "anything-goes",
                "task": "do the thing"
            }))
            .await;

        assert!(!result.is_error(), "legacy mode should bypass validation");
        assert_eq!(runner.invoked.lock().unwrap().len(), 1);
    }
}
