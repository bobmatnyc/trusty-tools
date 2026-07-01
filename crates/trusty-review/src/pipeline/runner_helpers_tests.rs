//! Unit tests for `pipeline::runner_helpers::resolve_diff_token` (#1880).
//!
//! Why: split from `runner_helpers.rs` to keep that file under the 500-line
//! cap while covering the serve-mode empty-token regression directly at the
//! function boundary (no network required — App-mode resolution fails fast
//! on missing credentials/installation before any HTTP call is made).
//! What: exercises all four branches of `resolve_diff_token`: `LocalFile`
//! pass-through, already-resolved-token pass-through, Serve-mode failure
//! without App credentials, and Cli-mode resolution from `config.github_token`.
//! Test: this is the test module; each function is a self-contained unit test.

use super::*;
use std::path::PathBuf;

fn config_without_app_creds() -> ReviewConfig {
    let mut config = ReviewConfig::load(None);
    config.github_app_id = None;
    config.github_app_private_key = None;
    config.github_installations = Vec::new();
    config
}

#[tokio::test]
async fn resolve_diff_token_passes_through_local_file() {
    // A LocalFile source needs no GitHub credentials at all; it must pass
    // through unchanged regardless of run mode or config.
    let config = config_without_app_creds();
    let source = DiffSource::LocalFile {
        path: PathBuf::from("/tmp/whatever.diff"),
    };

    let resolved = resolve_diff_token(&source, &config, RunMode::Serve)
        .await
        .expect("LocalFile source must never fail token resolution");

    match resolved {
        DiffSource::LocalFile { path } => assert_eq!(path, PathBuf::from("/tmp/whatever.diff")),
        other => panic!("expected LocalFile to pass through unchanged, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_diff_token_passes_through_already_resolved_token() {
    // A Github source with a non-empty token (the CLI run/compare/calibrate
    // paths resolve up front) must be returned unchanged — no redundant
    // resolution attempt. We prove this by using Serve mode with NO App
    // credentials configured: if resolution were (incorrectly) attempted,
    // this would error; since the token is already set, it must not be.
    let config = config_without_app_creds();
    let source = DiffSource::Github {
        owner: "acme".to_string(),
        repo: "widgets".to_string(),
        pr: 42,
        token: "already-resolved-token".to_string(),
    };

    let resolved = resolve_diff_token(&source, &config, RunMode::Serve)
        .await
        .expect("a non-empty token must never trigger re-resolution");

    match resolved {
        DiffSource::Github { token, .. } => assert_eq!(token, "already-resolved-token"),
        other => panic!("expected Github source to pass through unchanged, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_diff_token_serve_mode_without_app_creds_errors() {
    // This is the #1880 regression: serve-mode webhook/handler paths build
    // `DiffSource::Github` with an empty token placeholder. Without App
    // credentials configured, resolution must fail CLOSED with an actionable
    // error — never silently return an empty token that would surface as an
    // opaque 401 from GitHub.
    let config = config_without_app_creds();
    let source = DiffSource::Github {
        owner: "acme".to_string(),
        repo: "widgets".to_string(),
        pr: 7,
        token: String::new(),
    };

    let err = resolve_diff_token(&source, &config, RunMode::Serve)
        .await
        .expect_err("empty token with no App credentials must fail, not silently proceed");

    let message = err.to_string();
    assert!(
        message.contains("GITHUB_APP_ID") || message.contains("GITHUB_APP_PRIVATE_KEY"),
        "error must name the missing config so an operator can act on it: {message}"
    );
}

#[tokio::test]
async fn resolve_diff_token_cli_mode_resolves_from_config_token() {
    // Cli mode with an empty placeholder token and `config.github_token` set
    // must resolve synchronously (no network) to that configured token.
    let mut config = config_without_app_creds();
    config.github_token = "ghp_configured_pat".to_string(); // pragma: allowlist secret
    let source = DiffSource::Github {
        owner: "acme".to_string(),
        repo: "widgets".to_string(),
        pr: 99,
        token: String::new(),
    };

    let resolved = resolve_diff_token(&source, &config, RunMode::Cli)
        .await
        .expect("Cli mode must resolve from config.github_token");

    match resolved {
        DiffSource::Github {
            token,
            owner,
            repo,
            pr,
        } => {
            assert_eq!(token, "ghp_configured_pat");
            assert_eq!(owner, "acme");
            assert_eq!(repo, "widgets");
            assert_eq!(pr, 99);
        }
        other => panic!("expected a resolved Github source, got {other:?}"),
    }
}
