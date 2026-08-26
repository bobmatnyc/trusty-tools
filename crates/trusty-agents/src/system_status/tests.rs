//! Report-assembly tests for `system_status::gather` (epic #3052).
//!
//! Why: the individual probes are unit-tested in their own submodules
//! (`daemons`, `credentials`, `registry_counts`); this file covers the
//! orchestrator itself — that a bad/absent agent name degrades cleanly and
//! that the full report assembles without panicking or hanging in an
//! environment with no daemons and no MCP config (the live-verify scenario:
//! an empty, non-repo directory).

// Why: `uds_daemon_with_no_socket_reports_up_false` holds
// `crate::test_env::ENV_LOCK` across `.await` points by design — it serializes
// against every other test that mutates process-wide env vars for the duration
// of the body. Matches `daemons.rs`'s own test module, which allows the lint
// for `unresponsive_daemon_times_out_rather_than_hanging` for the same reason.
#![allow(clippy::await_holding_lock)]

use super::*;

/// Why: `gather` must never panic on an unknown/malformed agent name — the
/// tool must still report on daemons/credentials/registries even when the
/// caller passes a bogus `active_agent`.
/// What: calls `gather` with a name that cannot resolve to any TOML/MD
/// agent config and asserts the `tagent` section degrades to `"unknown"`
/// rather than the call failing.
/// Test: itself.
#[tokio::test]
async fn gather_never_panics_for_an_unknown_agent_name() {
    let report = gather("definitely-not-a-real-agent-name-xyz").await;
    assert_eq!(
        report.tagent.active_agent,
        "definitely-not-a-real-agent-name-xyz"
    );
    assert_eq!(report.tagent.model, "unknown");
    assert_eq!(report.tagent.runner, "unknown");
    assert!(!report.tagent.version.is_empty());
    // Four daemons are always reported, up or down.
    assert_eq!(report.daemons.len(), 4);
    // Every known provider has an entry — never empty.
    assert!(!report.credentials.is_empty());
}

/// Why: the report must be JSON-serialisable end to end — this is the
/// contract `tagent system status --json` and the tool's structured path
/// both depend on.
/// Test: itself.
#[tokio::test]
async fn gather_report_serializes_to_json_with_expected_keys() {
    let report = gather("ctrl").await;
    let value = serde_json::to_value(&report).expect("report must serialize");
    for key in [
        "tagent",
        "daemons",
        "mcp_servers",
        "credentials",
        "agent_registry_count",
        "skills_count",
    ] {
        assert!(
            value.get(key).is_some(),
            "expected top-level key {key:?} in {value}"
        );
    }
}

/// Why: this is the regression test for the core "live after /switch and
/// /model" requirement — a session-scoped `/model` override is folded into
/// `persona_cfg.agent.model` by `run_pm_task_with_persona`, then passed
/// straight through to `gather_with_resolved_endpoint`. This test proves the
/// report reflects that override rather than silently re-deriving the
/// on-disk TOML's model, by passing a model string that provably does NOT
/// match whatever `ctrl.toml` actually declares.
/// Test: itself.
#[tokio::test]
async fn gather_with_resolved_endpoint_reflects_override_not_toml() {
    let overridden_model = "test-vendor/definitely-not-the-toml-model-xyz";
    let report = gather_with_resolved_endpoint(
        "ctrl",
        overridden_model.to_string(),
        crate::agents::RunnerKind::Subprocess,
    )
    .await;
    assert_eq!(report.tagent.model, overridden_model);
    assert_eq!(report.tagent.runner, "subprocess");
    assert_eq!(report.tagent.active_agent, "ctrl");

    // Sanity: the plain (non-override) path for the same agent name must
    // NOT report the overridden model — proving the override actually took
    // effect rather than every path coincidentally converging.
    let plain_report = gather("ctrl").await;
    assert_ne!(plain_report.tagent.model, overridden_model);
}

/// Why: `runner_label` is the one place `RunnerKind` gets a string form for
/// this report; a silent drift from the TOML `kebab-case` convention would
/// make the CLI/tool output inconsistent with agent TOML files.
/// Test: itself.
#[test]
fn runner_label_matches_toml_kebab_case_convention() {
    use crate::agents::RunnerKind;
    assert_eq!(runner_label(RunnerKind::Subprocess), "subprocess");
    assert_eq!(runner_label(RunnerKind::Inline), "inline");
    assert_eq!(runner_label(RunnerKind::ClaudeCode), "claude-code");
    assert_eq!(runner_label(RunnerKind::InProcess), "in-process");
}

/// Hold `ENV_LOCK` and `TRUSTY_DATA_DIR_OVERRIDE` for a scope, restoring the
/// previous value on drop.
///
/// Why: `std::env::set_var` is a process-wide mutation and `cargo test` runs
/// this binary's tests in parallel, so a test that sets the override without
/// the crate-wide lock races `daemons::tests::
/// unresponsive_daemon_times_out_rather_than_hanging`, which sets the SAME var
/// under that lock. Restoring in `Drop` rather than inline also means a failed
/// assertion cannot leak a tempdir path into every sibling test in the binary.
/// What: takes `crate::test_env::ENV_LOCK`, records the prior value, sets the
/// new one, and puts the prior value back (or removes the var) on drop — the
/// lock releases only after the restore.
/// Test: `uds_daemon_with_no_socket_reports_up_false` is its only user.
struct DataDirOverride {
    prev: Option<std::ffi::OsString>,
    // Dropped last (declaration order), so the lock outlives the restore.
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl DataDirOverride {
    fn set(dir: &std::path::Path) -> Self {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV);
        // SAFETY: ENV_LOCK is held for this guard's whole lifetime, so no
        // sibling test reads or writes this var concurrently.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, dir);
        }
        Self { prev, _lock: lock }
    }
}

impl Drop for DataDirOverride {
    fn drop(&mut self) {
        // SAFETY: the lock is still held — it drops after this field.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, v),
                None => std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV),
            }
        }
    }
}

/// Why (#6286 review, finding 2): `probe_memory` and `probe_analyze` dialled an
/// address ADR-0032 and #6287 stopped publishing, so both reported their daemon
/// permanently down. The fix derives a socket instead — and a socket nothing is
/// serving must STILL be a clean `up: false` rather than a hang or a panic,
/// which is the contract the HTTP probe held.
/// What: points the data directory at an empty tempdir, so the derived socket
/// path exists nowhere, and asserts both probes report down promptly.
/// Test: itself.
#[tokio::test]
async fn uds_daemon_with_no_socket_reports_up_false() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _override = DataDirOverride::set(tmp.path());

    let started = std::time::Instant::now();
    let memory = super::daemons::probe_memory().await;
    let analyze = super::daemons::probe_analyze().await;

    assert!(!memory.up, "nothing is serving the memory socket");
    assert!(!analyze.up, "nothing is serving the analyze socket");
    assert!(
        memory.version.is_none() && analyze.version.is_none(),
        "a down daemon reports no version"
    );
    assert!(
        started.elapsed() < super::daemons::PROBE_TIMEOUT * 3,
        "a refused dial must not wait out the budget twice over: {:?}",
        started.elapsed()
    );
}
