//! Spawn the `claude` CLI and parse its stream-json output into `AgentOutput`.
//!
//! Why: Extracted from the runner's helper/setup code so the (long) spawn +
//! NDJSON parse loop lives in one focused file. Keeping it apart from the
//! prompt-sanitization helpers keeps each source file under the size cap.
//! What: Implements `ClaudeCodeAgentRunner::run_with_config{,_ctx,_public}`
//! plus the `AgentRunner` trait for `ClaudeCodeAgentRunner`.
//! Test: `run_parses_stream_json_result`, `run_honors_ctx_model_override`,
//! `run_propagates_is_error`, `run_recovers_from_max_turns_with_content`.

use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::ClaudeCodeAgentRunner;
use crate::agents::AgentConfig;
use crate::agents::agent_model_env;
use crate::ipc::extract_summary;
use crate::perf::TokenUsage;
use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};

impl ClaudeCodeAgentRunner {
    /// Internal: spawn the claude CLI with fully-resolved config and parse
    /// its stream-json output back into `AgentOutput`.
    ///
    /// Why: Extracted so both `run` and `run_with_history` can call it; also
    /// makes the I/O loop easier to test with a mock script.
    /// What: Builds the argv from `cfg`, spawns the process, reads NDJSON
    /// lines until it sees `{"type":"result"}`, returns the parsed content.
    /// Test: `run_parses_stream_json_result`.
    pub(super) async fn run_with_config(
        &self,
        cfg: &AgentConfig,
        task: &str,
    ) -> Result<AgentOutput> {
        self.run_with_config_ctx(cfg, task, &RunContext::default())
            .await
    }

    /// Public re-export of `run_with_config` for #250 ctrl OAuth routing.
    ///
    /// Why: The ctrl orchestrator needs to invoke the claude CLI for its own
    /// LLM call when only `CLAUDE_CODE_OAUTH_TOKEN` is configured. The
    /// existing `run_with_config` is crate-private to keep the runner's
    /// internal trait surface tight, but the ctrl path is also crate-internal
    /// so a thin pub wrapper costs nothing and avoids leaking `RunContext`
    /// into the call site.
    /// What: Forwards to `run_with_config_ctx` with a default context.
    /// Test: Exercised via `ctrl::run_pm_task_via_claude_cli`.
    pub async fn run_with_config_public(
        &self,
        cfg: &AgentConfig,
        task: &str,
    ) -> Result<AgentOutput> {
        self.run_with_config(cfg, task).await
    }

    /// Like `run_with_config` but honors a `RunContext`.
    ///
    /// Why: CRIT-1 / MAJ-1 — previously the parent process mutated
    /// `TAGENT_MAX_TURNS` (unsafe under multi-threaded tokio) and there
    /// was no way to set the subprocess CWD, so the wave loop had to copy
    /// files back out of the parent's cwd. Threading a `RunContext` lets
    /// the runner scope these to the child only and use `out_dir` as CWD.
    /// What: `ctx.working_dir` → `Command::current_dir`; `ctx.max_turns_override`
    /// overrides `cfg.llm.max_turns` (passed as `--max-turns` CLI arg);
    /// `ctx.assigned_file` is surfaced via `TAGENT_ASSIGNED_FILE` on the
    /// child env only.
    /// Test: Exercised end-to-end by the wave-loop tests.
    pub(super) async fn run_with_config_ctx(
        &self,
        cfg: &AgentConfig,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        // #107: `ctx.model` (from the workflow phase's `model` field) takes
        // precedence over the TOML-resolved model so per-phase overrides
        // actually reach the `claude` CLI. An agent-specific
        // `TAGENT_MODEL_<AGENT>` env var still wins overall because
        // `AgentConfig::load` bakes that into `cfg.agent.model` before we
        // even see `ctx`, preserving the documented priority chain
        // (env > phase override > agent TOML > default).
        let effective_model = match ctx.model.as_deref() {
            Some(m) if !m.is_empty() && agent_model_env(&cfg.agent.name).is_none() => m,
            _ => cfg.agent.model.as_str(),
        };
        let model = Self::normalize_model(effective_model);
        let max_turns_value = ctx.max_turns_override.unwrap_or(cfg.llm.max_turns);
        let max_turns = max_turns_value.to_string();

        tracing::info!(
            agent = %cfg.agent.name,
            model = %model,
            claude_bin = %self.claude_bin.display(),
            max_turns = max_turns_value,
            working_dir = ?ctx.working_dir,
            "spawning claude CLI"
        );

        // #325: The `claude` CLI subprocess does not accept `stop_sequences`
        // via its IPC, so any TOML-declared values are silently dropped on
        // this runner. Warn once per dispatch so operators discover the
        // mismatch instead of wondering why their stop strings don't fire.
        if !cfg.llm.stop_sequences.is_empty() {
            tracing::warn!(
                agent = %cfg.agent.name,
                stop_sequences = ?cfg.llm.stop_sequences,
                "stop_sequences set but not forwarded to claude-code runner — \
                 the claude CLI subprocess does not support this parameter via IPC; \
                 sequences will be ignored"
            );
        }

        // #281: Capture wall-clock start so we can record dispatch duration in
        // the usage log even when the CLI omits a `usage` block (older
        // versions). Read after the CLI exits below.
        let dispatch_started = std::time::Instant::now();

        // #199: signal that the claude-code agent's work loop is starting.
        // Distinct from `AgentSpawned` (which fires when delegation is decided)
        // — this fires at the actual CLI invocation.
        crate::events::publish(crate::events::Event::AgentStarted {
            session_id: crate::env_compat::env_var("TAGENT_RUN_ID", "OPEN_MPM_RUN_ID")
                .unwrap_or_default(),
            agent_name: cfg.agent.name.clone(),
            runner_type: "claude-code".to_string(),
        });

        // #113: The `claude` CLI does not know about trusty-agents's `finish_task`
        // tool, so any TOML / task text that tells the agent to "call
        // finish_task" makes it spin until max_turns. Strip those lines here
        // — proper fix corresponding to option 4 in the bug report.
        let sanitized_task = Self::strip_finish_task_instructions(task);
        // Inject harness-protocol layers (base + claude-code-specific or
        // finish-task depending on cfg.llm.use_finish_task) BEFORE the TOML
        // base prompt, then strip any lingering `finish_task` mentions. The
        // claude-code CLI doesn't speak the finish_task tool, so if the
        // finish-task harness layer was injected it must also be stripped by
        // `strip_finish_task_instructions` — that's fine because the rule is
        // "claude-code agents never call finish_task", which is enforced via
        // the harness injection selector above (we still strip defensively).
        let composed_system =
            Self::prepend_harness_layers(&cfg.system_prompt.content, cfg.llm.use_finish_task);
        let sanitized_system = Self::strip_finish_task_instructions(&composed_system);

        let mut cmd = Command::new(&self.claude_bin);
        cmd.args([
            "-p",
            &sanitized_task,
            "--model",
            &model,
            "--system-prompt",
            &sanitized_system,
            "--output-format",
            "stream-json",
            "--verbose",
            "--dangerously-skip-permissions",
            "--max-turns",
            &max_turns,
        ])
        .stdout(Stdio::piped())
        // #268: Sub-agent `claude` CLI subprocesses must not bleed their own
        // tracing/log output to the parent's TTY. The parent REPL routes its
        // own tracing to `~/.trusty-agents/logs/repl.log` when stdin is a TTY, but
        // sub-agent processes detect a non-TTY stdin and default to stderr —
        // which the parent inherits, so log lines clobber the carefully
        // positioned chat scrollback.
        // Detection: when the parent's stdin is a TTY, this is an interactive
        // REPL session and sub-agent stderr must be silenced. When the parent
        // is non-interactive (CI, piped input, --workflow, --api), inherit
        // stderr so existing log-capture tooling continues to work.
        .stderr(if crate::repl::is_tty() {
            Stdio::null()
        } else {
            Stdio::inherit()
        });

        if let Some(wd) = &ctx.working_dir {
            cmd.current_dir(wd);
            // #159: The `claude` CLI anchors write paths to the git root, not
            // the subprocess CWD. Passing `--add-dir` scopes the claude
            // session to the designated output directory so file writes land
            // in `out_dir` rather than the repo root.
            cmd.arg("--add-dir").arg(wd);
        }
        // Pass --allowedTools if the agent config restricts claude CLI tools.
        // Why: Research agents should not write files or run shell — only
        // WebSearch/WebFetch. Empty vec (default) means no restriction.
        if !cfg.llm.claude_allowed_tools.is_empty() {
            cmd.arg("--allowedTools")
                .arg(cfg.llm.claude_allowed_tools.join(","));
        }
        if let Some(path) = &ctx.assigned_file {
            cmd.env("TAGENT_ASSIGNED_FILE", path);
        }
        // Mark this claude CLI invocation as an MPM-spawned sub-agent so the
        // `trusty-mpm hook` command wired into its Claude Code settings
        // short-circuits. Without this guard a nested claude session would
        // re-emit PreToolUse / PostToolUse events (doubling the daemon's
        // audit feed). The memory enrichment hook (`trusty-memory
        // prompt-context`) deliberately does NOT guard on this variable —
        // sub-agents benefit from the parent palace's prompt-fact block as
        // much as the PM does. The variable name is sourced from
        // `trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR` so
        // every spawn site and consumer references the same literal.
        cmd.env(
            trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR,
            "1",
        );

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn claude CLI at {}",
                self.claude_bin.display()
            )
        })?;

        // #283 follow-up: latency instrumentation. Record spawn-to-ready
        // wall-clock so we can distinguish "CLI took forever to start" from
        // "model itself was slow".
        let spawn_elapsed_ms = dispatch_started.elapsed().as_millis() as u64;
        tracing::debug!(spawn_ms = spawn_elapsed_ms, "claude CLI spawned");

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("claude CLI stdout was not captured"))?;

        let mut lines = BufReader::new(stdout).lines();
        // TTFT (time-to-first-token) is the elapsed from `dispatch_started`
        // to the first line emitted on stdout.
        let mut first_line = true;
        let mut final_result: Option<String> = None;
        let mut is_error = false;
        let mut subtype: Option<String> = None;
        // #232: Capture token usage from the terminal `result` event so the
        // perf collector can attribute prompt/completion/cache tokens to
        // claude-code phases instead of always recording zeros.
        let mut result_usage = TokenUsage::default();
        // #113: The `claude` CLI streams incremental assistant text events
        // BEFORE the terminal `result` event. When the CLI hits `max_turns`
        // the final `result` event is `{is_error:true, subtype:"error_max_turns"}`
        // with `result` sometimes empty — but the actual content the agent
        // produced lives in those intermediate `assistant` events. We
        // accumulate that text so we can recover usable output on max_turns.
        let mut assistant_buf = String::new();

        while let Some(line) = lines
            .next_line()
            .await
            .context("failed to read claude CLI stdout")?
        {
            if first_line {
                tracing::debug!(
                    ttft_ms = dispatch_started.elapsed().as_millis() as u64,
                    "time-to-first-token"
                );
                first_line = false;
            }
            if line.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                tracing::trace!(line = %line, "skipping non-JSON line from claude");
                continue;
            };

            match event.get("type").and_then(|v| v.as_str()) {
                Some("result") => {
                    is_error = event
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    subtype = event
                        .get("subtype")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    final_result = event
                        .get("result")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    if let Some(u) = event.get("usage") {
                        result_usage = TokenUsage {
                            prompt_tokens: u
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32,
                            completion_tokens: u
                                .get("output_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32,
                            cache_read_tokens: u
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32,
                            cache_creation_tokens: u
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0)
                                as u32,
                        };
                    }
                    break;
                }
                Some("assistant") => {
                    // Extract any text content blocks from the assistant
                    // message so we can salvage output if the CLI terminates
                    // abnormally (e.g. max_turns). Silently ignore shapes we
                    // don't recognize — these are advisory only.
                    if let Some(blocks) = event
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        for block in blocks {
                            if block.get("type").and_then(|v| v.as_str()) == Some("text")
                                && let Some(text) = block.get("text").and_then(|v| v.as_str())
                            {
                                if !assistant_buf.is_empty() {
                                    assistant_buf.push('\n');
                                }
                                assistant_buf.push_str(text);
                            }
                        }
                    }
                }
                Some(other) => {
                    tracing::trace!(event_type = %other, "claude stream event");
                }
                None => {}
            }
        }

        // Reap the child; exit status is advisory (the stream-json `result`
        // event is the authoritative outcome).
        let _ = child.wait().await;

        // #281: Per-dispatch usage log. tokens come from the result event's
        // `usage` block when present (zero otherwise). duration is wall-clock
        // around the entire spawn → result-event loop.
        let dispatch_duration_ms = dispatch_started.elapsed().as_millis() as u64;

        // Publish LlmRequested + LlmResponded so the REPL's token relay (and
        // any other event-bus consumer) receives the same tokens-in /
        // tokens-out signal that OpenRouter / Anthropic-direct / Bedrock
        // produce. The HTTP-based paths in `src/llm/mod.rs` emit these via
        // `emit_llm_requested` / `emit_llm_responded`; the claude-code CLI
        // path didn't, so the input-bar live counter never moved when an
        // agent ran via the CLI. Fire them as a pair after parsing the
        // `result` stream-json event so both prompt and completion totals
        // are known.
        //
        // Why: Live token tracking must work for ALL LLM dispatch paths, not
        // just the REST ones.
        // What: Empty `agent_name` and zero latency are acceptable here —
        // the relay only needs the token counts. We use the agent's name
        // and the dispatch duration to keep the events useful for other
        // consumers too.
        {
            let session_id =
                crate::env_compat::env_var("TAGENT_RUN_ID", "OPEN_MPM_RUN_ID").unwrap_or_default();
            let agent_name = cfg.agent.name.clone();
            let model_name = model.clone();
            let prompt_tokens = result_usage.prompt_tokens;
            let completion_tokens = result_usage.completion_tokens;
            crate::events::publish(crate::events::Event::LlmRequested {
                session_id: session_id.clone(),
                agent_name: agent_name.clone(),
                model: model_name.clone(),
                prompt_tokens: if prompt_tokens > 0 {
                    Some(prompt_tokens)
                } else {
                    None
                },
            });
            crate::events::publish(crate::events::Event::LlmResponded {
                session_id,
                agent_name,
                model: model_name,
                completion_tokens: if completion_tokens > 0 {
                    Some(completion_tokens)
                } else {
                    None
                },
                latency_ms: dispatch_duration_ms,
            });
        }
        {
            let agent = cfg.agent.name.clone();
            let model_for_log = model.clone();
            let task_for_prefix = task.to_string();
            let input_tokens = result_usage.prompt_tokens;
            let output_tokens = result_usage.completion_tokens;
            let record = crate::usage::UsageRecord::new(
                agent,
                model_for_log,
                "claude-code",
                input_tokens,
                output_tokens,
                dispatch_duration_ms,
                &task_for_prefix,
            );
            let project_dir = crate::usage::project_dir();
            tokio::spawn(async move {
                crate::usage::append_usage(&project_dir, &record).await;
            });
        }

        // #113: Treat `error_max_turns` with recoverable content as SUCCESS.
        // When `finish_task` slips into a claude-code agent's prompt (or the
        // agent is simply verbose) the CLI can exhaust its turn budget even
        // though the work was completed. If we can recover content from the
        // result or the accumulated assistant stream, prefer that over
        // failing the whole phase. A warning log keeps the occurrence
        // visible so operators notice runaway turn counts.
        let max_turns_recoverable = is_error
            && subtype.as_deref() == Some("error_max_turns")
            && (final_result.as_ref().is_some_and(|s| !s.trim().is_empty())
                || !assistant_buf.trim().is_empty());

        if max_turns_recoverable {
            tracing::warn!(
                agent = %cfg.agent.name,
                max_turns = max_turns_value,
                result_chars = final_result.as_deref().map(str::len).unwrap_or(0),
                assistant_chars = assistant_buf.len(),
                "claude CLI hit max_turns but produced content; treating as success (#113)"
            );
            is_error = false;
            if final_result.as_ref().is_none_or(|s| s.trim().is_empty()) {
                final_result = Some(assistant_buf.clone());
            }
        }

        if is_error {
            let msg = final_result
                .unwrap_or_else(|| "claude CLI returned an error with no message".into());
            let subtype_tag = subtype.as_deref().unwrap_or("<none>");
            return Err(anyhow!("claude CLI error [subtype={subtype_tag}]: {msg}"));
        }

        let content = final_result
            .ok_or_else(|| anyhow!("claude CLI exited without producing a `result` event"))?;

        // Reuse the shared summary extractor so claude-code agents match
        // other runners in how downstream phases consume output.
        let summary_raw = extract_summary(&content);
        let summary = if summary_raw.is_empty() {
            None
        } else {
            Some(summary_raw)
        };

        // #199: emit `ReportGenerated` so the UI shows the agent's report
        // size before the engine moves on to the next phase.
        crate::events::publish(crate::events::Event::ReportGenerated {
            session_id: crate::env_compat::env_var("TAGENT_RUN_ID", "OPEN_MPM_RUN_ID")
                .unwrap_or_default(),
            agent_name: cfg.agent.name.clone(),
            word_count: content.split_whitespace().count(),
            status: "success".to_string(),
        });

        Ok(AgentOutput {
            content,
            summary,
            // #232: Token usage is parsed from the `usage` field of the
            // terminal `{"type":"result"}` event in stream-json output. When
            // the field is absent (older CLI versions) this falls back to
            // zeros, matching the previous behavior.
            usage: result_usage,
        })
    }
}

#[async_trait]
impl AgentRunner for ClaudeCodeAgentRunner {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        // #96: Load the agent config via the tokio-aware loader so we don't
        // block the async runtime on disk I/O mid-dispatch.
        let cfg = AgentConfig::by_name_async(agent_name)
            .await
            .with_context(|| format!("failed to load agent config for '{agent_name}'"))?;
        self.run_with_config(&cfg, task).await
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        let cfg = AgentConfig::by_name_async(agent_name)
            .await
            .with_context(|| format!("failed to load agent config for '{agent_name}'"))?;
        self.run_with_config_ctx(&cfg, task, ctx).await
    }
}
