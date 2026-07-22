//! `trusty-search doctor` check pipeline, decomposed into a `DoctorCheck`
//! trait so each diagnostic is an independently-testable unit.
//!
//! Why: the original `run_doctor_checks` was an inline orchestrator with
//! cyclomatic complexity 48 — hard to test, hard to extend, and hard to read.
//! Decomposing into a `Vec<Box<dyn DoctorCheck>>` driven by a single loop
//! drops the orchestrator's CC to ~3, makes each check unit-testable, and
//! lets new checks be added by implementing one trait method.
//! What: defines the [`DoctorCheck`] trait, a shared [`DoctorState`] passed
//! to every check, and the concrete check structs that wrap the pure helpers
//! in `doctor_checks`. The orchestrator [`run_doctor_checks`] walks the
//! trait-object list and aggregates results.
//! Test: `cargo test --workspace` exercises the existing doctor integration
//! tests; `cargo run -- doctor` produces byte-identical output to the
//! pre-refactor implementation.

use super::daemon_utils::daemon_base_url;
use super::doctor_checks::{
    check_daemon_running, check_data_dir, check_lock_file, check_log_rotation, check_model_cache,
    check_port_reachable, check_python_device_note, check_python_launcher, check_python_uv,
    check_python_venv, doctor_data_dir, fetch_index_names, fetch_index_statuses,
    print_index_breakdown, probe_daemon_health, python_embedder_enabled, read_daemon_port,
    summarize_indexes, CheckResult, EmptyIndex,
};
use async_trait::async_trait;
use std::sync::Mutex;

// ── Trait + shared state ──────────────────────────────────────────────────

#[async_trait]
pub(crate) trait DoctorCheck: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &str;

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult>;
}

pub(crate) struct DoctorState {
    pub client: reqwest::Client,
    pub base: String,
    pub port: u16,
    pub data_dir: std::path::PathBuf,
    daemon_running: Mutex<bool>,
    daemon_version: Mutex<String>,
    empty_indexes: Mutex<Vec<EmptyIndex>>,
}

impl DoctorState {
    fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base: daemon_base_url(),
            port: read_daemon_port(),
            data_dir: doctor_data_dir(),
            daemon_running: Mutex::new(false),
            daemon_version: Mutex::new(String::new()),
            empty_indexes: Mutex::new(Vec::new()),
        }
    }

    fn set_daemon_health(&self, running: bool, version: String) {
        *self.daemon_running.lock().expect("doctor state poisoned") = running;
        *self.daemon_version.lock().expect("doctor state poisoned") = version;
    }

    fn daemon_running(&self) -> bool {
        *self.daemon_running.lock().expect("doctor state poisoned")
    }

    #[allow(dead_code)]
    fn daemon_version(&self) -> String {
        self.daemon_version
            .lock()
            .expect("doctor state poisoned")
            .clone()
    }

    fn push_empty_indexes(&self, mut items: Vec<EmptyIndex>) {
        self.empty_indexes
            .lock()
            .expect("doctor state poisoned")
            .append(&mut items);
    }

    fn take_empty_indexes(&self) -> Vec<EmptyIndex> {
        std::mem::take(&mut *self.empty_indexes.lock().expect("doctor state poisoned"))
    }
}

// ── Concrete checks ───────────────────────────────────────────────────────

pub(crate) struct DaemonHealthCheck;

#[async_trait]
impl DoctorCheck for DaemonHealthCheck {
    fn name(&self) -> &str {
        "daemon_health"
    }

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult> {
        let (running, version) = probe_daemon_health(&state.client, &state.base).await;
        state.set_daemon_health(running, version.clone());
        vec![check_daemon_running(running, &state.base, &version)]
    }
}

pub(crate) struct ModelCacheCheck;

#[async_trait]
impl DoctorCheck for ModelCacheCheck {
    fn name(&self) -> &str {
        "model_cache"
    }

    async fn run(&self, _state: &DoctorState) -> Vec<CheckResult> {
        vec![check_model_cache()]
    }
}

pub(crate) struct DataDirCheck;

#[async_trait]
impl DoctorCheck for DataDirCheck {
    fn name(&self) -> &str {
        "data_dir"
    }

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult> {
        vec![check_data_dir(&state.data_dir)]
    }
}

pub(crate) struct LockFileCheck;

#[async_trait]
impl DoctorCheck for LockFileCheck {
    fn name(&self) -> &str {
        "lock_file"
    }

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult> {
        vec![check_lock_file(&state.data_dir, state.daemon_running())]
    }
}

pub(crate) struct IndexesCheck;

#[async_trait]
impl DoctorCheck for IndexesCheck {
    fn name(&self) -> &str {
        "indexes"
    }

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult> {
        if !state.daemon_running() {
            return vec![CheckResult::Warn(
                "Indexes: skipped (daemon not running)".into(),
            )];
        }

        let names = fetch_index_names(&state.client, &state.base).await;
        if names.is_empty() {
            return vec![CheckResult::Warn(
                "No indexes registered — run `trusty-search index` to add a project".into(),
            )];
        }

        let per_index = fetch_index_statuses(&state.client, &state.base, &names).await;
        let zero_count = per_index
            .iter()
            .filter(|(_, b)| b.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0) == 0)
            .count();
        let summary = summarize_indexes(per_index.len(), zero_count);

        let mut empty_buf: Vec<EmptyIndex> = Vec::new();
        print_index_breakdown(&per_index, &mut empty_buf);
        state.push_empty_indexes(empty_buf);

        vec![summary]
    }
}

pub(crate) struct PortReachableCheck;

#[async_trait]
impl DoctorCheck for PortReachableCheck {
    fn name(&self) -> &str {
        "port_reachable"
    }

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult> {
        vec![check_port_reachable(state.port).await]
    }
}

pub(crate) struct LogRotationCheck;

#[async_trait]
impl DoctorCheck for LogRotationCheck {
    fn name(&self) -> &str {
        "log_rotation"
    }

    async fn run(&self, _state: &DoctorState) -> Vec<CheckResult> {
        vec![check_log_rotation()]
    }
}

/// Python/MPS embedder sidecar check (epic #3524 slice 5).
///
/// Why: since the sidecar becomes the Apple-Silicon default, operators need
/// a way to diagnose venv/uv/launcher state without tailing daemon logs —
/// mirrors the existing `ModelCacheCheck` shape (a synchronous, read-only
/// probe with no daemon dependency) rather than inventing a new pattern.
/// What: no-ops with a single informational `Ok` when
/// `TRUSTY_EMBEDDER=python` is not set; otherwise runs the four pure checks
/// (`uv`, venv/`.ready`, launcher, device note) from `doctor_checks`.
/// Test: `check_python_venv_*` and `python_embedder_enabled_*` in
/// `doctor_checks/tests.rs` cover the pure logic this wrapper composes;
/// `python_embedder_check_disabled_reports_single_informational_ok` and
/// `python_embedder_check_enabled_aggregates_all_four_checks_in_order` in
/// this module's `tests` cover the wrapper's own no-op-vs-aggregation
/// branching directly (review finding, PR #3560 MEDIUM fix — this doc
/// comment previously claimed that direct coverage without it existing).
pub(crate) struct PythonEmbedderCheck;

#[async_trait]
impl DoctorCheck for PythonEmbedderCheck {
    fn name(&self) -> &str {
        "python_embedder"
    }

    async fn run(&self, _state: &DoctorState) -> Vec<CheckResult> {
        if !python_embedder_enabled() {
            return vec![CheckResult::Ok(
                "Python/MPS embedder: not enabled (set TRUSTY_EMBEDDER=python to opt in)".into(),
            )];
        }

        let mut results = vec![check_python_uv()];
        match trusty_embedderd_py::resolve_layout() {
            Ok(layout) => results.push(check_python_venv(&layout)),
            Err(e) => results.push(CheckResult::Error(format!(
                "Python/MPS embedder: could not resolve venv layout: {e:#}"
            ))),
        }
        results.push(check_python_launcher());
        results.push(check_python_device_note());
        results
    }
}

// ── Orchestrator ──────────────────────────────────────────────────────────

fn default_checks() -> Vec<Box<dyn DoctorCheck>> {
    vec![
        Box::new(DaemonHealthCheck),
        Box::new(ModelCacheCheck),
        Box::new(DataDirCheck),
        Box::new(LockFileCheck),
        Box::new(IndexesCheck),
        Box::new(PortReachableCheck),
        Box::new(LogRotationCheck),
        Box::new(PythonEmbedderCheck),
    ]
}

/// Drive the doctor pipeline and return `(checks, empty_indexes)` for the
/// caller (and `--fix`) to consume.
pub(crate) async fn run_doctor_checks() -> (Vec<CheckResult>, Vec<EmptyIndex>) {
    let client = match trusty_common::server::daemon_http_client() {
        Ok(c) => c,
        Err(e) => {
            return (
                vec![CheckResult::Error(format!(
                    "failed to build HTTP client: {e}"
                ))],
                Vec::new(),
            );
        }
    };

    let state = DoctorState::new(client);
    let mut checks: Vec<CheckResult> = Vec::new();

    for check in default_checks() {
        checks.extend(check.run(&state).await);
    }

    (checks, state.take_empty_indexes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Save/restore a single env var across a test body — mirrors the
    /// identically-named guard in `doctor_checks/tests.rs` (kept as a
    /// separate local copy since the two are in different modules and this
    /// one is only needed by the two tests below).
    struct EnvVarGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }

        fn remove(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// `PythonEmbedderCheck::run` must no-op with a single informational
    /// `Ok` when `TRUSTY_EMBEDDER` is not `"python"` (review finding, PR
    /// #3560 MEDIUM fix — the doc comment claimed this was "exercised by
    /// the doctor integration tests" but no such test existed).
    ///
    /// Why: the four detailed sub-checks (`uv`, venv, launcher, device note)
    /// are only meaningful once the operator has opted into the Python/MPS
    /// sidecar; running them unconditionally would produce spurious warnings
    /// about a backend nobody selected.
    /// What: unsets `TRUSTY_EMBEDDER`, runs the check, asserts exactly one
    /// `CheckResult::Ok` mentioning "not enabled".
    /// Test: this test.
    #[tokio::test]
    #[serial]
    async fn python_embedder_check_disabled_reports_single_informational_ok() {
        let _g = EnvVarGuard::remove("TRUSTY_EMBEDDER");

        let client = reqwest::Client::new();
        let state = DoctorState::new(client);
        let results = PythonEmbedderCheck.run(&state).await;

        assert_eq!(
            results.len(),
            1,
            "disabled path must be a single no-op result, got: {results:?}"
        );
        match &results[0] {
            CheckResult::Ok(msg) => {
                assert!(msg.contains("not enabled"), "unexpected message: {msg}")
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    /// `PythonEmbedderCheck::run` must aggregate all four sub-checks, in
    /// order, when `TRUSTY_EMBEDDER=python` is set (review finding, PR #3560
    /// MEDIUM fix).
    ///
    /// Why: this is the wrapper's own composition logic — the four pure
    /// helpers it calls are already covered individually in
    /// `doctor_checks/tests.rs`, but nothing previously proved `run()` itself
    /// calls all four, in the right order, when enabled.
    /// What: sets `TRUSTY_EMBEDDER=python`, runs the check, asserts exactly 4
    /// results came back. The first (`check_python_uv`) and last
    /// (`check_python_device_note`) sub-checks are deterministic pure
    /// functions regardless of the host's actual `uv`/venv/launcher state, so
    /// their positions are asserted directly; the middle two vary with the
    /// host environment (real `uv`/venv/launcher presence) and are only
    /// checked for count.
    /// Test: this test.
    #[tokio::test]
    #[serial]
    async fn python_embedder_check_enabled_aggregates_all_four_checks_in_order() {
        let _g = EnvVarGuard::set("TRUSTY_EMBEDDER", "python");

        let client = reqwest::Client::new();
        let state = DoctorState::new(client);
        let results = PythonEmbedderCheck.run(&state).await;

        assert_eq!(
            results.len(),
            4,
            "enabled path must aggregate exactly 4 sub-check results (uv, \
             venv/layout, launcher, device note), got: {results:?}"
        );

        // First result is always `check_python_uv()` — deterministic given
        // the host's actual `uv` installation state, but always present.
        assert!(
            matches!(&results[0], CheckResult::Ok(msg) if msg.starts_with("uv:"))
                || matches!(&results[0], CheckResult::Error(msg) if msg.contains("uv not found")),
            "result[0] must be the uv check, got: {:?}",
            results[0]
        );

        // Last result is always `check_python_device_note()` — a pure,
        // env-only formatter with no filesystem/network dependency, so its
        // content is fully deterministic.
        match &results[3] {
            CheckResult::Ok(msg) => assert!(
                msg.contains("Python/MPS device: requested="),
                "unexpected device-note message: {msg}"
            ),
            other => panic!("expected Ok device note, got {other:?}"),
        }
    }
}
