//! Tests for `spawn_startup_tasks` (#474).
//!
//! Why: `spawn_startup_tasks` populates `AppState::pin_project_map` in the
//! background so handlers can resolve a palace id to a project path without
//! a filesystem walk at request time (issue #470). Moved out of `main.rs`
//! into its own file (pure code motion, no logic change) to keep `main.rs`
//! under the 500 SLOC production cap — see issue #2522 review.
//!
//! What: exercises the full wiring — a real `AppState` with a real temp
//! search root, a pinned project, and the async background task.
//!
//! Test: this file.

use super::*;
use std::fs;
use trusty_memory::project_root::{write_project_pin, ProjectPin, PIN_SCHEMA_VERSION};

/// Why: the pin scan inside `spawn_startup_tasks` must populate
/// `AppState::pin_project_map` so handlers can resolve a palace id to a
/// project path without a filesystem walk at request time (issue #470).
/// This test verifies the full wiring: a real `AppState` with a real temp
/// search root, a pinned project, and the async background task.
/// What: creates a project with a pin file under a temp search root; then
/// calls `spawn_startup_tasks` and yields to the tokio runtime until the
/// task completes; asserts the pin map contains the expected entry.
/// Test: itself (issue #474 regression guard).
#[tokio::test]
async fn spawn_startup_tasks_populates_pin_map() {
    // Build a temp search root with one pinned project.
    let tmp = tempfile::tempdir().expect("tempdir");
    let search_root = tmp.path().join("Projects");
    let project_dir = search_root.join("my-project");
    fs::create_dir_all(&project_dir).expect("create project dir");
    write_project_pin(
        &project_dir,
        &ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "my-palace".to_string(),
            note: None,
        },
    )
    .expect("write pin");

    // Override HOME so `default_search_dirs()` points at our temp root.
    // SAFETY: single-threaded test; env var only affects this process.
    let prev_home = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    // Also bypass palace-slug enforcement so AppState::new doesn't
    // need a real project root.
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }

    let state_root = tmp.path().join("data");
    fs::create_dir_all(&state_root).expect("create data dir");
    let state = AppState::new(state_root);

    // Fire the background task.
    spawn_startup_tasks(&state);

    // Yield to the tokio runtime repeatedly until the task populates the
    // pin map or a timeout is reached (50 × 10 ms = 500 ms ceiling).
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        if state.pin_project_map.contains_key("my-palace") {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "pin_project_map was not populated within 500 ms; \
                 spawn_startup_tasks may not be running the pin scan"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Restore HOME.
    match prev_home {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    let found = state.pin_project_map.get("my-palace").map(|e| e.clone());
    assert!(
        found.is_some(),
        "pin_project_map must contain 'my-palace' after spawn_startup_tasks"
    );
    // Canonicalize to handle macOS /private symlinks.
    let actual = fs::canonicalize(found.unwrap()).expect("canonicalize actual");
    let expected = fs::canonicalize(&project_dir).expect("canonicalize expected");
    assert_eq!(
        actual, expected,
        "pin_project_map entry must point to the project directory"
    );
}
