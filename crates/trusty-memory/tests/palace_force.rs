//! Integration tests for the `palace_create` `force` flag (spec-001 Phase 1).
//!
//! Why: ordinary `palace_create` calls are gated by issue #88 project-slug
//! validation (the palace name must match the cwd-derived project slug or be
//! `personal`). An application driving trusty-memory as a chat-session manager
//! needs to create palaces under arbitrary app/tenant slugs, so a `force=true`
//! escape hatch was added. This test lives in its own integration binary so it
//! runs in a process where `TRUSTY_SKIP_PALACE_ENFORCEMENT` is never set —
//! the lib's unit-test helpers set that var globally, which would otherwise
//! mask the gate we are trying to exercise.
//! What: drives `dispatch_tool` against an `AppState` rooted at a tempdir,
//! passing an explicit `cwd` that has no project markers so validation has a
//! deterministic outcome regardless of where `cargo test` runs.
//! Test: this IS the test module.

use serde_json::json;
use tempfile::TempDir;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

/// Build a ready `AppState` plus two tempdirs: the palace data root and a
/// marker-free "project" dir used as the validation cwd.
///
/// Why: `force=false` must fail deterministically; pointing the validator at a
/// directory with no `.git`/`Cargo.toml`/etc. guarantees the "no project root"
/// rejection path rather than depending on the test runner's cwd.
/// What: returns `(state, root_tmp, cwd_tmp)`; callers bind all three so drop
/// cleans up after the test.
/// Test: used by both tests below.
fn fixture() -> (AppState, TempDir, TempDir) {
    let root_tmp = tempfile::tempdir().expect("data root tempdir");
    let cwd_tmp = tempfile::tempdir().expect("cwd tempdir");
    let state = AppState::new(root_tmp.path().to_path_buf());
    state.set_ready();
    (state, root_tmp, cwd_tmp)
}

/// `force=true` bypasses slug validation and creates an arbitrary-slug palace.
///
/// Why: spec-001 acceptance criterion 1 — applications create custom palaces.
/// What: creates two forced palaces under non-project slugs, confirms both
/// appear in `palace_list`, and confirms each reports its own isolated
/// `data_dir` (separate redb + HNSW root) via `palace_info`.
/// Test: this function.
#[tokio::test]
async fn force_true_creates_custom_slug_palace_with_isolated_dirs() {
    let (state, _root, cwd) = fixture();
    let cwd_str = cwd.path().to_string_lossy().to_string();

    for slug in ["acme-app", "tenant-42"] {
        let created = dispatch_tool(
            &state,
            "palace_create",
            json!({ "name": slug, "force": true, "cwd": cwd_str }),
        )
        .await
        .unwrap_or_else(|e| panic!("force create '{slug}' should succeed: {e:#}"));
        assert_eq!(created["palace_id"], slug);
        assert_eq!(created["status"], "created");
    }

    let listed = dispatch_tool(&state, "palace_list", json!({}))
        .await
        .expect("palace_list");
    let palaces = listed["palaces"].as_array().expect("palaces array");
    let names: Vec<&str> = palaces.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"acme-app"), "acme-app present: {names:?}");
    assert!(names.contains(&"tenant-42"), "tenant-42 present: {names:?}");

    // Each palace must report its own data dir under the shared root — this is
    // the on-disk isolation boundary (separate redb + HNSW per palace).
    let info_a = dispatch_tool(&state, "palace_info", json!({ "palace": "acme-app" }))
        .await
        .expect("palace_info acme-app");
    let info_b = dispatch_tool(&state, "palace_info", json!({ "palace": "tenant-42" }))
        .await
        .expect("palace_info tenant-42");
    let dir_a = info_a["data_dir"].as_str().expect("acme data_dir");
    let dir_b = info_b["data_dir"].as_str().expect("tenant data_dir");
    assert_ne!(dir_a, dir_b, "palaces must have distinct data dirs");
    assert!(dir_a.ends_with("acme-app"), "dir_a = {dir_a}");
    assert!(dir_b.ends_with("tenant-42"), "dir_b = {dir_b}");
    assert!(
        std::path::Path::new(dir_a).is_dir(),
        "acme-app data dir exists on disk"
    );
}

/// `force=false` (the default) still enforces project-slug validation.
///
/// Why: spec-001 acceptance criterion 1 — the gate must remain intact for
/// ordinary callers; `force` is opt-in only.
/// What: attempts to create an arbitrary-slug palace from a marker-free cwd
/// without `force`; expects an error mentioning the missing project root.
/// Test: this function.
#[tokio::test]
async fn force_false_still_enforces_validation() {
    let (state, _root, cwd) = fixture();
    let cwd_str = cwd.path().to_string_lossy().to_string();

    let err = dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "acme-app", "cwd": cwd_str }),
    )
    .await
    .expect_err("force=false must reject a non-project slug");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("no project root") || msg.contains("does not match"),
        "expected a validation error, got: {msg}"
    );
}
