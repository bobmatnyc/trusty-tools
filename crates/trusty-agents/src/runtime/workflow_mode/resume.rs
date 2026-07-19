// Pre-existing clippy warnings across this large binary crate — see
// `workflow_mode/mod.rs`'s header block for the full rationale list.
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! `tagent resume <run-id>` CLI surface (#3062, SPEC-AGENTFW-02 §3.4).
//!
//! Why: A checkpointed run needs the same agent-runner/skills/memory wiring
//! a fresh `--workflow` invocation gets — the resumed phases dispatch real
//! agents, not mocks. This mirrors `run_workflow`'s engine construction
//! (minus task/out_dir/code_dir, which come from the checkpoint itself) and
//! calls `WorkflowEngine::resume_with_perf_and_dirs` instead of
//! `run_with_perf_and_dirs`.
//! What: `run_resume_subcommand` parses `tagent resume [--list] [--json]
//! <run-id>` and either lists resumable run ids (from
//! `checkpoint::list_resumable_runs`) or resumes the given one, printing the
//! narrative (or a `PmResponse` JSON envelope with `--json`, matching
//! `run_workflow`'s `--json` contract).
//! Test: Manual — a killed multi-phase workflow leaves a checkpoint;
//! `tagent resume <id>` continues without re-dispatching completed phases.
//! Engine-level resume logic is unit/integration-tested in
//! `workflow::engine::executor::tests::checkpoint_resume_tests`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use crate::workflow::engine::checkpoint;
use crate::workflow::{ResumeOutcome, WorkflowEngine};
use crate::{api, subprocess, tools};

use super::helpers::{
    build_runner_for_workflow, load_skill_registry, load_tag_skill_registry,
    load_user_memory_suffix, read_current_build_number,
};

/// Handle `tagent resume [--list] [--json] <run-id>`.
///
/// Why: Single entry point so `subcommands::dispatch_subcommands` only needs
/// one call for the `resume` subcommand, matching the `postmortem`/`agents`/
/// `skills` pre-clap dispatch pattern already used for other subcommands
/// that own their own tiny argv grammar.
/// What: `--list` (or no positional arg at all) prints resumable run ids and
/// returns. Otherwise builds a `WorkflowEngine` wired the same way
/// `run_workflow` is, then resumes the named run and prints its result.
/// Test: See module doc.
pub(crate) async fn run_resume_subcommand(args: &[String]) -> Result<()> {
    let json_output = args.iter().any(|a| a == "--json");
    let list_only = args.iter().any(|a| a == "--list");
    let project_dir = crate::usage::project_dir();

    let run_id = args.iter().find(|a| !a.starts_with('-')).cloned();

    if list_only || run_id.is_none() {
        let ids = checkpoint::list_resumable_runs(&project_dir);
        if ids.is_empty() {
            println!(
                "No resumable runs found under {}/.trusty-agents/state/runs/",
                project_dir.display()
            );
        } else {
            println!("Resumable runs:");
            for id in &ids {
                println!("  {id}");
            }
            println!();
            println!("Resume one with: tagent resume <run-id>");
        }
        return Ok(());
    }
    let run_id = run_id.expect("checked above");

    // Peek the checkpoint once up front so the runner + subprocess env can be
    // wired with the checkpoint's workflow name / out_dir / code_dir before
    // constructing the engine — mirrors `run_workflow`'s wiring, just sourced
    // from the journal instead of CLI flags. `resume_with_perf_and_dirs`
    // below loads the checkpoint again internally; a cheap re-read, not worth
    // threading the already-loaded record through the engine API.
    let record = checkpoint::load_checkpoint(&project_dir, &run_id)?;

    let build_num = read_current_build_number().await.unwrap_or(0);
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let agents_config_dir = project_root.join(".trusty-agents").join("agents");
    let subprocess_runner: Arc<dyn tools::AgentRunner> = Arc::new(
        subprocess::SubprocessAgentRunner::new()
            .with_config_dir(Some(agents_config_dir))
            .with_out_dir(Some(record.out_dir.clone()))
            .with_code_dir(Some(record.code_dir.clone()))
            .with_project_dir(Some(record.code_dir.clone())),
    );
    let runner = build_runner_for_workflow(&record.workflow, subprocess_runner.clone()).await?;

    let perf_dir: PathBuf = project_root.join("docs").join("performance");
    let skill_registry = load_skill_registry(&project_root).await;
    let tag_skill_registry = load_tag_skill_registry();
    let user_memory_suffix = load_user_memory_suffix().await;

    let engine = WorkflowEngine::new(runner, PathBuf::from(".trusty-agents/workflows"))
        .with_build(build_num)
        .with_perf_dir(Some(perf_dir))
        .with_skill_registry(Some(skill_registry))
        .with_tag_skill_registry(Some(tag_skill_registry))
        .with_user_memory(user_memory_suffix)
        .with_progress(Some(Arc::new(crate::progress::ProgressReporter::new())));

    match engine.resume_with_perf_and_dirs(&run_id).await? {
        ResumeOutcome::AlreadyDone { run_id } => {
            println!("Run {run_id} already completed — nothing to resume.");
        }
        ResumeOutcome::Resumed {
            ctx,
            perf: perf_record,
            resumed_at_phase,
        } => {
            eprintln!("Resumed run {run_id} at phase '{resumed_at_phase}'.");
            let narrative = ctx
                .phase_outputs
                .get("observe")
                .cloned()
                .or_else(|| ctx.phase_outputs.values().last().cloned())
                .unwrap_or_default();
            if json_output {
                let response = api::builder::build_from_workflow(
                    &ctx,
                    Some(&perf_record),
                    narrative,
                    Some(&record.workflow),
                    Vec::new(),
                );
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else if !narrative.is_empty() {
                println!("{narrative}");
            }
        }
    }

    Ok(())
}
