//! Tests for `BashTool`.
//!
//! Why: Collects all bash tool tests in a dedicated file so `bash/mod.rs`
//! stays under the 500-line cap while preserving full coverage.
//! What: Fast-path, error-path, timeout, cwd, truncation, registry, and
//! RBAC tier tests.
//! Test: Each `#[tokio::test]` / `#[test]` function is its own test case.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::BashTool;
use crate::tools::registry::ToolRegistry;
use crate::tools::traits::ToolExecutor;

/// A fast command (echo) returns captured stdout and exit 0.
///
/// Why: Basic smoke test for the happy path.
/// What: Runs `echo hello`; asserts stdout contains "hello" and exit 0.
/// Test: This test.
#[tokio::test]
async fn fast_command_exit_zero() {
    let tool = BashTool::default_config();
    let result = tool.execute(json!({"command": "echo hello"})).await;
    assert!(
        !result.is_error(),
        "echo should succeed: {}",
        result.content()
    );
    let content = result.content();
    assert!(content.contains("hello"), "stdout must contain 'hello'");
    assert!(content.contains("exit_code: 0"), "exit code must be 0");
}

/// A failing command surfaces the non-zero exit code as a recoverable result.
///
/// Why: The LLM loop must read pytest failures, not abort on them. A non-zero
/// exit must come back as `ToolResult::ok` with the code embedded in the
/// output so the model can read and react.
/// What: Runs `false`; asserts result is not an error variant and content
/// contains "exit_code: 1".
/// Test: This test.
#[tokio::test]
async fn nonzero_exit_is_recoverable() {
    let tool = BashTool::default_config();
    let result = tool.execute(json!({"command": "false"})).await;
    assert!(
        !result.is_error(),
        "non-zero exit must be recoverable ToolResult::ok, not ToolResult::Error"
    );
    assert!(
        result.content().contains("exit_code: 1"),
        "exit code 1 must appear in content, got: {}",
        result.content()
    );
}

/// A custom non-zero exit code is captured correctly.
///
/// Why: Validates that arbitrary exit codes propagate correctly.
/// What: Runs `sh -c 'exit 42'`; asserts content contains "exit_code: 42".
/// Test: This test.
#[tokio::test]
async fn custom_exit_code_is_captured() {
    let tool = BashTool::default_config();
    let result = tool.execute(json!({"command": "sh -c 'exit 42'"})).await;
    assert!(!result.is_error());
    assert!(
        result.content().contains("exit_code: 42"),
        "exit_code 42 must appear, got: {}",
        result.content()
    );
}

/// A sleeping command with a sub-second timeout is killed promptly.
///
/// Why: Without kill logic, a leaked child blocks the test suite.
/// What: Runs `sleep 30` with a 1-second timeout; asserts the call returns
/// well under the sleep duration and the result is an error (timeout).
/// Test: This test.
#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_child() {
    let tool = BashTool::new(None, Duration::from_secs(120));
    let start = Instant::now();
    let result = tool
        .execute(json!({"command": "sleep 30", "timeout_secs": 1}))
        .await;
    let elapsed = start.elapsed();

    assert!(
        result.is_error(),
        "timed-out command must return an error variant"
    );
    assert!(
        result.content().contains("timed out"),
        "error must mention timeout, got: {}",
        result.content()
    );
    // Should return well within the 30-second sleep duration.
    assert!(
        elapsed < Duration::from_secs(10),
        "timeout must fire in under 10 s, took {}ms",
        elapsed.as_millis()
    );
}

/// The working directory is honored for spawned commands.
///
/// Why: The coder loop needs `pwd` to reflect the project root, not the
/// tcode binary's current directory.
/// What: Creates a tempdir, sets it as `working_dir`, runs `pwd`, asserts
/// the output matches.
/// Test: This test.
#[tokio::test]
async fn cwd_is_honored() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let canonical = tmp.path().canonicalize().expect("canonicalize");
    let tool = BashTool::new(Some(canonical.clone()), Duration::from_secs(10));
    let result = tool.execute(json!({"command": "pwd"})).await;
    assert!(
        !result.is_error(),
        "pwd should succeed: {}",
        result.content()
    );
    let content = result.content();
    assert!(
        content.contains(canonical.to_string_lossy().as_ref()),
        "pwd output must contain the tempdir path, got: {content}"
    );
}

/// stdout output exceeding MAX_OUTPUT_BYTES is truncated with a notice.
///
/// Why: Unbounded output would exhaust memory and produce an unusably large
/// LLM context window entry.
/// What: Generates > 100 KB of stdout; asserts the content contains the
/// truncation notice.
/// Test: This test.
#[tokio::test]
async fn stdout_truncation() {
    let tool = BashTool::new(None, Duration::from_secs(30));
    // Generate ~110 KB of output (each `yes` line is 2 bytes "y\n").
    // head -c limits to a byte count so we stay deterministic.
    let result = tool
        .execute(json!({"command": "yes | head -c 110000"}))
        .await;
    assert!(
        !result.is_error(),
        "yes|head should succeed: {}",
        result.content()
    );
    let content = result.content();
    assert!(
        content.contains("truncated"),
        "output must include truncation notice, got length={}",
        content.len()
    );
}

/// Missing `command` argument returns a structured error.
///
/// Why: Guard against malformed LLM function calls.
/// What: `execute({})` without `command`; expects error.
/// Test: This test.
#[tokio::test]
async fn missing_command_returns_error() {
    let tool = BashTool::default_config();
    let result = tool.execute(json!({})).await;
    assert!(result.is_error());
    assert!(
        result.content().contains("missing required 'command'"),
        "got: {}",
        result.content()
    );
}

/// Empty `command` string is rejected with a structured error.
///
/// Why: An empty command would spawn `sh -c ''` which silently succeeds but
/// does nothing useful; better to surface an error.
/// What: `execute({"command": "   "})` returns error.
/// Test: This test.
#[tokio::test]
async fn empty_command_returns_error() {
    let tool = BashTool::default_config();
    let result = tool.execute(json!({"command": "   "})).await;
    assert!(result.is_error());
    assert!(
        result.content().contains("must not be empty"),
        "got: {}",
        result.content()
    );
}

/// The bash tool registers in a `ToolRegistry` and is callable via `dispatch`.
///
/// Why: End-to-end guard that the registry plumbing works and the schema
/// roundtrip is correct.
/// What: Registers `BashTool`; asserts `contains("bash")` and `schemas()`
/// returns one entry; dispatches `echo ok` and checks output.
/// Test: This test.
#[tokio::test]
async fn registry_registration_and_dispatch() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(BashTool::default_config()));

    assert!(reg.contains("bash"), "registry must know about 'bash'");
    assert_eq!(reg.schemas().len(), 1, "exactly one schema");

    let schema_val = reg.schemas().into_iter().next().expect("schema");
    let fn_name = schema_val
        .pointer("/function/name")
        .and_then(Value::as_str)
        .expect("function.name");
    assert_eq!(fn_name, "bash");

    let result = reg
        .dispatch("bash", json!({"command": "echo registry-ok"}))
        .await;
    assert!(
        !result.is_error(),
        "dispatch must succeed: {}",
        result.content()
    );
    assert!(result.content().contains("registry-ok"));
}

/// `restricted_tiers` includes `ReadOnly` and `Analytics`.
///
/// Why: The RBAC check at `dispatch_for_user` reads `restricted_tiers()`; we
/// must guarantee the bash tool is never accessible to low-trust callers.
/// What: Constructs `BashTool` and checks the returned slice.
/// Test: This test.
#[test]
fn restricted_tiers_includes_readonly_and_analytics() {
    use crate::tools::traits::ServiceTier;
    let tool = BashTool::default_config();
    let tiers = tool.restricted_tiers();
    assert!(
        tiers.contains(&ServiceTier::ReadOnly),
        "ReadOnly must be restricted"
    );
    assert!(
        tiers.contains(&ServiceTier::Analytics),
        "Analytics must be restricted"
    );
}
