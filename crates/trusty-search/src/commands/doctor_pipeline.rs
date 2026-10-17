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

use super::doctor_checks::mcp_registration::{
    check_claude_registrations, check_registration, read_codex_registration,
    registered_exe_version, McpRegistration,
};
use super::doctor_checks::{
    check_daemon_running, check_data_dir, check_http_listener, check_lock_file, check_log_rotation,
    check_model_cache, check_python_device_note, check_python_launcher, check_python_uv,
    doctor_data_dir, fetch_index_names, fetch_index_statuses, health_http_addr, health_version,
    is_apple_silicon, layout_check_result, print_index_breakdown, probe_daemon_health,
    python_embedder_selection_for, summarize_indexes, CheckResult, EmptyIndex,
    PythonEmbedderSelection,
};
use super::setup::MCP_SERVER_KEY;
use async_trait::async_trait;
use std::sync::Mutex;
use trusty_search::service::daemon_client::DaemonClient;

// ── Trait + shared state ──────────────────────────────────────────────────

#[async_trait]
pub(crate) trait DoctorCheck: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &str;

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult>;
}

pub(crate) struct DoctorState {
    // #9214: every daemon probe goes over the socket; `location` labels it.
    pub client: DaemonClient,
    pub location: String,
    pub data_dir: std::path::PathBuf,
    daemon_running: Mutex<bool>,
    http_addr: Mutex<Option<String>>,
    daemon_version: Mutex<String>,
    empty_indexes: Mutex<Vec<EmptyIndex>>,
}

impl DoctorState {
    fn new(client: DaemonClient) -> Self {
        Self {
            location: client.socket().display().to_string(),
            client,
            data_dir: doctor_data_dir(),
            daemon_running: Mutex::new(false),
            http_addr: Mutex::new(None),
            daemon_version: Mutex::new(String::new()),
            empty_indexes: Mutex::new(Vec::new()),
        }
    }

    fn set_daemon_health(&self, running: bool, version: String, http_addr: Option<String>) {
        *self.daemon_running.lock().expect("doctor state poisoned") = running;
        *self.daemon_version.lock().expect("doctor state poisoned") = version;
        *self.http_addr.lock().expect("doctor state poisoned") = http_addr;
    }

    fn http_addr(&self) -> Option<String> {
        self.http_addr
            .lock()
            .expect("doctor state poisoned")
            .clone()
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
        let health = probe_daemon_health(&state.client).await;
        let version = health.as_ref().map(health_version).unwrap_or_default();
        let http_addr = health.as_ref().and_then(health_http_addr);
        state.set_daemon_health(health.is_some(), version.clone(), http_addr);
        vec![check_daemon_running(
            health.is_some(),
            &state.location,
            &version,
        )]
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

/// Lists the indexes and records the empty ones for `--fix`.
///
/// A failed list is an Error and a failed status read a Warn; neither feeds
/// `--fix` (#9214).
/// Test: `indexes_check_reports_a_failed_list_and_queues_no_reindex`,
/// `indexes_check_never_reads_a_failed_status_as_empty`.
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

        // #9214: a failed list is reported as such, never "No indexes registered".
        let names = match fetch_index_names(&state.client).await {
            Ok(names) => names,
            Err(e) => return vec![CheckResult::Error(format!("Indexes: list failed: {e}"))],
        };
        if names.is_empty() {
            return vec![CheckResult::Warn(
                "No indexes registered — run `trusty-search index` to add a project".into(),
            )];
        }

        // #9214: a failed status read is reported, never counted as a
        // zero-chunk index (which `--fix` would reindex).
        let mut results = Vec::new();
        let mut per_index = Vec::new();
        for (name, status) in fetch_index_statuses(&state.client, &names).await {
            match status {
                Ok(body) => per_index.push((name, body)),
                Err(e) => results.push(CheckResult::Warn(format!(
                    "Index '{name}': status read failed: {e}"
                ))),
            }
        }
        let zero_count = per_index
            .iter()
            .filter(|(_, b)| b.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0) == 0)
            .count();
        let summary = summarize_indexes(per_index.len(), zero_count);

        let mut empty_buf: Vec<EmptyIndex> = Vec::new();
        print_index_breakdown(&per_index, &mut empty_buf);
        state.push_empty_indexes(empty_buf);

        results.insert(0, summary);
        results
    }
}

pub(crate) struct HttpListenerCheck;

#[async_trait]
impl DoctorCheck for HttpListenerCheck {
    fn name(&self) -> &str {
        "http_listener"
    }

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult> {
        vec![check_http_listener(
            state.daemon_running(),
            state.http_addr().as_deref(),
        )]
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
/// What: no-ops with a single informational `Ok` when the sidecar is not
/// selected; runs the four read-only checks (`uv`, venv/`.ready`, launcher,
/// device note) from `doctor_checks` for `TRUSTY_EMBEDDER=python` and, since
/// #9968, for the Apple Silicon graceful default. `is_apple_silicon` is a
/// field so tests can drive either platform on any host.
/// Test: `check_python_venv_*` and `python_embedder_selection_for_*` in
/// `doctor_checks/tests.rs` cover the pure logic this wrapper composes;
/// `python_embedder_check_disabled_reports_single_informational_ok`,
/// `python_embedder_check_enabled_aggregates_all_four_checks_in_order` and
/// `python_checks_run_under_graceful_default` in this module's `tests` cover
/// the wrapper's own no-op-vs-aggregation branching.
pub(crate) struct PythonEmbedderCheck {
    pub(crate) is_apple_silicon: bool,
}

impl PythonEmbedderCheck {
    /// The check for the platform this binary was built for.
    pub(crate) fn for_this_host() -> Self {
        Self {
            is_apple_silicon: is_apple_silicon(),
        }
    }
}

#[async_trait]
impl DoctorCheck for PythonEmbedderCheck {
    fn name(&self) -> &str {
        "python_embedder"
    }

    async fn run(&self, _state: &DoctorState) -> Vec<CheckResult> {
        // #9968: the graceful default runs these checks too, not only `python`.
        let selection = python_embedder_selection_for(
            std::env::var("TRUSTY_EMBEDDER").ok().as_deref(),
            self.is_apple_silicon,
        );
        if selection == PythonEmbedderSelection::NotSelected {
            return vec![CheckResult::Ok(
                "Python/MPS embedder: not enabled (set TRUSTY_EMBEDDER=python to opt in)".into(),
            )];
        }

        // #9970: under the graceful default the uv and layout rows downgrade
        // to Warn, as the launcher row does.
        vec![
            check_python_uv(selection),
            layout_check_result(trusty_embedderd_py::resolve_layout(), selection),
            check_python_launcher(selection),
            check_python_device_note(),
        ]
    }
}

/// MCP-client registration check (#5264).
///
/// Why: every other doctor check inspects the daemon side, so a `trusty-search`
/// that was running perfectly and a Codex registration that could never reach
/// it produced an all-green report. The registration is the one link in the
/// chain nothing verified.
/// What: reads the Codex registration and Claude Code's two global settings
/// files, and reports each one's executable and version, effective arguments,
/// selected project/index, daemon ownership, and remediation. Per-project
/// Claude settings are deliberately out of scope — see the module doc on
/// [`super::doctor_checks::mcp_registration`].
/// Test: the verdict logic is covered by that module's inline tests; this
/// wrapper's aggregation is covered by
/// `mcp_registration_check_reports_one_result_per_client_file`.
pub(crate) struct McpRegistrationCheck;

#[async_trait]
impl DoctorCheck for McpRegistrationCheck {
    fn name(&self) -> &str {
        "mcp_registration"
    }

    async fn run(&self, state: &DoctorState) -> Vec<CheckResult> {
        let Some(home) = dirs::home_dir() else {
            return vec![CheckResult::Warn(
                "MCP registrations: skipped (no home directory)".into(),
            )];
        };

        let codex = read_codex_registration(&home, MCP_SERVER_KEY);
        let codex_path = trusty_common::codex_config::codex_config_path(&home);
        let mut results = vec![check_registration(
            codex.as_ref(),
            "Codex",
            &codex_path,
            &version_of(codex.as_ref()),
            &state.location,
        )];

        results.extend(check_claude_registrations(
            &home,
            MCP_SERVER_KEY,
            &state.location,
            &version_of,
        ));
        results
    }
}

/// Version of the executable a registration names, or a placeholder when there
/// is no registration to interrogate.
fn version_of(reg: Option<&McpRegistration>) -> String {
    match reg {
        Some(r) => registered_exe_version(&r.command),
        None => "n/a".to_string(),
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
        Box::new(HttpListenerCheck),
        Box::new(LogRotationCheck),
        Box::new(PythonEmbedderCheck::for_this_host()),
        Box::new(McpRegistrationCheck),
    ]
}

/// Drive the doctor pipeline and return `(checks, empty_indexes)` for the
/// caller (and `--fix`) to consume.
pub(crate) async fn run_doctor_checks(client: DaemonClient) -> (Vec<CheckResult>, Vec<EmptyIndex>) {
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
    /// What: unsets `TRUSTY_EMBEDDER` off Apple Silicon (the Linux default,
    /// #9968 AC3), runs the check, asserts exactly one `CheckResult::Ok`
    /// mentioning "not enabled".
    /// Test: this test. Mutation caught: selecting the graceful default off
    /// Apple Silicon returns four rows and fails the length assert.
    #[tokio::test]
    #[serial]
    async fn python_embedder_check_disabled_reports_single_informational_ok() {
        let _g = EnvVarGuard::remove("TRUSTY_EMBEDDER");

        let state = DoctorState::new(DaemonClient::at("/nonexistent/daemon.sock"));
        let check = PythonEmbedderCheck {
            is_apple_silicon: false,
        };
        let results = check.run(&state).await;

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

    /// `McpRegistrationCheck::run` must emit one verdict per client file it
    /// can see, and never fewer than one (#5264).
    ///
    /// Why: the whole defect is a registration nothing inspected. A check that
    /// silently returned an empty vector on a machine with no Codex config
    /// would reproduce that gap exactly — the operator would see no line at
    /// all rather than "absent, run `trusty-search setup`".
    /// What: runs the check against the real `$HOME` (read-only) and asserts
    /// the Codex verdict is always present, and that every result names a
    /// client. The host's actual registration state varies, so the verdict
    /// KIND is deliberately not asserted — only that one exists.
    /// Test: this test.
    #[tokio::test]
    #[serial]
    async fn mcp_registration_check_reports_one_result_per_client_file() {
        let state = DoctorState::new(DaemonClient::at("/nonexistent/daemon.sock"));
        let results = McpRegistrationCheck.run(&state).await;

        assert!(
            !results.is_empty(),
            "the Codex verdict is unconditional, so there is always at least one"
        );
        for r in &results {
            let msg = match r {
                CheckResult::Ok(m) | CheckResult::Warn(m) | CheckResult::Error(m) => m,
            };
            assert!(
                msg.contains("Codex") || msg.contains("Claude Code") || msg.contains("skipped"),
                "every verdict names the client it is about: {msg}"
            );
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
        // #9966: keep the uv/venv lookups off the real data dir, and keep any
        // uv lookup offline even if it regressed into fetching.
        let data = tempfile::tempdir().expect("tempdir");
        let data_path = data.path().to_str().expect("utf-8 tempdir");
        let _d = EnvVarGuard::set("TRUSTY_DATA_DIR_OVERRIDE", data_path);
        let _f = EnvVarGuard::set("TRUSTY_UV_FETCH", "0");

        let state = DoctorState::new(DaemonClient::at("/nonexistent/daemon.sock"));
        let results = PythonEmbedderCheck::for_this_host().run(&state).await;

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

    /// #9968 AC1/AC2: on Apple Silicon with `TRUSTY_EMBEDDER` unset or
    /// `auto`, doctor runs the four python checks, and a missing launcher is
    /// one Warn naming the path searched and the fix command.
    ///
    /// Why: the daemon bootstraps the sidecar under this default, yet doctor
    /// reported "not enabled" and stayed silent about a missing launcher.
    /// What: injects `is_apple_silicon: true` (as `resolve_default_embedder_mode_for`
    /// takes its platform flag), points `TRUSTY_EMBEDDERD_PY_BIN` at a path
    /// that does not exist so the lookup fails on any host, and runs the check
    /// for both spellings of the default.
    /// Test: this test. Mutation caught: a gate that reads only
    /// `TRUSTY_EMBEDDER == "python"` returns the single "not enabled" row;
    /// an `Error` for the missing launcher fails the Warn match.
    #[tokio::test]
    #[serial]
    async fn python_checks_run_under_graceful_default() {
        let data = tempfile::tempdir().expect("tempdir");
        let data_path = data.path().to_str().expect("utf-8 tempdir");
        let _d = EnvVarGuard::set("TRUSTY_DATA_DIR_OVERRIDE", data_path);
        let _f = EnvVarGuard::set("TRUSTY_UV_FETCH", "0");
        let missing = data.path().join("bin").join("trusty-embedderd-py");
        let missing_str = missing.to_str().expect("utf-8 path");
        let _l = EnvVarGuard::set("TRUSTY_EMBEDDERD_PY_BIN", missing_str);

        for value in [None, Some("auto")] {
            let _e = match value {
                Some(v) => EnvVarGuard::set("TRUSTY_EMBEDDER", v),
                None => EnvVarGuard::remove("TRUSTY_EMBEDDER"),
            };
            let state = DoctorState::new(DaemonClient::at("/nonexistent/daemon.sock"));
            let check = PythonEmbedderCheck {
                is_apple_silicon: true,
            };
            let results = check.run(&state).await;

            assert_eq!(
                results.len(),
                4,
                "TRUSTY_EMBEDDER={value:?} on Apple Silicon runs uv, venv, launcher \
                 and device checks, got: {results:?}"
            );
            match &results[2] {
                CheckResult::Warn(msg) => assert!(
                    msg.contains(missing_str)
                        && msg.contains("tctl install trusty-search")
                        && msg.contains("cargo install trusty-search --locked --force"),
                    "the launcher Warn names the searched path and the fix: {msg}"
                ),
                other => panic!("a missing launcher is a Warn, got {other:?}"),
            }
        }
    }

    /// #9970: a bad `TRUSTY_UV_BIN` is a Warn under the Apple Silicon default
    /// (the daemon stays on ort and says so) and an Error under explicit
    /// `TRUSTY_EMBEDDER=python`, the same split the launcher row makes.
    /// Mutation caught: the pre-#9970 uv row is an Error for both.
    #[tokio::test]
    #[serial]
    async fn uv_rows_warn_under_the_graceful_default_and_error_for_explicit_python() {
        let data = tempfile::tempdir().expect("tempdir");
        let _d = EnvVarGuard::set(
            "TRUSTY_DATA_DIR_OVERRIDE",
            data.path().to_str().expect("utf-8"),
        );
        let _f = EnvVarGuard::set("TRUSTY_UV_FETCH", "0");
        let bad = data.path().join("no-such-uv");
        let _u = EnvVarGuard::set("TRUSTY_UV_BIN", bad.to_str().expect("utf-8"));
        for (value, graceful) in [(None, true), (Some("python"), false)] {
            let _e = match value {
                Some(v) => EnvVarGuard::set("TRUSTY_EMBEDDER", v),
                None => EnvVarGuard::remove("TRUSTY_EMBEDDER"),
            };
            let state = DoctorState::new(DaemonClient::at("/nonexistent/daemon.sock"));
            let check = PythonEmbedderCheck {
                is_apple_silicon: true,
            };
            let uv = &check.run(&state).await[0];
            let names_cause = |m: &str| m.contains("TRUSTY_UV_BIN");
            match uv {
                CheckResult::Warn(m) if graceful && names_cause(m) => {}
                CheckResult::Error(m) if !graceful && names_cause(m) => {}
                other => panic!("TRUSTY_EMBEDDER={value:?}: unexpected uv row {other:?}"),
            }
        }
    }

    /// A mock daemon refusing `failing` and answering every other method with
    /// a one-index list or an empty status body.
    async fn daemon_failing(failing: &'static str) -> crate::commands::mock_socket::MockDaemon {
        use trusty_search::service::rpc::reads::METHOD_INDEXES_LIST;
        crate::commands::mock_socket::mock_daemon(move |method, _| {
            if method == failing {
                Err(trusty_common::uds::server::RpcError::new(
                    trusty_common::uds::server::CODE_INTERNAL_ERROR,
                    "registry unreadable",
                ))
            } else if method == METHOD_INDEXES_LIST {
                Ok(serde_json::json!({"indexes": ["a"]}))
            } else {
                Ok(serde_json::json!({}))
            }
        })
        .await
    }

    /// #9214: a failed list is an Error naming the cause, not "No indexes
    /// registered", and queues nothing for `--fix`.
    #[tokio::test]
    async fn indexes_check_reports_a_failed_list_and_queues_no_reindex() {
        use trusty_search::service::rpc::reads::METHOD_INDEXES_LIST;
        let daemon = daemon_failing(METHOD_INDEXES_LIST).await;
        let state = DoctorState::new(daemon.client.clone());
        state.set_daemon_health(true, "9.9.9".into(), None);
        let results = IndexesCheck.run(&state).await;
        assert!(
            matches!(results.as_slice(), [CheckResult::Error(m)] if m.contains("registry unreadable")),
            "{results:?}"
        );
        assert!(state.take_empty_indexes().is_empty());
    }

    /// #9214: a failed status read is a Warn, never a zero-chunk index that
    /// `--fix` would reindex.
    #[tokio::test]
    async fn indexes_check_never_reads_a_failed_status_as_empty() {
        let daemon = daemon_failing(trusty_search::service::rpc::reads::METHOD_INDEX_STATUS).await;
        let state = DoctorState::new(daemon.client.clone());
        state.set_daemon_health(true, "9.9.9".into(), None);
        let results = IndexesCheck.run(&state).await;
        assert!(
            results
                .iter()
                .any(|r| matches!(r, CheckResult::Warn(m) if m.contains("status read failed"))),
            "{results:?}"
        );
        assert!(state.take_empty_indexes().is_empty());
    }
}
