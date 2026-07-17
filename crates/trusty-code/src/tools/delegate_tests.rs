//! Unit tests for `tools::delegate` — extracted to a sibling test file so
//! the production module stays under the 500-SLOC cap while the tests live
//! under the 1500-SLOC test cap (mirrors `goals_tests.rs`, #2683).

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use super::{DelegateToAgentTool, EngineerCompletionSignal, redelegation_hint};
use crate::agent_loop::AgentLoopError;
use crate::llm::LlmError;
use crate::runner::RunnerError;
use crate::tools::finish_task::FinishStatus;
use crate::tools::traits::{AgentOutput, AgentRunner, ToolExecutor};

/// Recording mock runner.
///
/// Why: Tests need to verify the runner was (or was not) invoked.
/// What: Records `(agent_name, task)` pairs in a Mutex-guarded Vec.
/// Test: `known_agent_reaches_runner`, `no_config_dir_skips_validation`.
struct RecordingRunner {
    invoked: std::sync::Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl AgentRunner for RecordingRunner {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        self.invoked
            .lock()
            .expect("lock poisoned")
            .push((agent_name.to_string(), task.to_string()));
        Ok(AgentOutput::from_content("ok"))
    }
}

/// An unknown agent name returns a structured error listing available agents,
/// and the runner is NOT invoked.
///
/// Why: Guards against hallucinated agent names causing confusing IO errors.
/// What: Calls `execute({"agent_name":"ghost",...})` with a tempdir
/// containing only `engineer.toml`; expects error with "ghost" and "engineer".
/// Test: This test.
#[tokio::test]
async fn unknown_agent_returns_helpful_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("engineer.toml"),
        "[agent]\nname = \"engineer\"\n",
    )
    .expect("write");
    std::fs::write(
        tmp.path().join("qa-agent.toml"),
        "[agent]\nname = \"qa-agent\"\n",
    )
    .expect("write");

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone()).with_config_dir(tmp.path().to_path_buf());

    let result = tool
        .execute(json!({"agent_name": "ghost", "task": "do something"}))
        .await;

    assert!(result.is_error(), "must reject unknown agent");
    let msg = result.content();
    assert!(
        msg.contains("Unknown agent 'ghost'"),
        "error must name the unknown agent, got: {msg}"
    );
    assert!(
        msg.contains("engineer") && msg.contains("qa-agent"),
        "error must list available agents, got: {msg}"
    );
    assert!(
        msg.contains("native tools"),
        "error must clarify native-vs-agent, got: {msg}"
    );
    assert!(
        runner.invoked.lock().expect("lock").is_empty(),
        "runner must not be called when validation fails"
    );
}

/// A valid agent name passes validation and reaches the runner.
///
/// Why: Verify the happy-path dispatch contract.
/// What: Register `engineer.toml`, dispatch with `agent_name="engineer"`.
/// Test: This test.
#[tokio::test]
async fn known_agent_reaches_runner() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("engineer.toml"),
        "[agent]\nname = \"engineer\"\n",
    )
    .expect("write");

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone()).with_config_dir(tmp.path().to_path_buf());

    let result = tool
        .execute(json!({"agent_name": "engineer", "task": "do the thing"}))
        .await;

    assert!(
        !result.is_error(),
        "valid agent should succeed: {}",
        result.content()
    );
    let invoked = runner.invoked.lock().expect("lock");
    assert_eq!(invoked.len(), 1, "runner should be called exactly once");
    assert_eq!(invoked[0].0, "engineer");
}

/// Without `with_config_dir`, validation is skipped — the runner is invoked.
///
/// Why: Preserves backward compatibility with callers that don't need
/// pre-flight validation (e.g. integration tests).
/// What: Construct tool without config_dir; any agent name reaches the runner.
/// Test: This test.
#[tokio::test]
async fn no_config_dir_skips_validation() {
    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone());

    let result = tool
        .execute(json!({"agent_name": "anything-goes", "task": "do it"}))
        .await;

    assert!(!result.is_error(), "legacy mode should bypass validation");
    assert_eq!(runner.invoked.lock().expect("lock").len(), 1);
}

/// Missing `agent_name` field returns a structured error.
///
/// Why: Guard against malformed LLM function call arguments.
/// What: `execute({})` without `agent_name` key.
/// Test: This test.
#[tokio::test]
async fn missing_agent_name_returns_error() {
    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner);
    let result = tool.execute(json!({"task": "something"})).await;
    assert!(result.is_error());
    assert!(result.content().contains("missing 'agent_name'"));
}

/// A path-traversal agent name is rejected before any filesystem access.
///
/// Why: The LLM supplies `agent_name` which is joined into a filesystem path.
/// A crafted name like `../../etc/passwd` must be caught before the join to
/// prevent escaping the agents config directory.
/// What: Calls `execute` with a traversal string; asserts error and runner
/// not invoked.
/// Test: This test.
#[tokio::test]
async fn path_traversal_agent_name_is_rejected() {
    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    // No config_dir — traversal guard fires before config check.
    let tool = DelegateToAgentTool::new(runner.clone());

    for bad_name in &[
        "../../etc/passwd",
        "../sibling",
        "agent/sub",
        "agent name",
        "",
    ] {
        let result = tool
            .execute(json!({"agent_name": bad_name, "task": "do it"}))
            .await;
        assert!(
            result.is_error(),
            "traversal/invalid name '{bad_name}' must be rejected"
        );
        assert!(
            result.content().contains("Invalid agent name"),
            "error must describe the problem, got: {}",
            result.content()
        );
    }
    assert!(
        runner.invoked.lock().expect("lock").is_empty(),
        "runner must never be called for invalid names"
    );
}

/// A valid agent name passes the sanitisation guard and reaches the runner.
///
/// Why: Confirms the allowlist does not over-reject legitimate names.
/// What: `execute({"agent_name":"engineer",...})` without config_dir; runner
/// is called once.
/// Test: This test.
#[tokio::test]
async fn valid_agent_name_passes_sanitization() {
    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone());

    for good_name in &["engineer", "qa-agent", "python_engineer", "rust-2024"] {
        let result = tool
            .execute(json!({"agent_name": good_name, "task": "do it"}))
            .await;
        assert!(
            !result.is_error(),
            "valid name '{good_name}' must be accepted: {}",
            result.content()
        );
    }
    assert_eq!(
        runner.invoked.lock().expect("lock").len(),
        4,
        "runner must be called for each valid name"
    );
}

/// Mock runner that always fails with a caller-supplied error.
///
/// Why: `redelegation_hint` tests need to control exactly which
/// `RunnerError`/`AgentLoopError` shape `execute()` observes, without
/// driving a real `AgentLoop`.
/// What: `run` always returns `Err` built from the stored `anyhow::Error`
/// factory (a `Fn` so each call gets a fresh error, since `anyhow::Error`
/// is not `Clone`).
/// Test: `redelegation_hint_present_on_turn_cap_exceeded` and siblings.
struct FailingRunner<F: Fn() -> anyhow::Error + Send + Sync> {
    make_err: F,
}

#[async_trait]
impl<F: Fn() -> anyhow::Error + Send + Sync> AgentRunner for FailingRunner<F> {
    async fn run(&self, _agent_name: &str, _task: &str) -> Result<AgentOutput> {
        Err((self.make_err)())
    }
}

/// A `TurnCapExceeded` sub-agent failure carries the reuse/continue
/// directive in the tool's error text.
///
/// Why: This is the core bake-off L1 fix — the PM must see an automatic,
/// structured instruction to inspect and reuse partial work instead of
/// relying on its own free text remembering to ask for it.
/// What: `FailingRunner` returns `RunnerError::Loop` wrapping
/// `AgentLoopError::TurnCapExceeded`; assert the rendered error mentions
/// both re-delegation and reading/continuing existing work.
/// Test: this test.
#[tokio::test]
async fn redelegation_hint_present_on_turn_cap_exceeded() {
    let runner = Arc::new(FailingRunner {
        make_err: || {
            anyhow::Error::from(RunnerError::Loop {
                name: "engineer".to_string(),
                source: AgentLoopError::TurnCapExceeded {
                    max_turns: 8,
                    partial: Box::new(AgentOutput::from_content("partial work")),
                },
            })
        },
    });
    let tool = DelegateToAgentTool::new(runner);

    let result = tool
        .execute(json!({"agent_name": "engineer", "task": "build the package"}))
        .await;

    assert!(result.is_error());
    let msg = result.content();
    assert!(
        msg.contains("READ and CONTINUE"),
        "must instruct the PM to reuse existing work, got: {msg}"
    );
    assert!(
        msg.contains("already written real, partial progress"),
        "must explain why re-delegation should not start over, got: {msg}"
    );
}

/// A `Timeout` sub-agent failure gets the same reuse/continue directive.
///
/// Why: A wall-clock timeout carries partial work exactly like a turn-cap
/// abort (per `AgentLoopError`'s own docs); the hint must not be
/// TurnCapExceeded-specific.
/// What: `FailingRunner` returns `RunnerError::Loop` wrapping
/// `AgentLoopError::Timeout`.
/// Test: this test.
#[tokio::test]
async fn redelegation_hint_present_on_timeout() {
    let runner = Arc::new(FailingRunner {
        make_err: || {
            anyhow::Error::from(RunnerError::Loop {
                name: "engineer".to_string(),
                source: AgentLoopError::Timeout {
                    timeout_secs: 120,
                    partial: Box::new(AgentOutput::from_content("partial work")),
                },
            })
        },
    });
    let tool = DelegateToAgentTool::new(runner);

    let result = tool
        .execute(json!({"agent_name": "engineer", "task": "build the package"}))
        .await;

    assert!(result.is_error());
    assert!(result.content().contains("READ and CONTINUE"));
}

/// A `Cancelled` sub-agent failure gets the same reuse/continue directive.
///
/// What: `FailingRunner` returns `RunnerError::Loop` wrapping
/// `AgentLoopError::Cancelled`.
/// Test: this test.
#[tokio::test]
async fn redelegation_hint_present_on_cancelled() {
    let runner = Arc::new(FailingRunner {
        make_err: || {
            anyhow::Error::from(RunnerError::Loop {
                name: "engineer".to_string(),
                source: AgentLoopError::Cancelled {
                    partial: Box::new(AgentOutput::from_content("partial work")),
                },
            })
        },
    });
    let tool = DelegateToAgentTool::new(runner);

    let result = tool
        .execute(json!({"agent_name": "engineer", "task": "build the package"}))
        .await;

    assert!(result.is_error());
    assert!(result.content().contains("READ and CONTINUE"));
}

/// An `UnknownAgent` runner failure gets NO reuse directive — there is no
/// partial work to reuse when the agent config never resolved.
///
/// Why: The hint must not fire indiscriminately on every failure; guard
/// the negative case.
/// What: `FailingRunner` returns `RunnerError::UnknownAgent`.
/// Test: this test.
#[tokio::test]
async fn redelegation_hint_absent_on_unknown_agent() {
    let runner = Arc::new(FailingRunner {
        make_err: || {
            anyhow::Error::from(RunnerError::UnknownAgent {
                name: "ghost".to_string(),
                dir: std::path::PathBuf::from("/agents"),
            })
        },
    });
    let tool = DelegateToAgentTool::new(runner);

    let result = tool
        .execute(json!({"agent_name": "ghost", "task": "build the package"}))
        .await;

    assert!(result.is_error());
    assert!(
        !result.content().contains("READ and CONTINUE"),
        "no partial work exists for an unresolved agent config, got: {}",
        result.content()
    );
}

/// A plain LLM/transport failure gets the SAME reuse/continue directive
/// (#2265 fix #2: this is the dominant bake-off failure mode, previously
/// excluded from the hint).
///
/// Why: A recoverable Bedrock/transport error aborting the engineer's
/// sub-loop is the dominant failure mode observed in the bake-off
/// transcripts; the reuse hint must fire here too so re-delegation checks
/// for and continues from files already on disk instead of restarting
/// exploration from scratch.
/// What: `FailingRunner` returns `RunnerError::Loop` wrapping
/// `AgentLoopError::Llm`; assert the hint is present.
/// Test: this test.
#[tokio::test]
async fn redelegation_hint_present_on_llm_error() {
    let runner = Arc::new(FailingRunner {
        make_err: || {
            anyhow::Error::from(RunnerError::Loop {
                name: "engineer".to_string(),
                source: AgentLoopError::Llm(LlmError::ApiError {
                    status: 500,
                    body: "internal error".to_string(),
                }),
            })
        },
    });
    let tool = DelegateToAgentTool::new(runner);

    let result = tool
        .execute(json!({"agent_name": "engineer", "task": "build the package"}))
        .await;

    assert!(result.is_error());
    assert!(
        result.content().contains("READ and CONTINUE"),
        "an LLM/transport error must ALSO carry the reuse directive (#2265), got: {}",
        result.content()
    );
}

/// `redelegation_hint` itself (unit-level, not through the tool) returns
/// `Some` for `AgentLoopError::Llm` (#2265 fix #2).
///
/// Why: The tool-level test above proves the end-to-end wiring; this test
/// pins the specific function contract directly, matching the task's
/// required "redelegation_hint(AgentLoopError::Llm) now returns
/// Some(<reuse hint>)" assertion.
/// What: Builds a `RunnerError::Loop` wrapping `AgentLoopError::Llm`,
/// calls `redelegation_hint` directly, asserts `Some`.
/// Test: this test.
#[test]
fn redelegation_hint_fn_returns_some_for_llm_error() {
    let err = anyhow::Error::from(RunnerError::Loop {
        name: "engineer".to_string(),
        source: AgentLoopError::Llm(LlmError::ApiError {
            status: 503,
            body: "bedrock throttled".to_string(),
        }),
    });
    assert!(
        redelegation_hint(&err).is_some(),
        "redelegation_hint must return Some for AgentLoopError::Llm (#2265)"
    );
}

/// Mock runner returning an `AgentOutput` with a caller-chosen
/// `finish_status`, recording whether it was invoked (#2683).
///
/// Why: The completion-signal tests need to control the exact
/// `finish_status` the delegate tool observes on a successful delegation,
/// and the refusal test needs to prove the runner is NOT invoked once the
/// signal has latched.
/// What: `run` records the invocation and returns an `AgentOutput` carrying
/// `finish_status`.
/// Test: `completion_signal_latches_on_completed_finish` and siblings.
struct FinishingRunner {
    finish_status: Option<FinishStatus>,
    invoked: std::sync::Mutex<u32>,
}

#[async_trait]
impl AgentRunner for FinishingRunner {
    async fn run(&self, _agent_name: &str, _task: &str) -> Result<AgentOutput> {
        *self.invoked.lock().expect("invoked lock") += 1;
        let mut out = AgentOutput::from_content("engineer finished");
        out.finish_status = self.finish_status;
        Ok(out)
    }
}

/// A delegation whose engineer returned `finish_status == Completed`
/// latches the shared completion signal (#2683).
///
/// Why: This is the authoritative "task is genuinely done" signal that both
/// the re-delegation refusal and `run_task::assemble_report`'s
/// success-not-partial mapping key off; it must latch on an explicit
/// successful completion.
/// What: Run the tool once with a `Completed` finish; assert the signal is
/// latched afterwards.
/// Test: this test.
#[tokio::test]
async fn completion_signal_latches_on_completed_finish() {
    let runner = Arc::new(FinishingRunner {
        finish_status: Some(FinishStatus::Completed),
        invoked: std::sync::Mutex::new(0),
    });
    let signal = EngineerCompletionSignal::new();
    let tool = DelegateToAgentTool::new(runner).with_completion_signal(signal.clone());

    let result = tool
        .execute(json!({"agent_name": "engineer", "task": "build it"}))
        .await;

    assert!(!result.is_error(), "a completed delegation must succeed");
    assert!(
        signal.is_completed(),
        "a Completed finish_task must latch the completion signal"
    );
}

/// A delegation that ended any other way (a `failed`/`cancelled` finish, or
/// no explicit finish at all) does NOT latch the completion signal (#2683).
///
/// Why: Re-delegation must stay legitimately available when the engineer did
/// not actually succeed — latching on a non-success would wrongly suppress a
/// warranted retry and could report a failed run as a success.
/// What: Run the tool with a `Failed` finish and, separately, with no
/// finish status; assert the signal stays un-latched in both cases.
/// Test: this test.
#[tokio::test]
async fn completion_signal_ignores_non_completed_finish() {
    for status in [
        Some(FinishStatus::Failed),
        Some(FinishStatus::Cancelled),
        None,
    ] {
        let runner = Arc::new(FinishingRunner {
            finish_status: status,
            invoked: std::sync::Mutex::new(0),
        });
        let signal = EngineerCompletionSignal::new();
        let tool = DelegateToAgentTool::new(runner).with_completion_signal(signal.clone());

        let _ = tool
            .execute(json!({"agent_name": "engineer", "task": "attempt it"}))
            .await;

        assert!(
            !signal.is_completed(),
            "finish_status {status:?} must not latch the completion signal"
        );
    }
}

/// Once the completion signal has latched, a further `delegate_to_agent`
/// call is refused with a recoverable error and the runner is NOT invoked
/// (#2683).
///
/// Why: This is the "do not re-delegate once the finish gate is satisfied"
/// half of the fix — the gratuitous post-finish re-verify round that
/// mislabels a complete run as `partial` must never reach the engineer at
/// all.
/// What: Pre-latch the signal, then call `execute`; assert the result is a
/// recoverable error naming `finish_task`, and the runner's invocation
/// count stays zero.
/// Test: this test.
#[tokio::test]
async fn delegate_refused_once_engineer_completed() {
    let runner = Arc::new(FinishingRunner {
        finish_status: Some(FinishStatus::Completed),
        invoked: std::sync::Mutex::new(0),
    });
    let signal = EngineerCompletionSignal::new();
    signal.mark_completed();
    let tool = DelegateToAgentTool::new(runner.clone()).with_completion_signal(signal);

    let result = tool
        .execute(json!({"agent_name": "engineer", "task": "re-verify the work"}))
        .await;

    assert!(
        result.is_error(),
        "a post-completion delegation must be refused"
    );
    assert!(
        !result.is_fatal(),
        "the refusal must be recoverable, not fatal"
    );
    let msg = result.content();
    assert!(
        msg.contains("refused") && msg.contains("finish_task"),
        "the refusal must nudge the PM to finish_task, got: {msg}"
    );
    assert_eq!(
        *runner.invoked.lock().expect("invoked lock"),
        0,
        "the runner must never be invoked once the engineer has completed"
    );
}

/// The #2683 completion-latch refusal fires a WARN-level log naming the
/// mechanism (#2857).
///
/// Why: This is the exact site #2857 names as unobserved — the refusal
/// silently drops a delegation attempt the model made without ever
/// invoking the runner; an operator reading stderr must be able to tell
/// this happened.
/// What: Same latch-then-refuse scenario as
/// `delegate_refused_once_engineer_completed`, captured via
/// `crate::test_support::begin_capture`/`captured_at_least`.
/// Test: this test.
#[tokio::test]
async fn delegate_refusal_logs_warn() {
    crate::test_support::begin_capture();

    let runner = Arc::new(FinishingRunner {
        finish_status: Some(FinishStatus::Completed),
        invoked: std::sync::Mutex::new(0),
    });
    let signal = EngineerCompletionSignal::new();
    signal.mark_completed();
    let tool = DelegateToAgentTool::new(runner).with_completion_signal(signal);

    tool.execute(json!({"agent_name": "engineer", "task": "re-verify the work"}))
        .await;

    let captured = crate::test_support::captured_at_least(tracing::Level::WARN);
    assert!(
        captured.iter().any(|m| m.contains("completion latch")),
        "expected a warn-level refusal log, got: {captured:?}"
    );
}
