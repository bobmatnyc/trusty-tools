//! Parallel-phase merge propagation tests (#3671).
//!
//! Why: `dispatch_phase`'s parallel branch used to fold `ConflictResolver
//! ::merge`'s `Err` into a string appended to a success payload — a real I/O
//! fault while assembling the merged output tree (a `create_dir_all`
//! blocked by a colliding non-directory entry, in this test) reported as a
//! successful phase with a partial, nondeterministic tree on disk. These
//! tests pin the fix: a merge I/O failure must fail the phase/workflow, and
//! a clean merge must still succeed exactly as before.
//! What: Drives the full `WorkflowEngine::run` (not just `dispatch_phase`
//! directly) through a single-phase workflow with `parallel_subtasks`, using
//! a mock `AgentRunner` that writes real files into the sub-agent out_dirs
//! `run_parallel_phase` computes, so `ConflictResolver::merge` does genuine
//! filesystem work.
//! Test: This file IS the test body.

use super::*;

/// Mock runner for the parallel-merge tests. Each subtask's agent call
/// writes a fixed file tree directly into `base_out_dir/<label>/...`
/// (mirroring what `run_parallel_phase` expects a real sub-agent to have
/// produced under its assigned out_dir), keyed off the task text so a
/// single mock instance can serve multiple differently-labeled subtasks.
struct ParallelMergeMock {
    base_out_dir: PathBuf,
}

#[async_trait]
impl AgentRunner for ParallelMergeMock {
    async fn run(&self, _agent_name: &str, task: &str) -> Result<AgentOutput> {
        // The task text carries "LABEL:<label>:<relative/file/path>" so this
        // single mock can place each subtask's file(s) without needing the
        // AgentRunner trait to expose the out_dir it will land in.
        for line in task.lines() {
            if let Some(rest) = line.strip_prefix("LABEL:") {
                let mut parts = rest.splitn(2, ':');
                let label = parts.next().unwrap_or_default();
                let rel = parts.next().unwrap_or_default();
                let dest = self.base_out_dir.join(label).join(rel);
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                tokio::fs::write(&dest, b"sub-agent content\n").await.ok();
            }
        }
        Ok(AgentOutput {
            content: "sub-agent done".to_string(),
            summary: None,
            usage: TokenUsage::default(),
        })
    }
}

fn parallel_workflow_json(label_a: &str, suffix_a: &str, label_b: &str, suffix_b: &str) -> String {
    format!(
        r#"{{
            "name": "parallel-merge-test",
            "description": "single parallel phase",
            "phases": [
                {{
                    "name": "build",
                    "agent": "parallel-mock",
                    "context_template": "{{{{task}}}}",
                    "parallel_subtasks": [
                        {{"label": "{label_a}", "task_suffix": "{suffix_a}"}},
                        {{"label": "{label_b}", "task_suffix": "{suffix_b}"}}
                    ]
                }}
            ]
        }}"#
    )
}

/// #3671: A merge I/O fault (here, `create_dir_all` blocked by a
/// pre-existing regular file at the exact path a merged sub-tree needs as a
/// directory) must fail the phase/workflow — not be swallowed into a
/// `"merge failed: ..."` string appended to a successful `AgentOutput`.
#[tokio::test]
async fn parallel_phase_merge_io_failure_fails_the_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("out");
    tokio::fs::create_dir_all(&out_dir).await.unwrap();

    // Block the merge's own `create_dir_all(out_dir/nested)`: a sub-agent's
    // file lands at "nested/leaf.txt" (relative to its own out_dir), so the
    // merge step will try to create `out_dir/nested` as a directory. Placing
    // a plain file there first makes that create_dir_all fail with a real
    // I/O error, distinct from run_parallel_phase's own (unrelated) subtask
    // dir creation, which only ever touches `out_dir/<label>` itself.
    tokio::fs::write(out_dir.join("nested"), b"blocker file, not a dir\n")
        .await
        .unwrap();

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("parallel-merge-test.json");
    tokio::fs::write(
        &wf_path,
        parallel_workflow_json("a", "LABEL:a:nested/leaf.txt", "b", "LABEL:b:other.txt"),
    )
    .await
    .unwrap();

    let mock = Arc::new(ParallelMergeMock {
        base_out_dir: out_dir.clone(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    let result = engine
        .run(
            "parallel-merge-test",
            "base task".into(),
            Some(out_dir.clone()),
        )
        .await;

    let err = result.expect_err(
        "a merge I/O fault must fail the workflow, not be swallowed into a success payload",
    );
    match err {
        WorkflowError::PhaseFailed { phase, source } => {
            assert_eq!(phase, "build");
            // The underlying I/O error is inspectable, not just a substring
            // buried in a success payload's content string.
            let msg = format!("{source:#}");
            assert!(
                !msg.is_empty(),
                "PhaseFailed source should carry the real I/O error"
            );
        }
        other => panic!("expected WorkflowError::PhaseFailed, got: {other:?}"),
    }
}

/// Regression: a clean merge (no conflicts, no I/O faults) must still
/// succeed exactly as before — both sub-agents' content shows up in the
/// aggregated output and the merged files land on disk under `out_dir`.
#[tokio::test]
async fn parallel_phase_clean_merge_still_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("out");
    tokio::fs::create_dir_all(&out_dir).await.unwrap();

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("parallel-merge-test.json");
    tokio::fs::write(
        &wf_path,
        parallel_workflow_json("a", "LABEL:a:a-file.txt", "b", "LABEL:b:b-file.txt"),
    )
    .await
    .unwrap();

    let mock = Arc::new(ParallelMergeMock {
        base_out_dir: out_dir.clone(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    let ctx = engine
        .run(
            "parallel-merge-test",
            "base task".into(),
            Some(out_dir.clone()),
        )
        .await
        .expect("clean parallel merge should still succeed");

    assert!(ctx.phase_outputs.contains_key("build"));
    let build_output = &ctx.phase_outputs["build"];
    assert!(
        build_output.contains("Sub-agent [a]") && build_output.contains("Sub-agent [b]"),
        "merged phase output should include both sub-agents' content: {build_output}"
    );

    // Both disjoint files should have been merged through to disk.
    assert!(
        tokio::fs::try_exists(out_dir.join("a-file.txt"))
            .await
            .unwrap()
    );
    assert!(
        tokio::fs::try_exists(out_dir.join("b-file.txt"))
            .await
            .unwrap()
    );
}
