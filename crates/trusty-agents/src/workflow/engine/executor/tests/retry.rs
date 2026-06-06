//! Wave-file retry, model elevation, and divergent-dir reconcile tests
//! (#231, #222).
//!
//! Why: Pins the error-recovery contracts — transient 5xx errors retry while
//! fatal 4xx errors don't, base-model exhaustion elevates to a stronger model
//! once, and reconciliation keeps artifacts (`out_dir`) separate from generated
//! code (`code_dir`).
//! What: Exercises the engine's wave loop end-to-end plus the `retry` wave-file
//! helper and the `helpers` reconcile-against function directly.
//! Test: This file IS the test body.

use super::*;

use crate::workflow::engine::retry::run_wave_file_with_retry;

/// A transient 5xx error on the first call must be retried; success on the
/// second call must be returned to the wave loop.
///
/// Why: The original code returned the first error immediately; this test
/// pins the new retry contract.
/// What: Mock runner fails once with "status 500 internal server error"
/// then succeeds; assert the final result is Ok and that two calls were
/// made.
/// Test: this function.
#[tokio::test]
async fn wave_loop_retries_on_transient_error() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TransientMock {
        calls: AtomicU32,
        out_dir: PathBuf,
    }

    #[async_trait]
    impl AgentRunner for TransientMock {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            unimplemented!("wave loop uses run_with_context")
        }

        async fn run_with_context(
            &self,
            _agent_name: &str,
            _task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First call: simulate a transient 5xx.
                return Err(anyhow::anyhow!(
                    "API returned status 500: internal server error"
                ));
            }
            // Second call: succeed and write the file.
            if let Some(path) = &ctx.assigned_file {
                let dest = self.out_dir.join(path);
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                tokio::fs::write(&dest, b"# ok\n").await.ok();
            }
            Ok(AgentOutput {
                content: "success after retry".into(),
                summary: Some("ok".into()),
                usage: TokenUsage::default(),
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();

    let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/module.py","stub":null,"purpose":"main module"}
                    ]
                }
            ]
        }"#;
    tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
        .await
        .unwrap();

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_json = r#"{
            "name":"retry-test",
            "description":"retry on 5xx",
            "phases":[{"name":"code","agent":"eng","context_template":"{{task}}"}]
        }"#;
    tokio::fs::write(workflows_dir.join("retry-test.json"), wf_json)
        .await
        .unwrap();

    let mock = Arc::new(TransientMock {
        calls: AtomicU32::new(0),
        out_dir: out_dir.clone(),
    });
    let call_count = &mock.calls as *const AtomicU32;
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    let result = engine
        .run("retry-test", "do it".into(), Some(out_dir.clone()))
        .await;

    assert!(result.is_ok(), "expected Ok after retry, got {result:?}");
    // SAFETY: mock outlives this assertion (it's in the Arc in the engine).
    let total_calls = unsafe { (*call_count).load(Ordering::SeqCst) };
    assert_eq!(total_calls, 2, "expected 2 calls (1 fail + 1 success)");
}

/// A fatal (non-retryable) error must NOT be retried; the wave loop must
/// fail immediately after the first call.
///
/// Why: Retrying 4xx errors wastes time and could mask misconfiguration.
/// What: Mock runner always returns "status 400 bad request"; assert
/// exactly one call is made and the error propagates.
/// Test: this function.
#[tokio::test]
async fn wave_loop_does_not_retry_fatal_error() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FatalMock {
        calls: AtomicU32,
    }

    #[async_trait]
    impl AgentRunner for FatalMock {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            unimplemented!()
        }

        async fn run_with_context(&self, _: &str, _: &str, _: &RunContext) -> Result<AgentOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("status 400 bad request: invalid prompt"))
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();

    let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/fail.py","stub":null,"purpose":"will fail"}
                    ]
                }
            ]
        }"#;
    tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
        .await
        .unwrap();

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_json = r#"{
            "name":"fatal-test",
            "description":"no retry on 4xx",
            "phases":[{"name":"code","agent":"eng","context_template":"{{task}}"}]
        }"#;
    tokio::fs::write(workflows_dir.join("fatal-test.json"), wf_json)
        .await
        .unwrap();

    let mock = Arc::new(FatalMock {
        calls: AtomicU32::new(0),
    });
    let call_count = &mock.calls as *const AtomicU32;
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    let result = engine
        .run("fatal-test", "do it".into(), Some(out_dir.clone()))
        .await;

    assert!(result.is_err(), "expected Err for fatal error");
    let total_calls = unsafe { (*call_count).load(Ordering::SeqCst) };
    assert_eq!(total_calls, 1, "fatal error must not be retried");
}

/// #231: After all transient retries on the base model fail, the wave
/// loop must make ONE more attempt using the elevation model.
///
/// Why: Engineer agents start on Sonnet for cost; some hard files require
/// Opus. Elevation lets the harness automatically retry on a stronger
/// model after repeated failures rather than requiring operator
/// intervention.
/// What: Mock runner always returns transient 5xx. With
/// `elevation_threshold=2` and `elevation_model="claude-opus-4-6"`, after
/// MAX_WAVE_RETRIES+1 base-model attempts fail, the runner must be called
/// once more with `ctx.model = Some("claude-opus-4-6")`.
/// Test: this function.
#[tokio::test]
async fn elevation_triggers_after_n_failures() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct ElevatingMock {
        calls: AtomicU32,
        seen_models: Mutex<Vec<Option<String>>>,
    }

    #[async_trait]
    impl AgentRunner for ElevatingMock {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            unimplemented!("elevation test uses run_with_context")
        }

        async fn run_with_context(
            &self,
            _agent_name: &str,
            _task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_models.lock().unwrap().push(ctx.model.clone());
            // If the runner sees the elevated model, succeed; otherwise
            // simulate transient 5xx errors so the retry loop exhausts.
            if ctx.model.as_deref() == Some("claude-opus-4-6") {
                Ok(AgentOutput {
                    content: "elevated success".into(),
                    summary: Some("ok".into()),
                    usage: TokenUsage::default(),
                })
            } else {
                Err(anyhow::anyhow!(
                    "API returned status 503: service unavailable"
                ))
            }
        }
    }

    let mock = ElevatingMock {
        calls: AtomicU32::new(0),
        seen_models: Mutex::new(Vec::new()),
    };

    let ctx = RunContext::default();
    // No sleep needed — but the retry helper does sleep with exponential
    // backoff. Use tokio's pause/auto-advance to keep the test fast.
    tokio::time::pause();
    let handle = tokio::spawn(async move {
        let result = run_wave_file_with_retry(
            &mock,
            "engineer",
            "build it",
            &ctx,
            Some(2),
            Some("claude-opus-4-6"),
        )
        .await;
        (result, mock)
    });
    // Auto-advance virtual time so the backoff sleeps complete instantly.
    // Loop a few times; total of 6s of virtual time covers 2s + 4s backoffs.
    for _ in 0..10 {
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
    }
    let (result, mock) = handle.await.unwrap();

    assert!(
        result.is_ok(),
        "expected elevation to succeed, got {result:?}"
    );
    let total = mock.calls.load(Ordering::SeqCst);
    // 3 base-model attempts (initial + 2 retries) + 1 elevated retry = 4
    assert_eq!(total, 4, "expected 3 base + 1 elevated call, got {total}");

    let seen = mock.seen_models.lock().unwrap();
    assert_eq!(
        seen.len(),
        4,
        "expected 4 recorded model overrides, got {}",
        seen.len()
    );
    // First three calls: no override (base model).
    assert!(seen[0].is_none() && seen[1].is_none() && seen[2].is_none());
    // Final call: elevated.
    assert_eq!(seen[3].as_deref(), Some("claude-opus-4-6"));
}

/// #222: When `assignments_dir` and `code_target` diverge, the
/// reconcile step must read the manifest from `assignments_dir` and
/// move misrouted files into `code_target` (the user's project tree),
/// not back into `assignments_dir`.
///
/// Why: This locks in the invariant that `--out-dir` (artifacts) and
/// `--project-dir` (code) stay separated. A regression that confused
/// the two would silently put generated source files back under
/// `out/` — exactly the bug #222 tracks.
/// What: Plants a divergent layout (project_root, code_target,
/// assignments_dir all distinct), with one file misrouted at
/// project_root, and asserts the reconciler relocates it to
/// `code_target`, NOT `assignments_dir`.
/// Test: This test.
#[tokio::test]
async fn reconcile_code_outputs_against_divergent_dirs() {
    // Note: `reconcile_code_outputs_against` reads CWD as project_root.
    // To keep the test deterministic we exercise the divergence by
    // writing the stray file at the real CWD's relative location and
    // then cleaning up. This mirrors what the wave-loop sees in
    // production where claude-code anchors writes at the git repo root.
    let tmp = tempfile::tempdir().unwrap();
    let assignments_dir = tmp.path().join("artifacts");
    let code_target = tmp.path().join("project");
    tokio::fs::create_dir_all(&assignments_dir).await.unwrap();
    tokio::fs::create_dir_all(&code_target).await.unwrap();

    // Sanity: divergent paths.
    assert_ne!(assignments_dir, code_target);

    // assignments.json with one declared file under a uniquely-named
    // subdirectory so this test can't collide with anything actually
    // present in the test runner's CWD.
    let unique = format!("oss_222_test_{}", uuid::Uuid::new_v4().simple());
    let rel_path = format!("{unique}/foo.py");
    let asg_body = format!(
        r#"{{"error_convention":"exceptions","waves":[{{"wave":1,"files":[{{"path":"{rel_path}","stub":null,"purpose":"test","depends_on":[],"max_lines":100}}]}}]}}"#,
        rel_path = rel_path
    );
    tokio::fs::write(assignments_dir.join("assignments.json"), asg_body)
        .await
        .unwrap();

    // Plant the misrouted file at the real CWD (= project_root for
    // `reconcile_code_outputs_against`). Use a unique subdirectory so
    // the test is isolated; clean up regardless of outcome.
    let cwd = std::env::current_dir().unwrap();
    let stray = cwd.join(&rel_path);
    tokio::fs::create_dir_all(stray.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&stray, b"# misrouted").await.unwrap();

    // Act.
    let result = reconcile_code_outputs_against(&assignments_dir, &code_target).await;

    // Cleanup unique subdir at CWD regardless of outcome.
    let cleanup_dir = cwd.join(&unique);
    let _ = tokio::fs::remove_dir_all(&cleanup_dir).await;

    result.expect("reconciliation should succeed");

    // Assert: file landed in `code_target`, NOT `assignments_dir`.
    let code_path = code_target.join(&rel_path);
    let artifacts_path = assignments_dir.join(&rel_path);
    assert!(
        code_path.is_file(),
        "#222: file should be in code_target (project dir), got missing at {}",
        code_path.display()
    );
    assert!(
        !artifacts_path.exists(),
        "#222: file must NOT land in assignments_dir (out_dir / artifacts)"
    );
}
