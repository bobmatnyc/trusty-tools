//! Tests for `spawn_startup_tasks` (#474).
//!
//! Why: `spawn_startup_tasks` populates `AppState::pin_project_map` in the
//! background so handlers can resolve a palace id to a project path without
//! a filesystem walk at request time (issue #470). Moved out of `main.rs`
//! into its own file (pure code motion, no logic change) to keep `main.rs`
//! under the 500 SLOC production cap — see issue #2522 review.
//!
//! What: exercises the full wiring — a real `AppState` with a real temp
//! search root, a pinned project, and the async background task. The test
//! owns its runtime and its environment: `spawn_startup_tasks` fans out the
//! whole daemon startup sequence, and two parts of it outlive the assertions
//! (#5937), while a third reads an override the calling shell may already
//! export (#5821).
//!
//! Test: this file.

use super::*;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::time::Duration;
use trusty_memory::project_root::{write_project_pin, ProjectPin};

/// RAII guard that pins one environment variable for the duration of a test
/// and restores the prior value on drop, including on panic.
///
/// Why: this binary's only env-mutating test set `HOME` and
/// `TRUSTY_SKIP_PALACE_ENFORCEMENT` and restored `HOME` on the success path
/// alone, so a failing assertion left the other twelve tests in this binary
/// pointed at a deleted tempdir (#5821).
/// What: `set` installs a value and `clear` removes one; both capture the
/// prior value and `Drop` puts it back.
/// Test: `spawn_startup_tasks_populates_pin_map`.
struct EnvGuard {
    key: &'static str,
    prev: Option<OsString>,
}

impl EnvGuard {
    fn set<V: AsRef<OsStr>>(key: &'static str, value: V) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: the caller is `#[serial_test::serial]`, so no sibling test
        // reads or writes the process environment concurrently.
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }

    fn clear(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: see `set`.
        unsafe { std::env::remove_var(key) };
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// Why: the pin scan inside `spawn_startup_tasks` must populate
/// `AppState::pin_project_map` so handlers can resolve a palace id to a
/// project path without a filesystem walk at request time (issue #470).
/// This test verifies the full wiring: a real `AppState` with a real temp
/// search root, a pinned project, and the async background task.
/// What: creates a project with a pin file under a temp search root; then
/// calls `spawn_startup_tasks` and yields to the tokio runtime until the
/// task completes; asserts the pin map contains the expected entry.
/// Test: itself (issue #474 regression guard, plus #5937 and #5821).
#[serial_test::serial]
#[test]
fn spawn_startup_tasks_populates_pin_map() {
    // #5937: the fan-out warms the shared embedder, and a cold init downloads
    // the ONNX model on a blocking-pool thread. Seeding the mock first makes
    // `shared_embedder()` resolve from an already-initialised OnceCell, so no
    // download starts. Must happen before the runtime exists.
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();

    // Build a temp search root with one pinned project.
    let tmp = tempfile::tempdir().expect("tempdir");
    let search_root = tmp.path().join("Projects");
    let project_dir = search_root.join("my-project");
    fs::create_dir_all(&project_dir).expect("create project dir");
    write_project_pin(&project_dir, &ProjectPin::new("my-palace".to_string())).expect("write pin");

    // `HOME` points `default_search_dirs()` at the temp root, and the
    // enforcement bypass keeps `AppState::new` off a real project root.
    let _home = EnvGuard::set("HOME", tmp.path());
    let _enforcement = EnvGuard::set("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    // #5821: the #880 isolation branch skips the pin scan outright when a
    // data-dir override is active, so a shell exporting one made this test
    // assert against a map the scan is designed to leave empty — it reported
    // "0 pin(s) discovered" and panicked on the 500 ms deadline.
    let _data_dir = EnvGuard::clear(trusty_common::DATA_DIR_OVERRIDE_ENV);
    // #5937: the update check resolves crates.io on the blocking pool, which
    // is the second task free to outlive this test.
    let _update = EnvGuard::set(trusty_common::update::NO_UPDATE_CHECK_ENV, "1");

    let state_root = tmp.path().join("data");
    fs::create_dir_all(&state_root).expect("create data dir");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build test runtime");

    let found = rt.block_on(async {
        let state = AppState::new(state_root);

        // Fire the background task.
        spawn_startup_tasks(&state);

        // Yield to the tokio runtime repeatedly until the task populates the
        // pin map or a timeout is reached (50 × 10 ms = 500 ms ceiling).
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        loop {
            if let Some(entry) = state.pin_project_map.get("my-palace") {
                return Some(entry.clone());
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    // #5937: shut down without waiting. Dropping a runtime blocks until every
    // blocking-pool task it started has finished, and the startup fan-out is
    // free to leave one in flight — that wait, not the pin scan, is what held
    // this test open for as long as the model fetch took.
    rt.shutdown_timeout(Duration::ZERO);

    let found = found.expect(
        "pin_project_map must contain 'my-palace' after spawn_startup_tasks; \
         the scan did not populate it within 500 ms",
    );
    // Canonicalize to handle macOS /private symlinks.
    let actual = fs::canonicalize(found).expect("canonicalize actual");
    let expected = fs::canonicalize(&project_dir).expect("canonicalize expected");
    assert_eq!(
        actual, expected,
        "pin_project_map entry must point to the project directory"
    );
}
