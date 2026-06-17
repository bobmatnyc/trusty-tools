//! Live in-process orchestrator for the standalone metaharness (#1030, WI-4).
//!
//! Why: This is the gating WI-4 piece of the #1045 metaharness — it replaces the
//! WI-2 `NoopAgentRunner` with the live [`InProcessAgentRunner`] so `tm meta run`
//! drives a *real* PM → sub-agent delegation end-to-end. The PM runs its own
//! [`AgentLoop`]; its `delegate_to_agent` tool dispatches to the in-process
//! runner, which loads the engineer agent and drives the engineer's own loop;
//! both share one `Arc<dyn LlmClientTrait>` so token usage rolls up onto a single
//! transcript. The orchestrator returns a combined [`MetaTranscript`] (PM turn +
//! engineer turns + usage rollup + file artifacts).
//! What: [`RecordingRunner`] wraps the real runner to capture each delegation's
//! `(agent, task, AgentOutput)` for the transcript. [`MetaRegistryFactory`] is the
//! [`RegistryFactory`] that assembles each sub-agent's tool set (fs/bash scoped to
//! the project + a nested delegate). [`Orchestrator`] wires the PM registry +
//! AgentLoop and exposes [`Orchestrator::run`], which executes one delegation
//! cycle and returns a [`MetaTranscript`].
//! Test: `orchestrator::tests` drive a full PM → delegate → engineer loop with a
//! scripted `LlmClientTrait` mock (no live LLM), asserting the combined transcript
//! captures both turns, both usages, and the engineer's file artifact.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use trusty_code::agent_loop::{AgentLoop, AgentLoopConfig};
use trusty_code::agents::AgentConfig;
use trusty_code::llm::LlmClientTrait;
use trusty_code::prompt::assemble_system_prompt;
use trusty_code::provider::resolve_model;
use trusty_code::runner::{InProcessAgentRunner, InProcessRunnerConfig, RegistryFactory};
use trusty_code::tools::{
    AgentOutput, AgentRunner, BashTool, DelegateToAgentTool, EditTool, ReadFileTool, RunContext,
    ToolRegistry, WriteFileTool,
};

use super::agents::PM_AGENT_NAME;
use super::transcript::{AgentTurn, Artifact, MetaTranscript};

/// One captured delegation: the agent, its task, and the returned output.
///
/// Why: The combined transcript needs every sub-agent turn, but the PM's
/// `AgentLoop` only returns the PM's own output — sub-agent results flow through
/// the delegate tool. Recording each delegation as it happens is how the
/// orchestrator reconstructs the sub-agent turns for the transcript.
/// What: the delegated `agent` slug, the `task` string, and the engineer's
/// `AgentOutput` (content + usage).
/// Test: `orchestrator_records_delegation` asserts a record is captured.
#[derive(Debug, Clone)]
pub(crate) struct DelegationRecord {
    /// The sub-agent slug that was delegated to.
    pub agent: String,
    /// The task string passed to the sub-agent.
    pub task: String,
    /// The sub-agent's returned output.
    pub output: AgentOutput,
}

/// An [`AgentRunner`] decorator that records every delegation it performs.
///
/// Why: The orchestrator must observe each sub-agent's output and usage to build
/// the combined transcript; the runner is the single choke point every delegation
/// flows through, so wrapping it captures them all without threading state through
/// the agent loop.
/// What: Holds the inner runner and a shared `Mutex<Vec<DelegationRecord>>`. Its
/// `run`/`run_with_context` delegate to the inner runner, then push the result
/// (on success) into the shared log before returning it unchanged.
/// Test: `orchestrator_records_delegation`.
pub(crate) struct RecordingRunner {
    inner: Arc<dyn AgentRunner>,
    records: Arc<Mutex<Vec<DelegationRecord>>>,
}

impl RecordingRunner {
    /// Wrap `inner`, recording into the shared `records` log.
    ///
    /// Why: Constructor injection keeps the decorator testable and lets the
    /// orchestrator share the same `records` handle it later reads.
    /// What: Stores the inner runner and the shared record log.
    /// Test: `orchestrator_records_delegation`.
    pub(crate) fn new(
        inner: Arc<dyn AgentRunner>,
        records: Arc<Mutex<Vec<DelegationRecord>>>,
    ) -> Self {
        Self { inner, records }
    }

    /// Push a successful delegation onto the shared log.
    ///
    /// Why: Both trait methods record identically; factoring it avoids drift.
    /// What: Locks the log and appends a `DelegationRecord`. A poisoned lock is
    /// best-effort-skipped — recording is telemetry, never a failure path — but it
    /// emits a `tracing::warn!` (to stderr) so the dropped delegation turn is
    /// visible rather than silently lost.
    /// Test: `orchestrator_records_delegation`.
    fn record(&self, agent: &str, task: &str, output: &AgentOutput) {
        match self.records.lock() {
            Ok(mut log) => log.push(DelegationRecord {
                agent: agent.to_string(),
                task: task.to_string(),
                output: output.clone(),
            }),
            Err(_) => tracing::warn!(
                agent,
                "metaharness: delegation-record lock poisoned; dropping recorded turn (transcript will omit this delegation)"
            ),
        }
    }
}

#[async_trait]
impl AgentRunner for RecordingRunner {
    /// Delegate to the inner runner and record the result on success.
    ///
    /// Why: The PM's delegate tool calls this; recording here captures the
    /// sub-agent turn for the transcript.
    /// What: Forwards to `inner.run`; on `Ok`, records `(agent, task, output)`.
    /// Test: `orchestrator_records_delegation`.
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        let out = self.inner.run(agent_name, task).await?;
        self.record(agent_name, task, &out);
        Ok(out)
    }

    /// Context-carrying variant: forward and record identically.
    ///
    /// Why: The orchestrator may pin a model/turn cap per delegation; this keeps
    /// recording consistent across both entry points.
    /// What: Forwards to `inner.run_with_context`; records on `Ok`.
    /// Test: covered indirectly by the recording path.
    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        let out = self.inner.run_with_context(agent_name, task, ctx).await?;
        self.record(agent_name, task, &out);
        Ok(out)
    }
}

/// [`RegistryFactory`] that assembles each sub-agent's tool set.
///
/// Why: The in-process runner is policy-free about tool construction (#1029); the
/// orchestrator owns *how* a sub-agent's tools are wired. For the metaharness,
/// every sub-agent gets fs/bash scoped to the project working directory plus a
/// nested `delegate_to_agent` (so deeper delegation is possible). Reusing the
/// same construction the PM uses keeps the capability surface consistent.
/// What: Holds the project dir, the recording runner (for the nested delegate),
/// and the agents config dir. `build` returns the *ungated* registry; the runner
/// then narrows it to each agent's `tools.allowed`.
/// Test: `orchestrator_runs_full_delegation_cycle` (the engineer's `write_file`
/// must run, proving the factory wired it).
pub(crate) struct MetaRegistryFactory {
    project: PathBuf,
    runner: Arc<dyn AgentRunner>,
    config_dir: PathBuf,
}

impl MetaRegistryFactory {
    /// Construct the factory from its collaborators.
    ///
    /// Why: Constructor injection lets the orchestrator share one project dir,
    /// one nested-delegate runner, and one config dir across every sub-agent.
    /// What: Stores the three inputs.
    /// Test: `orchestrator_runs_full_delegation_cycle`.
    pub(crate) fn new(
        project: impl Into<PathBuf>,
        runner: Arc<dyn AgentRunner>,
        config_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            project: project.into(),
            runner,
            config_dir: config_dir.into(),
        }
    }
}

#[async_trait]
impl RegistryFactory for MetaRegistryFactory {
    /// Build the ungated tool registry offered to `_agent` under `_ctx`.
    ///
    /// Why: Centralising tool assembly here means a single factory serves every
    /// sub-agent; the runner applies each agent's `tools.allowed` afterwards.
    /// What: Registers fs tools (`read_file`/`write_file`/`edit`) scoped to the
    /// project dir, a default `bash`, and a nested `delegate_to_agent` backed by
    /// the shared recording runner (validated against the config dir).
    /// Test: `orchestrator_runs_full_delegation_cycle`.
    async fn build(&self, _agent: &AgentConfig, _ctx: &RunContext) -> Arc<ToolRegistry> {
        Arc::new(build_agent_registry(
            &self.project,
            Arc::clone(&self.runner),
            &self.config_dir,
        ))
    }
}

/// Build the shared tool registry (fs/bash + delegate) scoped to `project`.
///
/// Why: Both the PM and every sub-agent are offered the same capability set; one
/// builder keeps them identical and reuses the WI-2 tool wiring concept.
/// What: Registers `read_file`/`write_file`/`edit` (scoped to `project`), a
/// default `bash`, and a `delegate_to_agent` backed by `runner` and validated
/// against `config_dir`.
/// Test: exercised via `Orchestrator::run` in `orchestrator_runs_full_delegation_cycle`.
fn build_agent_registry(
    project: &Path,
    runner: Arc<dyn AgentRunner>,
    config_dir: &Path,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool::new(project)));
    registry.register(Arc::new(WriteFileTool::new(project)));
    registry.register(Arc::new(EditTool::new(project)));
    registry.register(Arc::new(BashTool::default_config()));
    registry.register(Arc::new(
        DelegateToAgentTool::new(runner).with_config_dir(config_dir.to_path_buf()),
    ));
    registry
}

/// Wires the live PM loop + in-process sub-agent runner for one run.
///
/// Why: This is the #1030 orchestration entry point — it loads the PM config,
/// builds the PM's registry (delegate + read tools, gated by the PM's
/// `tools.allowed`), runs the PM via [`AgentLoop`], and returns the combined
/// transcript. A single shared LLM client threads through the PM and the runner
/// so usage rolls up.
/// What: Holds the shared client, the agents config dir, the project working dir,
/// the loop budget, and optional project context. [`run`] executes one delegation
/// cycle and returns a [`MetaTranscript`].
/// Test: `orchestrator::tests`.
///
/// [`run`]: Orchestrator::run
pub(crate) struct Orchestrator {
    llm: Arc<dyn LlmClientTrait>,
    config_dir: PathBuf,
    project: PathBuf,
    config: InProcessRunnerConfig,
    project_context: Option<String>,
}

impl Orchestrator {
    /// Construct an orchestrator from its collaborators.
    ///
    /// Why: Constructor injection keeps the orchestrator testable — production
    /// passes a real `LlmClient`; tests pass a scripted mock — and lets the same
    /// client be shared with the runner for usage rollup.
    /// What: Stores the shared client, config dir, project dir, and a default
    /// loop budget; project context starts `None`.
    /// Test: `orchestrator_runs_full_delegation_cycle`.
    pub(crate) fn new(
        llm: Arc<dyn LlmClientTrait>,
        config_dir: impl Into<PathBuf>,
        project: impl Into<PathBuf>,
    ) -> Self {
        Self {
            llm,
            config_dir: config_dir.into(),
            project: project.into(),
            config: InProcessRunnerConfig::default(),
            project_context: None,
        }
    }

    /// Attach project `CLAUDE.md` context injected into every assembled prompt.
    ///
    /// Why: Both the PM and sub-agents should see the same project rules
    /// (parity-spec); threading it here injects it into every prompt.
    /// What: Stores `context`.
    /// Test: `orchestrator_runs_full_delegation_cycle` (context-free path); the
    /// runner's own tests cover context propagation to sub-agents.
    pub(crate) fn with_project_context(mut self, context: impl Into<String>) -> Self {
        self.project_context = Some(context.into());
        self
    }

    /// Override the default loop budget (turn cap + timeout).
    ///
    /// Why: Tests want a tight, deterministic budget; deployments may want more.
    /// What: Replaces `self.config`.
    /// Test: `orchestrator_runs_full_delegation_cycle` uses a small budget.
    pub(crate) fn with_config(mut self, config: InProcessRunnerConfig) -> Self {
        self.config = config;
        self
    }

    /// Run one PM → sub-agent delegation cycle and return the combined transcript.
    ///
    /// Why: This is the orchestration deliverable — load the PM, run its loop
    /// (which delegates once to the engineer through the shared in-process
    /// runner), then assemble the combined transcript with both turns, the usage
    /// rollup, and the engineer's file artifacts.
    /// What: Loads `pm.toml`, builds the live recording runner + registry factory,
    /// constructs the PM's gated registry, resolves the PM model + assembled
    /// prompt, drives an `AgentLoop`, then folds the PM output and every recorded
    /// delegation into a [`MetaTranscript`] (artifacts captured by diffing the
    /// project tree against a pre-run snapshot).
    /// Test: `orchestrator_runs_full_delegation_cycle`.
    pub(crate) async fn run(&self, task: &str) -> Result<MetaTranscript> {
        let pm_path = self.config_dir.join(format!("{PM_AGENT_NAME}.toml"));
        let pm_config = AgentConfig::load(&pm_path)
            .with_context(|| format!("failed to load PM config: {}", pm_path.display()))?;

        // Snapshot the project tree so we can attribute new/changed files to the run.
        let before = snapshot_files(&self.project);

        // The live in-process runner the PM delegates through; wrapped so the
        // orchestrator can record each sub-agent turn for the transcript.
        let records: Arc<Mutex<Vec<DelegationRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let real_runner: Arc<dyn AgentRunner> = Arc::new(
            InProcessAgentRunner::new(
                Arc::clone(&self.llm),
                self.registry_factory(records.clone()),
                self.config_dir.clone(),
            )
            .with_config(self.config.clone())
            .maybe_with_project_context(self.project_context.clone()),
        );
        let recording: Arc<dyn AgentRunner> =
            Arc::new(RecordingRunner::new(real_runner, records.clone()));

        // The PM's own registry: fs/bash + a delegate tool dispatching to the
        // recording runner. The PM's `tools.allowed` then gates it down to
        // delegate + read.
        let full_pm_registry =
            build_agent_registry(&self.project, Arc::clone(&recording), &self.config_dir);
        let pm_registry = gate_registry(full_pm_registry, &pm_config);

        let model = resolve_model(&pm_config, None);
        let system = assemble_system_prompt(&pm_config, self.project_context.as_deref(), None);
        let loop_config = AgentLoopConfig {
            max_turns: self.config.max_turns,
            timeout_secs: self.config.timeout_secs,
            model: model.clone(),
        };

        let pm_loop = AgentLoop::new(loop_config, Arc::clone(&self.llm), Arc::new(pm_registry));
        let pm_output = pm_loop
            .run(&system, task)
            .await
            .context("PM agent loop failed")?;

        // Assemble the combined transcript from the PM output + recorded
        // delegations + the file artifacts the run produced.
        let pm_turn = AgentTurn::from_pm_output(&pm_output);
        let delegations = drain_delegations(&records);
        let artifacts = new_artifacts(&self.project, &before);
        Ok(MetaTranscript::assemble(
            model,
            task,
            pm_turn,
            delegations,
            artifacts,
        ))
    }

    /// Build the registry factory sharing `records` with the orchestrator.
    ///
    /// Why: The factory needs the recording runner so nested delegations are also
    /// captured; sharing `records` keeps every turn in one log.
    /// What: Returns a `MetaRegistryFactory` whose nested delegate dispatches to a
    /// recording runner wrapping a fresh in-process runner.
    /// Test: exercised via `run`.
    fn registry_factory(
        &self,
        records: Arc<Mutex<Vec<DelegationRecord>>>,
    ) -> Arc<dyn RegistryFactory> {
        // Sub-agents delegate (if at all) through their own recording runner so
        // nested turns also land in the transcript. For the demo the engineer
        // does not delegate further, but wiring it keeps the seam complete.
        let nested_real: Arc<dyn AgentRunner> = Arc::new(
            InProcessAgentRunner::new(
                Arc::clone(&self.llm),
                Arc::new(NullRegistryFactory),
                self.config_dir.clone(),
            )
            .with_config(self.config.clone()),
        );
        let nested: Arc<dyn AgentRunner> = Arc::new(RecordingRunner::new(nested_real, records));
        Arc::new(MetaRegistryFactory::new(
            self.project.clone(),
            nested,
            self.config_dir.clone(),
        ))
    }
}

/// A `RegistryFactory` that yields an empty registry.
///
/// Why: The nested delegate runner needs *some* factory, but a sub-agent that
/// delegates further would itself need tools; for the bounded demo an empty set
/// is sufficient and avoids unbounded recursion in factory construction.
/// What: `build` returns an empty `Arc<ToolRegistry>`.
/// Test: covered by `orchestrator_runs_full_delegation_cycle` (no nested
/// delegation occurs, so the empty registry is never narrowed).
///
/// NOTE (known limitation): in this bounded demo, a sub-agent that delegates
/// *further* (a nested sub-agent) gets this empty tool set — nested delegation is
/// wired for completeness but is not expected to occur. `build` therefore emits a
/// `tracing::warn!` if it is ever actually invoked, so the limitation surfaces in
/// the logs rather than silently handing a nested agent zero tools.
struct NullRegistryFactory;

#[async_trait]
impl RegistryFactory for NullRegistryFactory {
    async fn build(&self, agent: &AgentConfig, _ctx: &RunContext) -> Arc<ToolRegistry> {
        tracing::warn!(
            agent = %agent.agent.name,
            "metaharness: nested sub-agent delegation is unsupported in this bounded demo; \
             handing the nested agent an empty tool set"
        );
        Arc::new(ToolRegistry::new())
    }
}

/// Narrow `registry` to `config.tools.allowed`, if present.
///
/// Why: The PM's registry must be gated the same way the runner gates sub-agents
/// — the PM may only call the tools in its allowlist (delegate + read).
/// What: If `tools.allowed` is `Some`, returns `registry.gated(list)`; else
/// returns it unchanged.
/// Test: exercised via `run` (the PM cannot call `write_file`).
fn gate_registry(registry: ToolRegistry, config: &AgentConfig) -> ToolRegistry {
    match config.tools.as_ref().and_then(|t| t.allowed.as_ref()) {
        Some(allowed) => registry.gated(allowed),
        None => registry,
    }
}

/// Drain the recorded delegations into transcript turns.
///
/// Why: The transcript needs the sub-agent turns in delegation order; draining
/// the shared log yields exactly those, once.
/// What: Locks the log, maps each `DelegationRecord` to an `AgentTurn`, and
/// returns them. A poisoned lock yields an empty list (best-effort telemetry) but
/// first emits a `tracing::warn!` (to stderr) so the lost turns are visible.
/// Test: `orchestrator_runs_full_delegation_cycle`.
fn drain_delegations(records: &Arc<Mutex<Vec<DelegationRecord>>>) -> Vec<AgentTurn> {
    let Ok(log) = records.lock() else {
        tracing::warn!(
            "metaharness: delegation-record lock poisoned; transcript will omit all recorded delegations"
        );
        return Vec::new();
    };
    log.iter()
        .map(|r| AgentTurn::for_delegation(&r.agent, &r.task, &r.output))
        .collect()
}

/// Snapshot the relative paths of every file under `dir` (recursively).
///
/// Why: To attribute new/changed files to the run we compare the post-run tree
/// against this pre-run snapshot.
/// What: Walks `dir`, collecting each file's path relative to `dir`. IO errors
/// yield an empty set (the run still proceeds; artifacts are best-effort).
/// Test: `snapshot_then_new_artifacts_detects_created_file`.
fn snapshot_files(dir: &Path) -> std::collections::BTreeSet<PathBuf> {
    let mut out = std::collections::BTreeSet::new();
    collect_files(dir, dir, &mut out);
    out
}

/// Recursive helper for [`snapshot_files`].
///
/// Why: Directory walking needs a recursive worker that records paths relative to
/// the snapshot root — and must not follow symlinks, which would risk infinite
/// recursion (and a stack overflow) on a symlink cycle, and would also record a
/// symlink's target as if it were a run artifact.
/// What: For each entry under `current`, classifies it with `entry.file_type()`
/// (which does *not* traverse symlinks, unlike `Path::is_dir`/`is_file`): recurses
/// only into real directories and records only real files' paths relative to
/// `root`. Symlinks (and entries whose type is unreadable) are skipped, so a
/// symlink cycle can never drive the recursion.
/// Test: via `snapshot_files` in `snapshot_then_new_artifacts_detects_created_file`
/// and `collect_files_skips_symlinks` (symlink-skip / no-cycle-recursion).
fn collect_files(root: &Path, current: &Path, out: &mut std::collections::BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        // `file_type()` here reflects the entry itself (it does not follow
        // symlinks), so a symlink — even one pointing at a directory or forming a
        // cycle — is neither recursed into nor recorded.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_files(root, &path, out);
        } else if file_type.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.insert(rel.to_path_buf());
        }
    }
}

/// List files that appear after the run but were absent from `before`.
///
/// Why: The transcript must surface the engineer's file changes; newly created
/// files are the auditable evidence of the delegation's side effects.
/// What: Snapshots `dir` again, keeps paths not in `before`, and builds an
/// [`Artifact`] (relative path + byte length) for each, sorted by path.
/// Test: `snapshot_then_new_artifacts_detects_created_file`.
// TODO: also detect modified (not just new) files via mtime/size — the current
// snapshot diff only surfaces files that did not exist before the run, so an
// in-place edit of a pre-existing file is invisible in the transcript.
fn new_artifacts(dir: &Path, before: &std::collections::BTreeSet<PathBuf>) -> Vec<Artifact> {
    let after = snapshot_files(dir);
    after
        .into_iter()
        .filter(|p| !before.contains(p))
        .map(|rel| {
            let bytes = std::fs::metadata(dir.join(&rel))
                .map(|m| m.len())
                .unwrap_or(0);
            Artifact {
                path: rel.to_string_lossy().into_owned(),
                bytes,
            }
        })
        .collect()
}

/// Internal extension: optionally attach project context to the runner.
///
/// Why: `InProcessAgentRunner::with_project_context` takes a value, but the
/// orchestrator holds an `Option`; this keeps the call site branch-free.
/// What: If `Some`, calls `with_project_context`; else returns the runner as-is.
/// Test: exercised via `Orchestrator::run`.
trait MaybeProjectContext: Sized {
    fn maybe_with_project_context(self, ctx: Option<String>) -> Self;
}

impl MaybeProjectContext for InProcessAgentRunner {
    fn maybe_with_project_context(self, ctx: Option<String>) -> Self {
        match ctx {
            Some(c) => self.with_project_context(c),
            None => self,
        }
    }
}

#[cfg(test)]
mod tests;
