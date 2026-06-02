//! GitHub authentication — dual-mode strategy abstraction (issue #582).
//!
//! Why: trusty-review authenticates differently depending on how it is invoked
//! — developer CLI runs use a PAT or the user's `gh` login, while the deployed
//! webhook service authenticates as a GitHub App.  This module exposes a single
//! abstraction so every downstream GitHub operation (PR fetch, review posting,
//! and future tracker/issue calls in #585/#550) resolves a token the same way
//! regardless of mode.
//!
//! What: `app` holds the GitHub App JWT/installation mechanics; `strategy`
//! holds the run-mode-aware `AuthStrategy`/`RunMode` selection and the CLI
//! token-resolution chain (`GITHUB_TOKEN` → `GH_TOKEN` → `gh auth token`).
//! `resolve_token` is a thin convenience wrapper preserved for call sites that
//! have not yet been migrated to an explicit `AuthStrategy`.
//!
//! Test: each submodule carries its own unit tests; see `app::tests` and
//! `strategy::tests`.

pub mod app;
pub mod strategy;

pub use app::{exchange_installation_token, mint_app_jwt, resolve_app_token};
pub use strategy::{AUTH_MODE_ENV, AuthStrategy, GhTokenResolver, RunMode, SystemGhResolver};

use crate::config::ReviewConfig;
use crate::integrations::github::{GithubClient, GithubError};

/// Resolve a GitHub token using an explicitly chosen run mode.
///
/// Why: a convenience entry point for call sites that know their run mode but
/// do not want to construct an `AuthStrategy` themselves; it applies the same
/// auto-select + override precedence as `AuthStrategy::select`.
/// What: selects the strategy from `mode` (honouring the `TRUSTY_REVIEW_AUTH_MODE`
/// override) and resolves a token for `owner`.
/// Test: covered by `strategy::tests` (selection + resolution) and the
/// integration call sites.
pub async fn resolve_token_for_mode(
    client: &GithubClient,
    config: &ReviewConfig,
    owner: &str,
    mode: RunMode,
) -> Result<String, GithubError> {
    AuthStrategy::select(mode, None)
        .resolve_token(client, config, owner)
        .await
}
