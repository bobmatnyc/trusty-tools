//! Parallel phase execution — one sub-agent per subtask, concurrent (#73).
//!
//! Why: Some phases can be split into independent workstreams (backend +
//! frontend + tests). Running them in parallel cuts wall-clock time and
//! lets each agent focus on a narrow slice of the task.
//! What: `run_parallel_phase` spawns one `AgentRunner::run` call per
//! `ParallelSubtask` via `tokio::spawn`, collects the results (partial
//! results are OK — failed sub-agents are logged and skipped), and returns
//! the per-subtask outputs for downstream merging. When `use_worktrees` is
//! true, each sub-agent gets its own git worktree via `WorktreeManager`.
//! Test: `run_parallel_phase_collects_results` with a mock runner.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tools::traits::{AgentOutput, AgentRunner};
use crate::workflow::config::ParallelSubtask;
use crate::workflow::worktree::WorktreeManager;

/// Result from one parallel sub-agent invocation.
pub struct ParallelPhaseResult {
    pub label: String,
    pub output: AgentOutput,
    /// Directory where this sub-agent's files were intended to land.
    /// Used by the conflict resolver to collect per-agent file trees.
    pub out_dir: PathBuf,
}

/// Run one sub-agent per subtask concurrently.
///
/// Why: Concurrent execution cuts wall-clock time for independent workstreams.
/// Partial results are acceptable — a single sub-agent failure is logged and
/// skipped so the remaining workstreams still contribute.
/// What: For each `ParallelSubtask`, constructs `task = base_task + "\n\n" +
/// task_suffix`, picks an out_dir (either a worktree via `WorktreeManager`
/// or a plain `base_out_dir/<label>`), and spawns a tokio task that calls
/// `runner.run(agent_name, task)`. Awaits all handles and returns one
/// `ParallelPhaseResult` per successful sub-agent. Worktrees (if used) are
/// removed after collection.
/// Test: See unit test with MockAgentRunner below.
pub async fn run_parallel_phase(
    base_task: &str,
    subtasks: &[ParallelSubtask],
    agent_name: &str,
    runner: Arc<dyn AgentRunner>,
    base_out_dir: &Path,
    use_worktrees: bool,
) -> anyhow::Result<Vec<ParallelPhaseResult>> {
    // Place worktrees alongside the out_dir rather than inside it — otherwise
    // the conflict resolver would pick up `.trusty-agents/state/worktrees/...` as
    // "produced files".
    let worktree_parent = base_out_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| base_out_dir.to_path_buf());
    let worktree_mgr = WorktreeManager::new(worktree_parent.join(".trusty-agents/state/worktrees"));

    let mut handles = Vec::with_capacity(subtasks.len());

    for subtask in subtasks {
        let task_text = format!("{}\n\n{}", base_task, subtask.task_suffix);
        let label = subtask.label.clone();

        let out_dir = if use_worktrees {
            match worktree_mgr.create(&label).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        label = %label,
                        error = %e,
                        "worktree creation failed; using plain subdir"
                    );
                    base_out_dir.join(&label)
                }
            }
        } else {
            base_out_dir.join(&label)
        };
        tokio::fs::create_dir_all(&out_dir).await?;

        let agent_name = agent_name.to_string();
        let runner = runner.clone();
        let od = out_dir.clone();
        let lbl = label.clone();

        handles.push(tokio::spawn(async move {
            let result = runner.run(&agent_name, &task_text).await;
            (lbl, result, od)
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok((label, Ok(output), out_dir)) => {
                results.push(ParallelPhaseResult {
                    label,
                    output,
                    out_dir,
                });
            }
            Ok((label, Err(e), _)) => {
                tracing::warn!(
                    label = %label,
                    error = %e,
                    "parallel sub-agent failed; continuing with remaining results"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "parallel sub-agent task join failed");
            }
        }
    }

    // Cleanup worktrees after collection. Failures here are non-fatal — the
    // agent output has already been captured.
    if use_worktrees {
        for r in &results {
            let _ = worktree_mgr.remove(&r.out_dir).await;
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    use anyhow::Result;
    use async_trait::async_trait;

    use crate::perf::TokenUsage;
    use crate::session::HistoryMessage;

    struct MockRunner;

    #[async_trait]
    impl AgentRunner for MockRunner {
        async fn run(&self, _agent_name: &str, task: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: format!("echo:{task}"),
                summary: None,
                usage: TokenUsage::default(),
            })
        }

        async fn run_with_history(
            &self,
            agent_name: &str,
            task: &str,
            _history: &[HistoryMessage],
            _ctx: &crate::tools::traits::RunContext,
        ) -> Result<AgentOutput> {
            self.run(agent_name, task).await
        }
    }

    #[tokio::test]
    async fn run_parallel_phase_collects_results() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        let subs = vec![
            ParallelSubtask {
                label: "a".to_string(),
                task_suffix: "extra-a".to_string(),
            },
            ParallelSubtask {
                label: "b".to_string(),
                task_suffix: "extra-b".to_string(),
            },
        ];
        let runner: Arc<dyn AgentRunner> = Arc::new(MockRunner);
        let results = run_parallel_phase(
            "base-task",
            &subs,
            "mock-agent",
            runner,
            &base,
            /* use_worktrees = */ false,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 2);
        let labels: Vec<_> = results.iter().map(|r| r.label.clone()).collect();
        assert!(labels.contains(&"a".to_string()));
        assert!(labels.contains(&"b".to_string()));
        // Each sub-agent should have gotten base_task + suffix.
        for r in &results {
            assert!(r.output.content.contains("base-task"));
            assert!(r.output.content.contains(&format!("extra-{}", r.label)));
        }
    }

    #[tokio::test]
    async fn run_parallel_phase_creates_per_label_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        let subs = vec![ParallelSubtask {
            label: "only".to_string(),
            task_suffix: "x".to_string(),
        }];
        let runner: Arc<dyn AgentRunner> = Arc::new(MockRunner);
        let _ = run_parallel_phase("t", &subs, "a", runner, &base, false)
            .await
            .unwrap();
        assert!(base.join("only").exists());
    }
}
