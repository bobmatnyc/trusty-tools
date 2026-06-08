//! Unit and integration tests for the sensitive-root denylist enforced by
//! `validate_root_path` (issue: index-denylist).
//!
//! Why: keeping these tests in a sibling file prevents `tests_index.rs`
//! from exceeding the 500-line cap while keeping every assertion co-located
//! with the server module they validate.
//! What: covers daemon-side denylist rejection via `create_index_handler`
//! and directly via `super::helpers::validate_root_path`; also covers the
//! symlink-bypass prevention and safe-path acceptance.
//! Test: all tests in this file are collected by `cargo test -p trusty-search`.
use super::*;
use crate::core::embed::Embedder;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// The denylist must block ~/.ssh even when called directly through the
/// create_index HTTP handler (daemon-side guard, not just CLI).
///
/// Why: defense-in-depth — a direct `POST /indexes` call (bypassing the CLI)
/// must also be refused. This test pins the server-side behavior so a refactor
/// of `validate_root_path` can never silently remove the guard.
/// What: calls `create_index_handler` with root_path = ~/.ssh; asserts 400 and
/// an error body containing "indexing refused".
/// Test: this test.
#[tokio::test]
async fn validate_root_path_denylist_rejects_ssh() {
    let home = dirs::home_dir().expect("home dir required for this test");
    let ssh_path = home.join(".ssh");
    // Only run this test when ~/.ssh actually exists (common on developer
    // machines); skip on environments without it to avoid a 400 for a
    // different reason ("does not exist").
    if !ssh_path.is_dir() {
        return;
    }

    use crate::core::registry::IndexRegistry;
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state_arc = Arc::new(state);

    let resp = create_index_handler(
        State(Arc::clone(&state_arc)),
        Json(CreateIndexRequest {
            id: "sensitive-ssh".into(),
            root_path: ssh_path,
            include_paths: None,
            exclude_globs: None,
            extensions: None,
            domain_terms: None,
            path_filter: None,
            include_docs: None,
            respect_gitignore: None,
            lexical_only: None,
            skip_kg: None,
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "~/.ssh must be refused with 400"
    );
    let body = axum::body::to_bytes(resp.into_body(), 65536)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let err = json.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("indexing refused"),
        "error must mention 'indexing refused', got: {err:?}"
    );
}

/// The denylist must block $HOME itself at the daemon layer.
///
/// Why: indexing $HOME would capture an enormous amount of private data.
/// This test verifies the daemon-side guard rejects it regardless of the
/// caller (CLI bypass, direct HTTP, MCP tool).
/// What: calls `validate_root_path` directly with the home directory and
/// asserts an `Err` response.
/// Test: this test.
#[test]
fn validate_root_path_denylist_rejects_home() {
    let home = dirs::home_dir().expect("home dir required");
    // Home must exist as a directory on all CI machines.
    if !home.is_dir() {
        return;
    }
    let result = super::helpers::validate_root_path(&home);
    assert!(
        result.is_err(),
        "$HOME itself must be rejected by validate_root_path"
    );
}

/// The denylist must block /tmp at the daemon layer.
///
/// Why: /tmp is an OS-managed ephemeral directory; indexing it is meaningless
/// and potentially dangerous.
/// What: creates a real sub-directory under /tmp (so the "not a dir" check
/// passes), then calls `validate_root_path` and asserts an `Err` response.
/// Test: this test.
#[test]
fn validate_root_path_denylist_rejects_tmp() {
    // Create a real subdir under /tmp so the is_dir check passes.
    let tmp = std::env::temp_dir().join(format!("ts-denylist-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    if !tmp.is_dir() {
        // Can't create tmp dir (permission issue in this environment) — skip.
        return;
    }
    let result = super::helpers::validate_root_path(&tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        result.is_err(),
        "/tmp subdirectory must be rejected by validate_root_path"
    );
}

/// A normal project directory must NOT be denied by `validate_root_path`.
///
/// Why: the denylist must not block legitimate developer project dirs.
/// What: finds a well-known non-sensitive directory that exists on this
/// machine and asserts `validate_root_path` returns `Ok`.
/// Test: this test.
#[test]
fn validate_root_path_accepts_safe_project_dir() {
    // Strategy: find a directory that (a) exists, (b) is not sensitive.
    // On both macOS and Linux, /usr or /opt tend to be present and non-sensitive.
    // We skip on systems where neither exists.
    let candidate = [
        std::path::Path::new("/usr/local/share"),
        std::path::Path::new("/usr/share"),
        std::path::Path::new("/opt"),
        std::path::Path::new("/srv"),
    ]
    .iter()
    .find(|p| p.is_dir())
    .copied();

    if let Some(path) = candidate {
        let result = super::helpers::validate_root_path(path);
        assert!(
            result.is_ok(),
            "expected Ok for safe directory {:?}, got Err",
            path
        );
    }
    // If none of the well-known paths exist (unusual CI environment), skip gracefully.
}

/// Symlink-bypass test: a symlink pointing at ~/.ssh must still be refused.
///
/// Why: canonicalization in `validate_root_path` resolves symlinks before
/// the denylist check, so `ln -s ~/.ssh /tmp/safe-looking` cannot bypass it.
/// What: creates a symlink at a temp path pointing at ~/.ssh; calls
/// `validate_root_path` with the symlink path; asserts `Err`.
/// Test: this test (Unix-only).
#[cfg(unix)]
#[test]
fn validate_root_path_denylist_blocks_symlink_to_ssh() {
    let home = dirs::home_dir().expect("home dir");
    let ssh = home.join(".ssh");
    if !ssh.is_dir() {
        return; // No ~/.ssh on this machine — skip.
    }
    // Create a symlink in a non-denied location pointing at ~/.ssh.
    // We use /tmp for the symlink itself (the link file lives there, not the target).
    let link = std::env::temp_dir().join(format!("ts-denylist-ssh-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    if std::os::unix::fs::symlink(&ssh, &link).is_err() {
        return; // Cannot create symlink — skip.
    }
    let result = super::helpers::validate_root_path(&link);
    let _ = std::fs::remove_file(&link);
    assert!(
        result.is_err(),
        "symlink to ~/.ssh must be refused (canonicalization must resolve the symlink)"
    );
}
