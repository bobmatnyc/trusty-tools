//! `tcode run-task` end-to-end execution (#1034, #1035).
//!
//! Why: This is the closer that makes the `tcode` binary actually run a task. It
//! wires every M1 piece together: load the project `CLAUDE.md` context (#1033),
//! assemble the PM system prompt, run the PM through an `AgentLoop` against the
//! real (or mocked) `LlmClient`, let the PM delegate to the `python-engineer` via
//! the `DelegateToAgentTool` + `InProcessAgentRunner`, and have the engineer run
//! its own loop with fs/bash tools scoped to the project. The run captures a diff
//! of the working tree, the PM+engineer transcript, and aggregated token/cost
//! usage, then renders both a human and a JSON report with meaningful exit codes.
//! What: `RunTaskParams` carries the CLI inputs; `execute_run_task` is the async
//! orchestrator returning a `RunReport`. The LLM client is injected so production
//! passes a real `LlmClient` and tests pass a scripted mock — no live key needed.
//! Test: `run_task::tests` drive the whole PM→engineer path offline.

mod diff;
mod recorder;
mod redelegation;
mod report;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::agent_loop::{AgentLoop, AgentLoopConfig, AgentLoopError, CompactionConfig};
use crate::agents::AgentConfig;
use crate::llm::{DebugCaptureSink, LlmClientTrait, wrap_with_debug_capture};
use crate::project_context::load_project_context;
use crate::prompt::assemble_system_prompt;
use crate::provider::{
    resolve_context_window, resolve_deadline_secs, resolve_max_tokens, resolve_model,
};
use crate::runner::{InProcessAgentRunner, RegistryFactory};
use crate::tools::{
    AgentOutput, AgentRunner, BashTool, DelegateToAgentTool, EditTool, EngineerCompletionSignal,
    FinishTaskTool, GlobTool, GrepTool, ListDirTool, ReadFileTool, RunContext, ToolRegistry,
    TrustySearchTool, WriteFileTool, WriteFilesTool,
};

pub use recorder::{RecordingLlmClient, SharedTranscript, TurnRecord};
pub use redelegation::{MAX_REDELEGATIONS, RedelegationCapSignal};
pub use report::{ExitCode, RunReport, aggregate_usage_per_role};

/// Default bash timeout for the engineer's tools, in seconds.
const ENGINEER_BASH_TIMEOUT_SECS: u64 = 120;

/// The delegated engineer's agent name (matches
/// `task::executor::ENGINEER_AGENT_NAME`, the daemon path's own copy of this
/// same literal — kept as two constants, not one shared item, since neither
/// module depends on the other and a shared constant would be a heavier
/// coupling than the two paths' otherwise-independent module boundaries
/// warrant).
const ENGINEER_AGENT_NAME: &str = "python-engineer";

/// Fire-and-forget best-effort trusty-search indexing of a task's working
/// project, at task/session START (trusty-search-first discovery, PR B).
///
/// Why: a tcode run's answers are far better when the working project is
/// discoverable via trusty-search's `search`/`grep`/`get_call_chain` tools, but
/// nothing previously ensured the project's index existed and was populated —
/// that register-and-reindex logic lived only in trusty-mpm's session launch.
/// This shares the ONE promoted implementation
/// ([`trusty_common::search_index::ensure_project_indexed`]) so the two crates
/// can never diverge. It runs at task start, detached, so the index is built
/// WHILE the agent loop proceeds rather than blocking it. Fail-open by design:
/// the shared helper logs-and-swallows every daemon/HTTP error, so a missing or
/// slow trusty-search daemon never affects the run.
/// What: indexes `project` UNCONDITIONALLY — git is a nice-to-have for tcode,
/// not a requirement (owner directive), and trusty-search's own orphan-reaper
/// already garbage-collects indexes for scratch directories that get deleted,
/// so there is no cost to indexing a non-git or short-lived working directory.
/// [`trusty_common::search_index::ensure_project_indexed`] already resolves a
/// sensible root and index id whether or not `project` is inside a git repo.
/// Spawns a detached OS thread that calls the shared helper (which itself runs
/// the blocking HTTP on its own short-timeout threads) and returns immediately,
/// so the caller never waits on the network.
/// After warming, this also SURFACES the resulting index readiness to the
/// session (issue #2784): a cold-indexed large repo has a real semantic
/// warm-up window (lexical ready in seconds, semantic embedding `InProgress`
/// for minutes), so a daily-driver `tcode` session that starts querying
/// immediately would otherwise get lexical-only results with no signal. The
/// detached thread probes `GET /indexes/{id}/status` right after triggering the
/// warm and emits ONE `tracing` line (via the shared
/// [`trusty_common::search_readiness::log_index_readiness`]) describing which
/// lanes are ready — `warn` while semantic is still warming, `info` once it is
/// ready. Same fail-open contract: no daemon ⇒ no log, never a block.
///
/// Test: the promoted helper's fail-open + no-daemon + non-git-root behaviour is
/// unit-tested in `trusty_common::search_index::tests`; the readiness probe/parse
/// in `trusty_common::search_readiness::tests`. This thin spawn wrapper is
/// side-effect-only (detached thread) and has no return to assert; see
/// `spawns_indexing_thread_for_non_git_project_path` for the one thing it IS
/// mechanically checked for here — that it never short-circuits before spawning.
pub(crate) fn ensure_project_indexed_in_background(project: PathBuf) {
    std::thread::spawn(move || {
        // Warm first, then surface the readiness the warm produced so the
        // session knows whether a semantic search is trustworthy yet (#2784).
        let _ = trusty_common::search_index::ensure_project_indexed(&project);
        trusty_common::search_readiness::log_index_readiness(&project);
    });
}

/// Inputs to a single `run-task` invocation, parsed from the CLI.
///
/// Why: Bundling the inputs keeps `execute_run_task`'s signature stable as flags
/// are added and lets the binary layer construct one value from clap.
/// What: `agent` is the top-level (PM) agent name; `task` is the request; `project`
/// is the canonical project root; `agents_dir` is where `<agent>.toml` live;
/// `engineer_model` is the optional per-run engineer model override (#1035).
/// Test: `run_task::tests` build these directly.
#[derive(Debug, Clone)]
pub struct RunTaskParams {
    /// Top-level agent name (typically `"pm"`).
    pub agent: String,
    /// Free-form task description.
    pub task: String,
    /// Canonical project root.
    pub project: PathBuf,
    /// Directory holding `<agent>.toml` configs.
    pub agents_dir: PathBuf,
    /// Optional per-run engineer model override (#1035). `None` routes the
    /// engineer via its own config model.
    pub engineer_model: Option<String>,
    /// Optional per-run wall-clock deadline override, in seconds (#2207).
    /// `None` falls through to [`crate::provider::resolve_deadline_secs`]'s
    /// env-var/default tiers — see that function's docs for the full
    /// precedence. Applied to BOTH the PM's own loop and the delegated
    /// engineer's loop (`build_engineer_runner`).
    pub deadline_secs: Option<u64>,
}

/// Execute a `run-task` end-to-end and return the rendered report.
///
/// Why: The single orchestration entry point the binary calls (with a real
/// `LlmClient`) and tests call (with a scripted mock). Keeping the LLM client as
/// an injected `Arc<dyn LlmClientTrait>` is what makes the whole path testable
/// offline without a network call or an API key.
/// What: Loads the PM config, assembles its prompt (BASE + PM prompt + project
/// `CLAUDE.md`), builds the engineer runner (fs/bash scoped to the project, with
/// the optional per-run model override), wraps it in the PM's
/// `DelegateToAgentTool`, snapshots the working tree, runs the PM `AgentLoop`
/// (#2265 fix #5: wired with `with_stop_signal` against the SAME
/// `redelegation_signal` the engineer runner shares, so the PM stops issuing
/// `delegate_to_agent` calls the turn after the re-delegation cap latches
/// instead of spinning through its remaining turns on doomed retries),
/// snapshots again, and assembles a `RunReport` (diff + transcript + usage/cost +
/// exit code). A PM-config or loop error yields a `ConfigError`/`RunFailure`
/// report rather than a panic.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`,
/// `diff_reflects_engineer_file_change`, `usage_and_cost_aggregate_end_to_end`,
/// `exit_code_reflects_run_failure`,
/// `run_wide_ceiling_stops_the_pm_loop_and_ends_partial_promptly`.
pub async fn execute_run_task(params: RunTaskParams, llm: Arc<dyn LlmClientTrait>) -> RunReport {
    // Trusty-search-first discovery (PR B): at task START, best-effort/detached,
    // ensure the working project is indexed so `search`/`grep` are useful while
    // the loop below proceeds. Fail-open, git-optional — see the helper's docs.
    ensure_project_indexed_in_background(params.project.clone());

    let transcript: SharedTranscript = Arc::new(Mutex::new(Vec::new()));

    // #2264: resolve the (optional) full wire-level debug-capture sink once
    // per run, shared by BOTH the pm and engineer wrappers below so a
    // directory-mode capture lands in one file with a single globally
    // ordered turn sequence — mirrors `transcript`'s own "one shared
    // accumulator per run" shape. `None` when `TCODE_DEBUG_TRANSCRIPT` is
    // unset (the default): `wrap_with_debug_capture` then returns each
    // inner client unchanged, so this costs nothing when disabled.
    let debug_sink: Option<Arc<DebugCaptureSink>> = DebugCaptureSink::from_env();

    // Load the PM config; a missing/invalid config is a configuration error.
    let pm_config =
        match AgentConfig::load(&params.agents_dir.join(format!("{}.toml", params.agent))) {
            Ok(cfg) => cfg,
            Err(e) => return config_error_report(&params, &transcript, &format!("{e:#}")),
        };

    // Load project context (#1033) — absent file is fine.
    let project_context = load_project_context(&params.project);

    // Resolve the PM's own model (no per-call override for the PM).
    let pm_model = resolve_model(&pm_config, None);

    // #2207: resolve the run's wall-clock deadline once, applied to BOTH the
    // PM's own loop (below) and the delegated engineer's loop
    // (`build_engineer_runner`, via `with_timeout_secs`).
    let deadline_secs = resolve_deadline_secs(params.deadline_secs);

    // (#2265) The re-delegation cap signal is shared between the engineer
    // runner's retry decorator (`RedelegatingRunner`, wired inside
    // `build_engineer_runner`) and `assemble_report` below, so a cap-hit run
    // gets a clean terminal report regardless of what the PM's own loop
    // subsequently does.
    let redelegation_signal = RedelegationCapSignal::new();

    // (#2683) The engineer-completion latch is shared between the PM's
    // `DelegateToAgentTool` (which sets it when a delegation returns an explicit
    // successful `finish_task`, and refuses any subsequent re-delegation once
    // set) and `assemble_report` below (which reports success rather than
    // partial for a run the engineer already completed). This is what stops a
    // gratuitous post-finish re-verify round from mislabeling a complete,
    // all-tests-passing run as `partial`/exit-6.
    let completion_signal = EngineerCompletionSignal::new();

    // Build the engineer runner, scoped to the project working dir, sharing the
    // transcript (engineer turns are tagged "python-engineer").
    let engineer_runner = build_engineer_runner(
        wrap_with_debug_capture(Arc::clone(&llm), ENGINEER_AGENT_NAME, debug_sink.as_ref()),
        &params,
        project_context.clone(),
        Arc::clone(&transcript),
        deadline_secs,
        redelegation_signal.clone(),
    );

    // The PM's tool registry: the delegate tool (the PM orchestrates; the
    // engineer does the file work) plus `finish_task` (#2072) so the PM can
    // signal completion with a structured report instead of relying solely on
    // the implicit no-tool-call convention. Pre-flight validation uses the
    // agents dir.
    let mut pm_registry = ToolRegistry::new();
    pm_registry.register(Arc::new(
        DelegateToAgentTool::new(engineer_runner)
            .with_config_dir(params.agents_dir.clone())
            // (#2683) refuse a re-delegation once the engineer has completed.
            .with_completion_signal(completion_signal.clone()),
    ));
    pm_registry.register(Arc::new(FinishTaskTool::new()));

    // The PM's loop uses a transcript-recording client tagged "pm".
    let pm_llm: Arc<dyn LlmClientTrait> = Arc::new(RecordingLlmClient::new(
        wrap_with_debug_capture(Arc::clone(&llm), "pm", debug_sink.as_ref()),
        "pm",
        Arc::clone(&transcript),
    ));

    // Inject DOC-28 catch-up digest as seed context into the PM prompt (#1762 PR2).
    // The catch-up engine is async and fail-open: if the daemon is offline or the
    // project has no activity, it returns None and the prompt is assembled without
    // the section.  Sub-agents are NOT wired here; only the PM receives this digest.
    let catchup_ctx = crate::catchup::pm_catchup_context(&params.project).await;

    let pm_system = assemble_system_prompt(
        &pm_config,
        project_context.as_deref(),
        catchup_ctx.as_deref(),
    );
    // (#2265 fix #5, re-scoped by #2852) Once the shared cap latches, every
    // further `delegate_to_agent` call the PM might issue is a guaranteed dead
    // end — `RedelegatingRunner` will reject it immediately without even
    // invoking the engineer. Wiring the SAME signal into the PM's own loop as
    // a stop condition (checked at the existing turn-boundary, alongside the
    // #2056 cancellation flag) stops the PM from spending its remaining
    // `max_turns` issuing those doomed calls one per turn — the bake-off L1
    // regression fix #5 closes (see `redelegation` module docs for the
    // full before/after).
    //
    // #2852 is what makes that "guaranteed dead end" premise TRUE. This hook
    // is unrecoverable by construction — the PM never gets another turn — so
    // it may only ever fire on a condition no subsequent delegation could
    // clear. `is_cap_reached()` now latches ONLY on the run-wide
    // `MAX_FAILED_INVOCATIONS` ceiling — a run whose delegations have failed
    // that many times over, which is exactly such a condition. Successful
    // invocations never feed it, no matter how many the PM makes.
    // It deliberately does NOT latch on a single delegation exhausting its
    // `MAX_REDELEGATIONS` retries: that is recoverable (a fresh delegation
    // gets a full budget), and stopping the loop on it is precisely the bug
    // #2852 fixes — it killed runs whose next call was the real build.
    // `assemble_report` still maps a cap- OR retry-exhausted, deliverable-
    // bearing run to `ExitCode::Partial` regardless of which `AgentLoopError`
    // variant the PM's loop returns, so this purely trims wasted turns — it
    // does not change the reported outcome.
    let stop_signal = redelegation_signal.clone();
    let pm_loop = AgentLoop::new(
        AgentLoopConfig {
            model: pm_model.clone(),
            max_tokens: resolve_max_tokens(&pm_config),
            timeout_secs: deadline_secs,
            // #2308: model-aware compaction threshold — see
            // `CompactionConfig::for_context_window`'s doc for why the
            // struct-update `..AgentLoopConfig::default()` below must not be
            // allowed to silently supply the flat, model-blind default here.
            compaction: CompactionConfig::for_context_window(resolve_context_window(&pm_model)),
            // #2346: `cadence` stays the `..AgentLoopConfig::default()` value
            // (`None`) here, deliberately — the legacy one-shot/bake-off
            // `run-task` CLI path is explicitly out of scope for the cadence
            // compressor (epic #2343 scopes it to the daemon-driven,
            // persistent-session path in `task::executor::run_and_record`
            // only), so this run sees zero behaviour change from before
            // #2346.
            ..AgentLoopConfig::default()
        },
        pm_llm,
        Arc::new(pm_registry),
    )
    .with_stop_signal(Arc::new(move || stop_signal.is_cap_reached()))
    // #2279: the PM never calls `bash` itself (its registry above is
    // `delegate_to_agent` + `finish_task` only), so its verify-before-finish
    // gate must scan the delegated engineer's transcript instead of its
    // own — `verify_gate::pm_finish_gate` does exactly that against the
    // SAME shared `transcript` the engineer's `RecordingLlmClient` records
    // into (see `build_engineer_runner` above).
    .with_finish_gate(crate::verify_gate::pm_finish_gate(Arc::clone(&transcript)));

    // Snapshot before, run the PM, snapshot after.
    let before = diff::capture_snapshot(&params.project);
    let pm_result = pm_loop.run(&pm_system, &params.task).await;
    let after = diff::capture_snapshot(&params.project);

    // Resolve the engineer's model independently (#1475 bug 1) so the report
    // prices its turns at its OWN rate rather than blending everything
    // under the PM model.
    let engineer_model = resolve_agent_model_slug(
        &params.agents_dir,
        ENGINEER_AGENT_NAME,
        params.engineer_model.as_deref(),
    );

    assemble_report(
        &params,
        &transcript,
        &pm_model,
        &engineer_model,
        RunOutcome {
            before,
            after,
            pm_result,
        },
        &redelegation_signal,
        &completion_signal,
    )
}

/// Build the in-process engineer runner with project-scoped fs/bash tools.
///
/// Why: The engineer must write/read/run inside the project and see the same
/// project context as the PM. This wires a `RegistryFactory` that constructs fs +
/// bash tools scoped to the project dir plus the project context, then routes the
/// result through the per-run model-override seam (`apply_engineer_model_override`).
/// `deadline_secs` (#2207) is the SAME resolved wall-clock budget applied to the
/// delegating PM's own loop, so a raised deadline covers the whole run, not just
/// the PM's half of it. (#2265) The outermost layer is
/// `redelegation::RedelegatingRunner`, which internally retries a failed
/// attempt (reuse-hint-augmented, capped at `redelegation::MAX_REDELEGATIONS`)
/// entirely within one `AgentRunner::run`/`run_with_context` call — this is
/// the chosen "decouple retries from PM turns" design for fix #3: every
/// retry happens inside a SINGLE `delegate_to_agent` tool dispatch, so it
/// never consumes any of the PM's own `AgentLoopConfig::max_turns` budget
/// (left at its crate default of 8 — raising it was the alternative design,
/// rejected here because the cap in fix #1 is a cleaner, purpose-built ceiling
/// than a bigger, still-somewhat-arbitrary PM turn budget would be).
/// What: Returns an `Arc<dyn AgentRunner>` the `DelegateToAgentTool` dispatches
/// to. The engineer's own LLM turns are recorded under the "python-engineer" role.
/// `redelegation_signal` is shared with `assemble_report` via the caller.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`.
fn build_engineer_runner(
    llm: Arc<dyn LlmClientTrait>,
    params: &RunTaskParams,
    project_context: Option<String>,
    transcript: SharedTranscript,
    deadline_secs: u64,
    redelegation_signal: RedelegationCapSignal,
) -> Arc<dyn AgentRunner> {
    let engineer_llm: Arc<dyn LlmClientTrait> = Arc::new(RecordingLlmClient::new(
        llm,
        ENGINEER_AGENT_NAME,
        transcript,
    ));

    let factory: Arc<dyn RegistryFactory> = Arc::new(ProjectToolFactory {
        project: params.project.clone(),
    });

    let mut runner = InProcessAgentRunner::new(engineer_llm, factory, params.agents_dir.clone())
        .with_timeout_secs(deadline_secs);
    if let Some(ctx) = project_context {
        runner = runner.with_project_context(ctx);
    }
    let pinned = apply_engineer_model_override(Arc::new(runner), params);
    Arc::new(redelegation::RedelegatingRunner::new(
        pinned,
        redelegation_signal,
        params.project.clone(),
    ))
}

/// Builds the engineer's project-scoped tool registry for each delegation.
///
/// Why: The engineer needs real file and shell tools that cannot escape the
/// project root, plus (#2072) `finish_task` so it can signal completion with a
/// structured report. This factory constructs `read_file`, `write_file`,
/// `write_files` (#2681 batch write), `edit`, the `glob`/`grep`/`list_dir`
/// exploration tools (#1027), and `bash` all scoped to `self.project`, so the
/// engineer operates only inside the project working tree; `finish_task` has no
/// filesystem footprint and needs no scoping.
/// What: Implements `RegistryFactory::build`, returning an `Arc<ToolRegistry>`
/// with every tool; the runner then gates them by the engineer's
/// `tools.allowed`.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer` (the engineer's
/// `write_file` actually writes into the project).
struct ProjectToolFactory {
    project: PathBuf,
}

#[async_trait]
impl RegistryFactory for ProjectToolFactory {
    async fn build(&self, agent: &AgentConfig, ctx: &RunContext) -> Arc<ToolRegistry> {
        // #2068: resolve the engineer's model slug so `EditTool` can pick its
        // per-model edit-format fallback order (see `resolve_model`'s own
        // precedence docs — RunContext override > agent config > default).
        let model_slug = resolve_model(agent, Some(ctx));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::new(&self.project)));
        reg.register(Arc::new(WriteFileTool::new(&self.project)));
        // #2681: batch-write decouples turn-count from file-count.
        reg.register(Arc::new(WriteFilesTool::new(&self.project)));
        reg.register(Arc::new(
            EditTool::new(&self.project).with_model_slug(model_slug),
        ));
        // #1027: native file discovery so the engineer stops shelling out to
        // find/grep/ls (the dominant bake-off agent-turn sink).
        reg.register(Arc::new(GlobTool::new(&self.project)));
        reg.register(Arc::new(GrepTool::new(&self.project)));
        reg.register(Arc::new(ListDirTool::new(&self.project)));
        // PR A (#2689): trusty-search-backed conceptual discovery as the PRIMARY
        // path; the local glob/grep tools above are the fail-open FALLBACK. The
        // `run_task` CLI is always the daily-driver path (the parity benchmark
        // runs through `task::executor`, which gates this on `HarnessMode`), so
        // registration is unconditional here.
        reg.register(Arc::new(TrustySearchTool::new(&self.project)));
        reg.register(Arc::new(BashTool::new(
            Some(self.project.clone()),
            Duration::from_secs(ENGINEER_BASH_TIMEOUT_SECS),
        )));
        reg.register(Arc::new(FinishTaskTool::new()));
        Arc::new(reg)
    }
}

/// Apply the per-run engineer-model override to the runner, if any (#1035).
///
/// Why: The engineer routes to its own configured model by default. A per-run
/// `--engineer-model` (or `TCODE_ENGINEER_MODEL`) must reroute *only* the engineer
/// for that single run — the model-comparison harness varies the engineer model
/// while the PM model stays fixed. Keeping the override here lets
/// `build_engineer_runner` stay agnostic to whether a model is pinned.
/// What: When `params.engineer_model` is `Some`, wraps the runner in a
/// `ModelPinningRunner` that injects `RunContext.model` on every delegation;
/// otherwise returns the runner unchanged.
/// Test: `run_task::tests::engineer_model_swap_routes_engineer`,
/// `two_runs_route_engineer_to_distinct_slugs`.
fn apply_engineer_model_override(
    runner: Arc<dyn AgentRunner>,
    params: &RunTaskParams,
) -> Arc<dyn AgentRunner> {
    match &params.engineer_model {
        Some(model) => Arc::new(ModelPinningRunner {
            inner: runner,
            model: model.clone(),
        }),
        None => runner,
    }
}

/// Resolve an agent's model slug, honouring an optional per-run override
/// (#1035, #1475 bug 1 fix's prerequisite).
///
/// Why: `InProcessAgentRunner` resolves the engineer's model internally and
/// does not expose it, so pricing the engineer's turns against its OWN
/// model (rather than blending everything under the PM model — the #1475
/// bug) needs this same resolution done independently, here, purely for the
/// pricing step. Shared with the daemon path (`task::executor`'s own
/// `resolve_engineer_model`, which now delegates here) so the two paths
/// can never independently drift on how per-role pricing resolves a model.
/// What: loads `<agents_dir>/<agent_name>.toml`; on any load failure
/// (missing/invalid file) falls back to the literal string `"unknown"` —
/// `crate::perf::cost_usd` degrades gracefully (Sonnet-equivalent pricing)
/// for an unrecognised model rather than erroring, so a missing agent
/// config degrades pricing accuracy, not the whole run. When
/// `model_override` is `Some`, it wins over the config's own model (mirrors
/// `RunContext`'s override precedence).
/// Test: `run_task::tests::resolve_agent_model_slug_falls_back_when_config_missing`,
/// `run_task::tests::resolve_agent_model_slug_honours_override`.
pub fn resolve_agent_model_slug(
    agents_dir: &std::path::Path,
    agent_name: &str,
    model_override: Option<&str>,
) -> String {
    let path = agents_dir.join(format!("{agent_name}.toml"));
    let Ok(config) = AgentConfig::load(&path) else {
        return "unknown".to_string();
    };
    match model_override {
        Some(model) => {
            let ctx = RunContext {
                model: Some(model.to_string()),
                ..Default::default()
            };
            resolve_model(&config, Some(&ctx))
        }
        None => resolve_model(&config, None),
    }
}

/// An `AgentRunner` decorator that pins a fixed model for every delegation (#1035).
///
/// Why: `DelegateToAgentTool::execute` calls `runner.run` with a default
/// `RunContext`, so the per-run `--engineer-model` override cannot be threaded
/// through the tool itself. This decorator injects `RunContext.model` and forwards
/// to `run_with_context`, so the engineer's loop routes to the pinned slug while
/// the PM model stays fixed.
/// What: Holds the inner runner and the pinned model; both `run` and
/// `run_with_context` set `ctx.model` before delegating.
/// Test: `run_task::tests::engineer_model_swap_routes_engineer`.
struct ModelPinningRunner {
    inner: Arc<dyn AgentRunner>,
    model: String,
}

#[async_trait]
impl AgentRunner for ModelPinningRunner {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        let ctx = RunContext {
            model: Some(self.model.clone()),
            ..Default::default()
        };
        self.inner.run_with_context(agent_name, task, &ctx).await
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        let mut ctx = ctx.clone();
        ctx.model = Some(self.model.clone());
        self.inner.run_with_context(agent_name, task, &ctx).await
    }
}

/// Drain the shared transcript into an owned `Vec`.
///
/// Why: The report owns its transcript; this lifts it out of the shared mutex.
/// What: Clones the locked vector (empty on a poisoned lock — the run must not
/// panic while building the report).
/// Test: Exercised by every end-to-end test.
fn drain_transcript(transcript: &SharedTranscript) -> Vec<TurnRecord> {
    transcript.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Build a `ConfigError` report (PM config missing/invalid).
///
/// Why: A configuration failure must produce a faithful report + exit code, not a
/// panic, so callers can branch on it.
/// What: Returns a `RunReport` with an empty diff, the (likely empty) transcript,
/// zero usage, and `ExitCode::ConfigError`; the error text rides in the task line
/// of the human form via a synthesized note.
/// Test: `run_task::tests::missing_pm_config_is_config_error`.
fn config_error_report(
    params: &RunTaskParams,
    transcript: &SharedTranscript,
    err: &str,
) -> RunReport {
    RunReport {
        agent: params.agent.clone(),
        task: format!("{} [config error: {err}]", params.task),
        diff: String::new(),
        transcript: drain_transcript(transcript),
        usage: crate::perf::TokenUsage::default(),
        cost_usd: None,
        exit: ExitCode::ConfigError,
    }
}

/// The run's raw outcome: before/after working-tree snapshots plus the PM
/// loop's own `Result`.
///
/// Why: These three values are always produced and consumed together; bundling
/// them keeps `assemble_report`'s argument count under clippy's
/// `too_many_arguments` limit (#2265 added an 8th argument,
/// `redelegation_signal`, that pushed the plain-argument version over it)
/// without losing any of the fields or their names.
/// What: Plain data holder, no behaviour.
/// Test: Exercised through every `assemble_report` test.
struct RunOutcome {
    before: diff::Snapshot,
    after: diff::Snapshot,
    pm_result: Result<AgentOutput, AgentLoopError>,
}

/// Assemble the final report from the run's outcome.
///
/// Why: Centralises the mapping from (PM loop result, before/after snapshots) to a
/// `RunReport` with the correct exit code so the success / no-change / failure
/// branches never drift.
/// What: (#2683) FIRST — before any error-variant branching — if
/// `completion_signal` reports the delegated engineer already returned an
/// EXPLICIT successful `finish_task` completion, the task is genuinely done:
/// the outcome is `Success` (or `NoChanges` for an empty diff), NEVER
/// `Partial`/`DeadlineExceeded`/`RunFailure`, regardless of how the PM's own
/// loop later terminated. This is the data-integrity fix for the bug where a
/// gratuitous post-finish re-delegation that then ran out of turns/time
/// mislabeled a complete, all-tests-passing run as `partial`/exit-6. On a
/// PM-loop error with NO such completion → `DeadlineExceeded` when the error is
/// specifically `AgentLoopError::Timeout` (#2207 — distinct from a genuine
/// failure so a caller can tell "timed out" from "errored"). (#2265 fix #4)
/// Otherwise, when the error is `AgentLoopError::TurnCapExceeded` OR
/// `redelegation_signal` reports the re-delegation cap (fix #1) was hit, the
/// diff is computed (filesystem-based, independent of which turn/attempt
/// produced it) and — if it is non-empty, i.e. a deliverable actually exists on
/// disk — the outcome is `ExitCode::Partial`, NOT `RunFailure`; trusty-code
/// cannot generically verify correctness, so this gates purely on "work was
/// produced", leaving the downstream bake-off harness to score correctness
/// itself. Every other PM-loop error (a hard `Llm`/`Cancelled` failure, or ANY
/// error with an empty diff — including a cap/turn-cap hit that produced
/// nothing) still maps to `RunFailure`, preserving that path for genuine
/// no-output failures. On success → compute the diff; empty diff → `NoChanges`,
/// else `Success`.
/// Usage and cost are aggregated from the transcript and priced PER ROLE —
/// `pm_model` for `"pm"` turns, `engineer_model` for the delegated engineer's
/// — per the #1475 bug 1 fix (`aggregate_usage_per_role`), not blended under
/// one model. #2206: this aggregation now runs on every error path too, so a
/// `run_failure`, `partial`, or `deadline_exceeded` report still carries the
/// real usage/cost of every turn that DID complete, rather than a zeroed
/// placeholder.
/// Test: `run_task::tests::*` (success, no-change, failure, deadline, and
/// #2265 partial/cap-reached paths).
fn assemble_report(
    params: &RunTaskParams,
    transcript: &SharedTranscript,
    pm_model: &str,
    engineer_model: &str,
    outcome: RunOutcome,
    redelegation_signal: &RedelegationCapSignal,
    completion_signal: &EngineerCompletionSignal,
) -> RunReport {
    let RunOutcome {
        before,
        after,
        pm_result,
    } = outcome;
    let turns = drain_transcript(transcript);

    // #2683: the authoritative success signal. If the delegated engineer
    // already reported an EXPLICIT successful `finish_task` completion, the
    // task is genuinely done — a gratuitous post-finish re-delegation that
    // subsequently ran the PM's loop out of turns/time (or hit any other
    // terminal error) must NOT mislabel this complete, correct run as
    // `partial`/`deadline_exceeded`/`run_failure`. The on-disk diff is the
    // authoritative deliverable regardless of which turn produced it: a
    // non-empty diff is `Success`, an empty one is `NoChanges`. This is checked
    // FIRST, before any PM-error-variant branching, so it overrides every one
    // of them.
    if completion_signal.is_completed() {
        let (usage, cost) = aggregate_usage_per_role(&turns, pm_model, engineer_model);
        let rendered_diff = diff::diff_snapshots(&before, &after);
        let exit = if rendered_diff.trim().is_empty() {
            ExitCode::NoChanges
        } else {
            ExitCode::Success
        };
        return RunReport {
            agent: params.agent.clone(),
            task: params.task.clone(),
            diff: rendered_diff,
            transcript: turns,
            usage,
            cost_usd: Some(cost),
            exit,
        };
    }

    // A PM-loop error is a runtime failure (or, #2207, a deadline; or, #2265,
    // a turn-cap/re-delegation-cap hit that still produced a deliverable);
    // the transcript still reflects whatever turns ran before the error. The
    // PM's `AgentOutput` content is not needed for the report — the
    // transcript and diff are the authoritative artifacts. #2206: usage/cost
    // are aggregated from those completed turns here too, not zeroed.
    if let Err(e) = pm_result {
        let (usage, cost) = aggregate_usage_per_role(&turns, pm_model, engineer_model);

        if matches!(e, AgentLoopError::Timeout { .. }) {
            return RunReport {
                agent: params.agent.clone(),
                task: format!("{} [deadline exceeded: {e}]", params.task),
                diff: String::new(),
                transcript: turns,
                usage,
                cost_usd: Some(cost),
                exit: ExitCode::DeadlineExceeded,
            };
        }

        // #2265 fix #4: a turn-cap or re-delegation-cap terminal condition
        // must not be blindly reported as `run_failure` when a deliverable
        // already exists on disk — the diff is filesystem-based, so it is
        // authoritative regardless of which attempt produced it.
        //
        // #2852 split the one cap flag in two. Reporting must key off BOTH:
        // `is_cap_reached` (the run-wide invocation ceiling, which also stops
        // the PM loop) and `is_retry_budget_exhausted` (some delegation burned
        // all its retries — recoverable, so it does NOT stop the loop, but it
        // is still exactly the "engineer was retried to exhaustion" diagnosis
        // this label exists to report). Keying off only the former would have
        // silently downgraded genuinely retry-exhausted runs to an opaque
        // `run_failure` — losing the diagnostics #2852 set out to improve.
        let cap_reached = redelegation_signal.is_cap_reached();
        let retry_exhausted = redelegation_signal.is_retry_budget_exhausted();
        let budget_spent = cap_reached || retry_exhausted;
        let is_turn_cap = matches!(e, AgentLoopError::TurnCapExceeded { .. });
        let rendered_diff = diff::diff_snapshots(&before, &after);
        let has_deliverable = !rendered_diff.trim().is_empty();

        if (budget_spent || is_turn_cap) && has_deliverable {
            let label = if cap_reached {
                // #2852: quote the FAILURE count, which is the ceiling's actual
                // input and is exactly the number of invocations that really
                // failed — never a pre-incremented dispatch counter, which
                // would tell an operator "13" when 12 occurred.
                format!(
                    "partial: engineer failure ceiling reached after {} failed invocations; \
                     partial work preserved at {}",
                    redelegation_signal.failed_invocations(),
                    params.project.display()
                )
            } else if retry_exhausted {
                format!(
                    "partial: re-delegation limit reached after {} attempts; partial work \
                     preserved at {}",
                    redelegation::MAX_REDELEGATIONS,
                    params.project.display()
                )
            } else {
                "partial: PM turn cap exceeded but a deliverable was produced".to_string()
            };
            return RunReport {
                agent: params.agent.clone(),
                task: format!("{} [{label}: {e}]", params.task),
                diff: rendered_diff,
                transcript: turns,
                usage,
                cost_usd: Some(cost),
                exit: ExitCode::Partial,
            };
        }

        // Genuine failure: no deliverable exists, or the error is neither a
        // turn cap nor a re-delegation-cap hit (e.g. a hard `Llm`/`Cancelled`
        // failure). This path is intentionally left mapped to `RunFailure` —
        // "Keep genuine no-output failures mapped to RunFailure" per #2265.
        // When the cap WAS reached but produced nothing reusable, the label
        // still names that (so an operator/log reader is not left guessing
        // whether this was an opaque crash or an exhausted, reuse-aware
        // retry budget), even though the exit code itself is unchanged.
        let label = if cap_reached {
            format!(
                "run failure (engineer failure ceiling reached after {} failed invocations, \
                 no deliverable produced)",
                redelegation_signal.failed_invocations()
            )
        } else if retry_exhausted {
            format!(
                "run failure (re-delegation limit reached after {} attempts, no deliverable \
                 produced)",
                redelegation::MAX_REDELEGATIONS
            )
        } else {
            "run failure".to_string()
        };
        return RunReport {
            agent: params.agent.clone(),
            task: format!("{} [{label}: {e}]", params.task),
            diff: String::new(),
            transcript: turns,
            usage,
            cost_usd: Some(cost),
            exit: ExitCode::RunFailure,
        };
    }

    let rendered_diff = diff::diff_snapshots(&before, &after);

    // Sum usage across every recorded PM + engineer turn, pricing each turn
    // against its OWN role's resolved model (#1475 bug 1 fix). The PM's own
    // `AgentOutput.usage` omits the engineer's tokens (they return as a
    // tool-result string), so the transcript is the faithful total.
    let (total_usage, cost) = aggregate_usage_per_role(&turns, pm_model, engineer_model);

    let exit = if rendered_diff.trim().is_empty() {
        ExitCode::NoChanges
    } else {
        ExitCode::Success
    };

    RunReport {
        agent: params.agent.clone(),
        task: params.task.clone(),
        diff: rendered_diff,
        transcript: turns,
        usage: total_usage,
        cost_usd: Some(cost),
        exit,
    }
}
