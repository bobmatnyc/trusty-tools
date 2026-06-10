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
            defer_embed: None,
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
#[tokio::test]
async fn validate_root_path_denylist_rejects_home() {
    let home = dirs::home_dir().expect("home dir required");
    // Home must exist as a directory on all CI machines.
    if !home.is_dir() {
        return;
    }
    let result = super::helpers::validate_root_path(&home).await;
    assert!(
        result.is_err(),
        "$HOME itself must be rejected by validate_root_path"
    );
}

/// The denylist must block /tmp, /private/tmp, and /var/folders at the daemon
/// layer via `validate_root_path` — tested using LITERAL path values.
///
/// Why: the previous test used `std::env::temp_dir()` which on macOS returns
/// `/var/folders/…` (not `/tmp`). That meant the assertion "a /tmp subdir is
/// denied" was actually testing a completely different prefix, making the test
/// pass for the wrong reason. By constructing each path prefix that
/// `SENSITIVE_PATH_PREFIXES` covers and asserting each one, we verify exactly
/// the rules stated in the denylist — not the ambient OS temp directory.
/// What: calls `validate_root_path` (which calls `is_denied` on the canonical
/// path) with paths rooted at every prefix in `SENSITIVE_PATH_PREFIXES` that
/// covers ephemeral OS directories. Because these directories don't exist on
/// disk, `validate_root_path` will return `Err` at the "does not exist" check
/// rather than the denylist check; so we test `is_denied` directly (the public
/// function that the daemon's validate path calls after canonicalization). This
/// validates the denylist logic in isolation — the daemon's canonical guard
/// would apply the same check after the path is resolved.
/// Test: this test; see also `allowlist::tests::denylist_blocks_tmp` which
/// tests `is_denied` directly for `/tmp/my-project`.
#[test]
fn validate_root_path_denylist_rejects_tmp_literal_paths() {
    use crate::allowlist::{is_denied, SENSITIVE_PATH_PREFIXES};

    // Build one representative sub-path for each OS-temp prefix in the denylist.
    // These are the LITERAL prefix values from SENSITIVE_PATH_PREFIXES — not
    // whatever std::env::temp_dir() happens to return on the current host.
    let tmp_subpaths: &[&str] = &[
        "/tmp/ts-denylist-probe",         // SENSITIVE_PATH_PREFIXES[0]: "/tmp/"
        "/private/tmp/ts-denylist-probe", // SENSITIVE_PATH_PREFIXES[1]: "/private/tmp"
        "/var/folders/ts-denylist-probe", // SENSITIVE_PATH_PREFIXES[2]: "/var/folders"
        "/private/var/folders/ts-denylist-probe", // SENSITIVE_PATH_PREFIXES[3]: "/private/var/folders"
    ];

    // Cross-check: every path we're testing is actually covered by a prefix in
    // the static table — this assertion documents the coupling and catches
    // future renames of the prefix strings.
    for subpath in tmp_subpaths {
        let covered = SENSITIVE_PATH_PREFIXES
            .iter()
            .any(|prefix| subpath.starts_with(prefix));
        assert!(
            covered,
            "test path {subpath:?} is not covered by any SENSITIVE_PATH_PREFIXES entry — \
             update the test or the denylist"
        );
    }

    // Now verify is_denied rejects every literal path.
    for subpath in tmp_subpaths {
        let path = std::path::Path::new(subpath);
        assert!(
            is_denied(path).is_some(),
            "expected is_denied to block {subpath:?} (covers SENSITIVE_PATH_PREFIXES rule)"
        );
    }
}

/// A normal project directory must NOT be denied by `validate_root_path`.
///
/// Why: the denylist must not block legitimate developer project dirs.
/// What: finds a well-known non-sensitive directory that exists on this
/// machine and asserts `validate_root_path` returns `Ok`.
/// Test: this test.
#[tokio::test]
async fn validate_root_path_accepts_safe_project_dir() {
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
        let result = super::helpers::validate_root_path(path).await;
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
#[tokio::test]
async fn validate_root_path_denylist_blocks_symlink_to_ssh() {
    let home = dirs::home_dir().expect("home dir");
    let ssh = home.join(".ssh");
    if !ssh.is_dir() {
        return; // No ~/.ssh on this machine — skip.
    }
    // Create a symlink under target/ so both the symlink location and resolved
    // target are unambiguous: the link itself lives in a non-denied path, but
    // its resolved target (~/.ssh) is in SENSITIVE_PATH_PREFIXES.  Using
    // std::env::temp_dir() is unreliable here because on macOS it returns
    // /private/var/folders/… which is itself in the denylist, causing the
    // rejection to fire at the wrong check.
    let base = std::env::current_dir().expect("cwd").join("target");
    std::fs::create_dir_all(&base).ok();
    let link = base.join(format!("ts-denylist-ssh-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    if std::os::unix::fs::symlink(&ssh, &link).is_err() {
        return; // Cannot create symlink — skip.
    }
    let result = super::helpers::validate_root_path(&link).await;
    let _ = std::fs::remove_file(&link);
    assert!(
        result.is_err(),
        "symlink to ~/.ssh must be refused (canonicalization must resolve the symlink)"
    );
}
