//! `delegate_to_agent` tool — dispatches a task to a named specialist agent,
//! shared by every caller: `pm`, `ctrl`, and (since #3052 PR A) the
//! black-boxed assistant-tier personas.
//!
//! Why: Keeps the delegation schema and its executor colocated so every
//! caller's tool registry can register a single type that owns both.
//! Pre-flight validation of `agent_name` against the on-disk agent config
//! directory prevents the LLM from hallucinating a specialist name (e.g.
//! inventing `code-searcher` from the `search_code` native tool description,
//! see #204) and crashing the subprocess runner with a confusing IO error.
//! What: `DelegateToAgentTool` wraps an `AgentRunner` and (optionally) an
//! agent config directory. `execute()` parses `{agent_name, task}`, validates
//! the agent TOML exists, and hands off to the runner. On miss, returns a
//! GENERIC error (#3052 PR A code-critic CRITICAL-2: no on-disk roster
//! enumeration — the caller's own instructions, not this shared tool, are
//! the source of truth for which specialist names are legitimate, and this
//! tool's output must stay safe to surface from a black-boxed persona). The
//! tool's schema `description` is likewise generic (CRITICAL-1: no hardcoded
//! internal agent-name examples) — `pm` still knows its roster via its own
//! system prompt's `{{available_agents}}` template substitution
//! (`agents::registry::roster::inject_roster_into_prompt`), which is
//! independent of this schema.
//! Test: `unknown_agent_is_rejected_without_naming_the_agent_or_roster`
//! asserts the error names the REJECTED agent but leaks no on-disk roster
//! entry and no bundled agent filename/internal system name.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{AgentRunner, ToolExecutor, ToolResult};

/// Tool executor that delegates a task to a named specialist agent.
pub struct DelegateToAgentTool {
    runner: Arc<dyn AgentRunner>,
    /// Directory holding `<agent>.toml` files. When `Some`, `execute()` will
    /// reject calls whose `agent_name` does not have a matching TOML, with a
    /// generic error (#3052 PR A: no on-disk roster enumeration — see the
    /// module docs). When `None` (legacy callers / tests), validation is
    /// skipped and the runner is invoked directly.
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
    /// structured, GENERIC `ToolResult::err` so the LLM can self-correct on
    /// the next turn by re-checking its OWN instructions for the specialists
    /// legitimately available to it — this tool does not enumerate the
    /// on-disk roster (#3052 PR A code-critic CRITICAL-2).
    /// What: Stores `dir`. Files matching `<dir>/<agent_name>.toml` are
    /// considered valid. Missing dir is treated like "no agents available".
    /// Test: `unknown_agent_is_rejected_without_naming_the_agent_or_roster`.
    pub fn with_config_dir(mut self, dir: PathBuf) -> Self {
        self.config_dir = Some(dir);
        self
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
                "description": "Hand a task off to a specialist so it actually gets done. Use this for any implementation work (writing code, running analysis, etc.) rather than doing it yourself. The specialist runs independently and its result is returned to you. NOTE: agent_name must identify an actual specialist you know about (see your own instructions for which ones are available to you) — native tools like search_code, web_search, move_file, create_dir are NOT specialist names — call them directly instead.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "agent_name": {
                            "type": "string",
                            "description": "The specialist to hand this task to, by its internal name. Must be one of the specialists available to you (see your own instructions); native tools are not specialist names."
                        },
                        "task": {
                            "type": "string",
                            "description": "Concrete task description for the specialist."
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
        // the LLM can act on.
        //
        // #3052 PR A code-critic CRITICAL-2: the error message MUST NOT
        // enumerate the on-disk agent roster — this tool is now reachable
        // from black-boxed assistant-tier personas, and `delegate.rs` is
        // shared by every caller (pm, ctrl, assistant, ...), so the message
        // is sent straight back into whichever persona's turn is in
        // progress. A generic "not recognized" response is enough for the
        // LLM to self-correct (re-check its own instructions for the
        // specialists actually available to it) without the tool itself
        // dumping internal agent IDs/filenames into a black-boxed
        // conversation. `available_agents()` is kept (used by tests only
        // now) rather than deleted, since a future caller-gated surface
        // (e.g. an internal-only diagnostic) may still want it.
        if let Some(dir) = &self.config_dir {
            let agent_toml = dir.join(format!("{agent_name}.toml"));
            if !agent_toml.exists() {
                return ToolResult::err(format!(
                    "'{agent_name}' is not a recognized specialist. Check your own \
                     instructions for the specialists available to you — native tools \
                     (search_code, web_search, move_file, create_dir, memory_store, \
                     memory_recall, etc.) are NOT specialist names; call them directly \
                     as tools instead of via delegate_to_agent."
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

    /// #3052 PR A code-critic CRITICAL-2 regression guard: calling
    /// `delegate_to_agent` with an unknown agent name (e.g. the hallucinated
    /// `code-searcher` from #204) must return a GENERIC error — it must
    /// echo back the caller's OWN rejected input, but it must NOT enumerate
    /// the on-disk agent roster, name any bundled agent filename, or leak
    /// any internal trusty-* system/daemon name. This tool is reachable
    /// from black-boxed assistant-tier personas (`assistant`, `izzie`,
    /// `cto-assistant`), so its own error text must be as safe to surface
    /// to a user as the persona prompt that calls it — the seeded config
    /// dir intentionally includes names spanning the full roster (workers,
    /// meta/infra agents, and internal system terms) so this test would
    /// catch a regression regardless of which category leaked.
    #[tokio::test]
    async fn unknown_agent_is_rejected_without_naming_the_agent_or_roster() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed a realistic roster — worker + meta/infra agent names — so a
        // regression that reintroduces roster enumeration is caught no
        // matter which name it would have leaked.
        for name in [
            "engineer",
            "python-engineer",
            "qa-agent",
            "research-agent",
            "docs-agent",
            "local-ops-agent",
            "plan-agent",
            "ctrl",
            "pm",
            "postmortem-agent",
        ] {
            std::fs::write(
                tmp.path().join(format!("{name}.toml")),
                format!("[agent]\nname = \"{name}\"\n"),
            )
            .unwrap();
        }

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
            msg.contains("code-searcher"),
            "error may echo back the caller's own rejected input, got: {msg}"
        );
        // No roster enumeration: none of the seeded agent names may leak,
        // regardless of whether they're workers or meta/infra agents.
        for leaked in [
            "engineer",
            "python-engineer",
            "qa-agent",
            "research-agent",
            "docs-agent",
            "local-ops-agent",
            "plan-agent",
            "ctrl",
            "postmortem-agent",
        ] {
            assert!(
                !msg.contains(leaked),
                "error must NOT enumerate the on-disk roster (found '{leaked}'), got: {msg}"
            );
        }
        // No internal system/daemon names either.
        for leaked in ["trusty-mpm", "trusty-code", "tcode", "subprocess"] {
            assert!(
                !msg.to_lowercase().contains(&leaked.to_lowercase()),
                "error must NOT leak internal system name '{leaked}', got: {msg}"
            );
        }
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
