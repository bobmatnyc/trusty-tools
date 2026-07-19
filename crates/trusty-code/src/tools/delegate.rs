//! `delegate_to_agent` tool — the PM's primary tool for dispatching to sub-agents.
//!
//! Why: Keeps the delegation schema and its executor colocated so the PM's tool
//! registry can register a single type that owns both. Pre-flight validation of
//! `agent_name` against the on-disk agent config directory prevents the LLM from
//! hallucinating sub-agent names and crashing the subprocess runner with a
//! confusing IO error (#204 equivalent). (bake-off L1 diagnosis) A delegated
//! engineer that exhausts its turn/time budget has usually already written
//! real, partial progress to disk; the PM's recovery historically was a
//! free-text "re-delegate with a streamlined brief" that never told the next
//! engineer instance to look at what was already there, so it silently
//! rebuilt a simpler (sometimes incomplete) solution and orphaned the correct
//! work. `redelegation_hint` makes the safer instruction automatic: it fires
//! on every `TurnCapExceeded`/`Timeout`/`Cancelled` abort, regardless of
//! whether the PM's own free text remembers to ask for it.
//! What: `DelegateToAgentTool` wraps an `AgentRunner` and (optionally) an agent
//! config directory. `execute()` parses `{agent_name, task}`, validates the agent
//! config exists, and hands off to the runner. On miss, returns a helpful error
//! listing available agents. On a runner failure caused by the sub-agent's own
//! loop aborting with partial work still on disk, the returned `ToolResult`
//! error appends a fixed reuse/continue directive (`redelegation_hint`).
//! (#2265) `redelegation_hint` is also `pub(crate)` so
//! `run_task::redelegation::RedelegatingRunner` can reuse the SAME hint logic
//! to decide, entirely inside one `delegate_to_agent` dispatch, whether a
//! failed engineer attempt is worth automatically retrying — see that
//! module's docs for the retry/cap mechanics.
//! Test: `unknown_agent_returns_helpful_error` builds a tool pointed at a tempdir
//! containing `engineer.toml` only, calls `execute({"agent_name":"ghost",...})`,
//! and asserts the error names the unknown agent and lists `engineer`.
//! `redelegation_hint_*` tests cover the reuse-directive behaviour.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::agent_loop::AgentLoopError;
use crate::runner::RunnerError;
use crate::tools::finish_task::FinishStatus;
use crate::tools::traits::{AgentOutput, AgentRunner, ToolExecutor, ToolResult};

/// Detect whether an `AgentRunner` failure means the sub-agent's OWN attempt
/// may have left partial work already on disk (turn cap, timeout,
/// cancellation, or a retryable LLM/transport hiccup), as opposed to a hard
/// failure (unknown agent, bad config) that leaves nothing to reuse.
///
/// Why: (bake-off L1 diagnosis) The PM only sees this failure as opaque error
/// text; without a structured signal, its free-text re-delegation brief has no
/// reliable reason to mention the sub-agent's partial progress, so the next
/// delegated instance rebuilds from scratch and orphans correct prior work.
/// Surfacing a fixed directive automatically — every time this specific
/// failure shape occurs — makes "read existing files, then continue" the
/// default recovery instead of something the PM must remember to ask for.
/// (#2265) `AgentLoopError::Llm` — a recoverable Bedrock/transport error that
/// aborts the engineer's sub-loop mid-session — is the DOMINANT failure mode
/// observed in the bake-off transcripts, yet #2233 left it out of this match,
/// so the reuse hint never fired for the most common case and every retry
/// re-read PROBLEM.md from zero. It now gets the same hint: tool calls the
/// engineer already dispatched before the failing `chat` call (e.g.
/// `write_file`) already landed on disk even though `AgentLoopError::Llm`
/// itself carries no `partial: Box<AgentOutput>` (unlike the three
/// budget-abort variants) — there is real, on-disk work to reuse even though
/// there is no partial transcript snapshot to point to.
/// What: Downcasts `err` to [`RunnerError`] and matches `RunnerError::Loop`
/// whose source is `AgentLoopError::TurnCapExceeded`, `Timeout`, `Cancelled`,
/// (#2265) `Llm`, or (#2265 fix #5) `StoppedBySignal`. Returns `None` only
/// for `UnknownAgent`/`ConfigLoad` — failures that never reached a sub-agent
/// loop at all, so there is nothing on disk to reuse.
/// Note: a delegated engineer's own sub-loop never actually attaches a stop
/// signal today (only `run_task`'s PM loop does, via
/// `AgentLoop::with_stop_signal`), so `StoppedBySignal` is unreachable from
/// this call site in practice; it is included for exhaustiveness and to stay
/// correct if that ever changes.
/// Test: `redelegation_hint_present_on_turn_cap_exceeded`,
/// `redelegation_hint_present_on_timeout`,
/// `redelegation_hint_present_on_cancelled`,
/// `redelegation_hint_present_on_llm_error`,
/// `redelegation_hint_absent_on_unknown_agent`.
pub(crate) fn redelegation_hint(err: &anyhow::Error) -> Option<&'static str> {
    let RunnerError::Loop { source, .. } = err.downcast_ref::<RunnerError>()? else {
        return None;
    };
    match source {
        AgentLoopError::TurnCapExceeded { .. }
        | AgentLoopError::Timeout { .. }
        | AgentLoopError::Cancelled { .. }
        | AgentLoopError::StoppedBySignal { .. }
        | AgentLoopError::Llm(_) => Some(
            "\n\nNOTE for re-delegation: the sub-agent's previous attempt did not reach a \
             normal finish — it ran out of turns or time, was cancelled, or hit a \
             transient LLM/transport error — NOT a task failure. It has likely already \
             written real, partial progress to disk. Before delegating this task again: \
             (1) inspect the project's current files to see what the sub-agent already \
             produced, (2) instruct the next sub-agent to READ and CONTINUE/BUILD ON that \
             existing work rather than rewriting it from scratch, and (3) consider a narrower \
             follow-up task (or a higher turn budget) so it can finish what is already started \
             instead of starting over.",
        ),
    }
}

/// Run-scoped latch recording that a delegated engineer already reported an
/// EXPLICIT successful completion (`finish_task` with `status: completed`)
/// during this run (#2683).
///
/// Why: The bake-off regression this closes is a PM that, after the engineer
/// already called `finish_task` with all tests passing, fires ONE MORE
/// gratuitous `delegate_to_agent` "re-verify" round; when that extra round
/// then runs out of turns / wall-clock time, the run terminated mid-round and
/// was mislabeled `partial`/exit-6 even though a complete, correct,
/// all-tests-passing deliverable already sat on disk — corrupting run
/// status/telemetry (issue #2683 recurrence comment, 2026-07-15). This shared
/// latch is the authoritative "the task is genuinely done" signal: once set,
/// [`DelegateToAgentTool`] refuses further re-delegation (part b of the fix)
/// and `run_task::assemble_report` reports success rather than partial (the
/// data-integrity half). Modeled as a cheap `Arc<AtomicBool>` handle,
/// mirroring `run_task::redelegation::RedelegationCapSignal`, so the tool (the
/// setter) and the report assembler (the reader) observe the SAME state.
/// What: `new()` starts un-latched; `mark_completed` latches it (set only by
/// [`DelegateToAgentTool::execute`] when the delegated runner returns an
/// `AgentOutput` whose `finish_status` is `Some(FinishStatus::Completed)`);
/// `is_completed` reads it. `Clone` shares the same underlying flag.
/// Test: `tools::delegate::tests::completion_signal_latches_on_completed_finish`,
/// `tools::delegate::tests::completion_signal_ignores_non_completed_finish`,
/// `tools::delegate::tests::delegate_refused_once_engineer_completed`.
#[derive(Debug, Clone, Default)]
pub struct EngineerCompletionSignal(Arc<AtomicBool>);

impl EngineerCompletionSignal {
    /// Build a fresh, un-latched signal for one run.
    ///
    /// Why: Each `execute_run_task` call needs its OWN signal — a prior run's
    /// completion must never leak into a later run's report.
    /// What: `Self::default()`.
    /// Test: `completion_signal_latches_on_completed_finish`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Latch the "engineer completed successfully" flag.
    ///
    /// Why: Called exactly once, the first time a delegation returns an explicit
    /// successful `finish_task` completion.
    /// What: Stores `true` with `SeqCst` (paired with `is_completed`'s load).
    /// Test: `completion_signal_latches_on_completed_finish`.
    pub(crate) fn mark_completed(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether a delegated engineer has already completed successfully.
    ///
    /// Why: [`DelegateToAgentTool::execute`] reads this to refuse a gratuitous
    /// re-delegation, and `run_task::assemble_report` reads it to report
    /// success instead of partial.
    /// What: Loads the latched flag.
    /// Test: `completion_signal_latches_on_completed_finish`.
    pub fn is_completed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Tool executor that delegates a task to a named sub-agent.
///
/// Why: Encapsulates the `delegate_to_agent` tool so the PM loop registers a
/// single `Arc<dyn ToolExecutor>` rather than branching on the tool name inline.
/// What: Holds an `AgentRunner` for subprocess/in-process dispatch, an optional
/// `config_dir` for pre-flight agent name validation, and (#2683) an optional
/// shared [`EngineerCompletionSignal`] used to refuse a re-delegation once the
/// engineer has already reported a successful `finish_task` completion.
/// Test: `unknown_agent_returns_helpful_error`, `known_agent_reaches_runner`,
/// `no_config_dir_skips_validation`, `delegate_refused_once_engineer_completed`.
pub struct DelegateToAgentTool {
    runner: Arc<dyn AgentRunner>,
    /// Directory holding `<agent>.toml` files. When `Some`, `execute()` rejects
    /// calls whose `agent_name` does not have a matching TOML. When `None`,
    /// validation is skipped (legacy / test mode).
    config_dir: Option<PathBuf>,
    /// Shared, run-scoped completion latch (#2683). When `Some` and already
    /// latched, `execute()` refuses further delegation with a recoverable error
    /// that nudges the PM to call `finish_task`. When `None` (the default /
    /// legacy path), the refusal behaviour is disabled entirely.
    completion_signal: Option<EngineerCompletionSignal>,
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
            completion_signal: None,
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

    /// Attach a shared [`EngineerCompletionSignal`] so a re-delegation after a
    /// successful engineer completion is refused (#2683).
    ///
    /// Why: Once the delegated engineer has reported an explicit successful
    /// `finish_task` completion, any further `delegate_to_agent` call is the
    /// gratuitous post-finish re-delegation that mislabels a complete run as
    /// `partial`; refusing it (and nudging the PM to `finish_task` instead) is
    /// the "do not re-delegate once the finish gate is satisfied" half of the
    /// fix. `run_task::execute_run_task` shares the SAME signal instance with
    /// `assemble_report`.
    /// What: Builder-style setter; returns `self` for chaining. When unset (the
    /// default), the refusal behaviour is disabled and this tool behaves
    /// exactly as before.
    /// Test: `delegate_refused_once_engineer_completed`.
    pub fn with_completion_signal(mut self, signal: EngineerCompletionSignal) -> Self {
        self.completion_signal = Some(signal);
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
        // #2683: once the delegated engineer has already reported a successful
        // `finish_task` completion, refuse any further delegation BEFORE
        // parsing/validating args or invoking the runner. This is the "do not
        // re-delegate once the finish gate is satisfied" half of the fix: the
        // deliverable is on disk and the task is done, so a re-verify round is
        // gratuitous — nudge the PM to `finish_task` instead of spawning
        // another engineer loop that can only end the run mid-round and
        // mislabel it `partial`. Recoverable (not fatal) so the PM's loop
        // continues and can act on the guidance.
        if let Some(signal) = &self.completion_signal
            && signal.is_completed()
        {
            // #2857: a harness policy refusal — the PM asked for a delegation
            // and we overrode it. Not a model input error (the args are never
            // even parsed), so `warn`, not `debug`: the run's trajectory just
            // changed by our decision, and only stderr can record that.
            tracing::warn!(
                "delegate_to_agent refused: the completion latch (#2683/#2805) is set — the \
                 delegated engineer already reported a successful finish_task, so this \
                 delegation was rejected without invoking the runner; the PM is being \
                 redirected to finish_task"
            );
            return ToolResult::err(
                "delegate_to_agent refused: the delegated engineer already reported a \
                 successful completion (finish_task with status=completed) for this run. \
                 The deliverable is on disk and the task is done — do NOT re-delegate to \
                 re-verify. Call finish_task now to report the result.",
            );
        }

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
            Ok(out) => {
                // #2683: latch the run-scoped completion signal when the engineer
                // terminated via an EXPLICIT successful `finish_task` — the
                // authoritative "task is genuinely done" signal both the refusal
                // above and `run_task::assemble_report` key off. A `failed` /
                // `cancelled` finish, or a plain no-tool-call stop, does NOT
                // latch it: those leave re-delegation legitimately available.
                self.mark_completion_if_finished(&out);
                ToolResult::ok(out.content)
            }
            Err(e) => {
                let hint = redelegation_hint(&e).unwrap_or("");
                ToolResult::err(format!("sub-agent '{agent_name}' failed: {e:#}{hint}"))
            }
        }
    }
}

impl DelegateToAgentTool {
    /// Latch the shared completion signal iff `out` represents an explicit
    /// successful `finish_task` completion (#2683).
    ///
    /// Why: Factored out of `execute` so the "what counts as a successful
    /// completion" rule lives in one named place and is unit-testable.
    /// What: No-op when no `completion_signal` is attached; otherwise latches it
    /// only when `out.finish_status == Some(FinishStatus::Completed)`.
    /// Test: `completion_signal_latches_on_completed_finish`,
    /// `completion_signal_ignores_non_completed_finish`.
    fn mark_completion_if_finished(&self, out: &AgentOutput) {
        if let Some(signal) = &self.completion_signal
            && out.finish_status == Some(FinishStatus::Completed)
        {
            signal.mark_completed();
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "delegate_tests.rs"]
mod tests;
