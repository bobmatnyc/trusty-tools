//! `ClaudeCodeAgentRunner` — spawn the `claude` CLI as an agent (#60).
//!
//! Why: Claude Max subscribers already pay for unlimited Claude usage through
//! their claude.ai OAuth session. Routing trusty-agents agents through the
//! locally-installed `claude` CLI in headless mode lets them run under that
//! subscription with no API key — no `OPENROUTER_API_KEY`, no
//! `ANTHROPIC_API_KEY`. Any agent can opt in by setting
//! `[agent].runner = "claude-code"` in its TOML.
//! What: This module root holds the `ClaudeCodeAgentRunner` struct, its
//! prompt-sanitization + auth helpers, and the `claude` discovery helper.
//! The spawn + stream-json parse loop lives in `run`, and the per-agent
//! `DispatchingAgentRunner` lives in `dispatcher`.
//! Test: `normalize_model_strips_anthropic_prefix`,
//! `normalize_model_openai_untouched`, and `run_parses_stream_json_result`
//! (the last uses a mock shell script standing in for `claude`).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use tokio::process::Command;

use crate::agents::harness_protocol::{BASE_PROTOCOL, CLAUDE_CODE_PROTOCOL, FINISH_TASK_PROTOCOL};
use crate::agents::prompt_builder::LAYER_SEPARATOR;

mod dispatcher;
mod run;

#[cfg(test)]
mod tests;

pub use dispatcher::DispatchingAgentRunner;

/// Runs a task by spawning the `claude` CLI.
///
/// Why: The CLI already handles OAuth/session auth and the Claude Max plan,
/// so we delegate the entire LLM round-trip to it and just parse its
/// stream-json output back into our internal shape.
/// What: Holds the absolute path to the `claude` binary (resolved once at
/// construction) plus a flag for skipping the auth-status check (for tests).
/// Test: See module tests.
pub struct ClaudeCodeAgentRunner {
    pub(crate) claude_bin: PathBuf,
}

impl ClaudeCodeAgentRunner {
    /// Locate `claude` in `PATH` and return a ready-to-use runner.
    ///
    /// Why: Failing fast at construction time gives callers (the
    /// `WorkflowEngine` startup path) a clear error before any agent tries to
    /// execute.
    /// What: Searches each entry in `PATH` for an executable `claude` file.
    /// Returns `Err` with a user-facing message if nothing is found.
    /// Test: Covered indirectly — the happy-path test injects a fake binary
    /// via the pub(crate) field to avoid depending on the host's `claude`.
    pub async fn new() -> Result<Self> {
        let claude_bin = find_claude()?;
        Ok(Self { claude_bin })
    }

    /// Strip the `anthropic/` OpenRouter prefix so the `claude` CLI sees a
    /// model name it recognizes.
    ///
    /// Why: trusty-agents agent TOMLs use OpenRouter-style model strings like
    /// `anthropic/claude-sonnet-4-6`; the `claude` CLI expects either a
    /// short alias (`sonnet`) or a bare model name (`claude-sonnet-4-6`).
    /// What: `anthropic/<name>` → `<name>`; other prefixes pass through
    /// unchanged.
    /// Test: `normalize_model_strips_anthropic_prefix`,
    /// `normalize_model_openai_untouched`.
    pub fn normalize_model(model: &str) -> String {
        model.trim_start_matches("anthropic/").to_string()
    }

    /// Strip `finish_task`-related instructions from a prompt before passing
    /// it to the `claude` CLI (#113).
    ///
    /// Why: Agent TOMLs with `use_finish_task = true` include instructions
    /// telling the agent to call `finish_task` to signal completion. That
    /// tool is auto-registered only on the subprocess/IPC path; the `claude`
    /// CLI knows nothing about it. When those instructions leak into a
    /// claude-code agent's system prompt, the model dutifully waits for a
    /// tool that never exists, burns through every turn trying to invent it,
    /// and the CLI eventually terminates with `max_turns`. Stripping the
    /// instructions at the runner boundary makes claude-code agents behave
    /// like any normal Claude chat: they just stop producing text when done.
    /// What: Scans the input line-by-line. Drops any line whose
    /// case-insensitive content mentions `finish_task`. Also removes simple
    /// numbered-list items whose surviving body becomes empty. Keeps the
    /// surrounding structure intact so prose around the instruction is
    /// unaffected.
    /// Test: `strip_finish_task_instructions_removes_callouts`,
    /// `strip_finish_task_instructions_is_idempotent`,
    /// `strip_finish_task_instructions_preserves_unrelated_text`.
    /// Prepend harness-protocol layers to the agent's base system prompt.
    ///
    /// Why: The `claude` CLI path doesn't go through `SystemPromptBuilder`
    /// (it passes `cfg.system_prompt.content` directly via `--system-prompt`),
    /// so we need to apply the same harness-layer injection the subprocess
    /// path gets from `SystemPromptBuilder::add_harness_layer`. Keeping the
    /// loading logic in one helper means we only mirror the selection rules
    /// (base always, claude-code when not use_finish_task, finish-task when
    /// use_finish_task) in one place.
    /// What: Sources harness content from compiled-in constants
    /// (`BASE_PROTOCOL` always, `CLAUDE_CODE_PROTOCOL` when
    /// `use_finish_task == false` since this runner ALWAYS targets
    /// claude-code, `FINISH_TASK_PROTOCOL` when `use_finish_task == true`),
    /// then concatenates them above `base_prompt` using `LAYER_SEPARATOR`.
    /// Test: `prepend_harness_layers_adds_base_and_claude_code`.
    fn prepend_harness_layers(base_prompt: &str, use_finish_task: bool) -> String {
        let mut layers: Vec<&str> = Vec::new();
        if !BASE_PROTOCOL.trim().is_empty() {
            layers.push(BASE_PROTOCOL);
        }
        if !use_finish_task && !CLAUDE_CODE_PROTOCOL.trim().is_empty() {
            layers.push(CLAUDE_CODE_PROTOCOL);
        }
        if use_finish_task && !FINISH_TASK_PROTOCOL.trim().is_empty() {
            layers.push(FINISH_TASK_PROTOCOL);
        }
        if layers.is_empty() {
            return base_prompt.to_string();
        }
        let mut out = String::new();
        for layer in &layers {
            out.push_str(layer.trim_end());
            out.push_str(LAYER_SEPARATOR);
        }
        out.push_str(base_prompt);
        out
    }

    pub fn strip_finish_task_instructions(prompt: &str) -> String {
        let mut out = String::with_capacity(prompt.len());
        for line in prompt.split_inclusive('\n') {
            // Preserve the trailing newline if present.
            let (body, nl) = match line.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (line, ""),
            };
            if body.to_ascii_lowercase().contains("finish_task") {
                // Drop the whole line; for standalone bullets this also
                // removes the enumeration marker so no orphan "5." remains.
                continue;
            }
            out.push_str(body);
            out.push_str(nl);
        }
        out
    }

    /// Verify that the resolved `claude` CLI is authenticated.
    ///
    /// Why: (#60) If the user has not run `claude auth login`, every spawn
    /// will fail with a confusing error buried in stderr. A one-shot check
    /// at engine startup surfaces the problem with an actionable message.
    /// What: Runs `claude auth status` with stdio suppressed and inspects
    /// the exit code. Exit 0 = authenticated.
    /// Test: Covered at the wiring level; we don't exercise the real CLI in
    /// unit tests since that would require a real OAuth session.
    pub async fn check_auth(&self) -> Result<()> {
        let status = Command::new(&self.claude_bin)
            .args(["auth", "status"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("failed to run `claude auth status`")?;
        if !status.success() {
            anyhow::bail!(
                "claude CLI is not authenticated — run `claude auth login` to connect your Claude Max account"
            );
        }
        Ok(())
    }
}

/// Find an executable named `claude` in the user's `PATH`.
///
/// Why: Avoid pulling in the `which` crate for a single tiny helper; the
/// project already has zero-dep discipline elsewhere.
/// What: Walks `PATH` entries in order, returning the first one that
/// contains a regular file named `claude`. On non-unix platforms also
/// considers `claude.exe`.
/// Test: Indirectly exercised by `ClaudeCodeAgentRunner::new` on dev
/// machines; unit tests inject the path directly.
fn find_claude() -> Result<PathBuf> {
    let path_var = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH env var is not set"))?;
    for dir in std::env::split_paths(&path_var) {
        if candidate_exists(&dir, "claude") {
            return Ok(dir.join("claude"));
        }
        #[cfg(windows)]
        if candidate_exists(&dir, "claude.exe") {
            return Ok(dir.join("claude.exe"));
        }
    }
    Err(anyhow!(
        "`claude` CLI not found in PATH — install Claude Code (https://claude.ai/download) \
         to use `runner = \"claude-code\"`"
    ))
}

fn candidate_exists(dir: &Path, name: &str) -> bool {
    let p = dir.join(name);
    p.is_file()
}
