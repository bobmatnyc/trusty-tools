//! Dual-mode GitHub authentication strategy (run-mode-dependent).
//!
//! Why: trusty-review runs in two distinct contexts with different credential
//! models (issue #582).  Local/CLI invocations (`run`, `compare`, `profile`)
//! authenticate as a *developer* using a Personal Access Token or the user's
//! `gh` CLI login; the deployed webhook service (`serve`) authenticates as a
//! *GitHub App* using per-installation tokens.  Routing every GitHub operation
//! through one strategy abstraction keeps the two modes from leaking into the
//! call sites (PR fetch, review posting, future tracker upsert in #585, future
//! GH-Issues context in #550 all share this single entry point).
//!
//! What: `RunMode` captures which invocation surface we are on; `AuthStrategy`
//! is the resolved credential strategy (`Cli` = PAT/`gh`, `App` = GitHub App);
//! `AuthStrategy::select` auto-selects from the run mode with an explicit
//! override (`TRUSTY_REVIEW_AUTH_MODE` env or a passed flag), and
//! `resolve_token` yields a bearer token for a given org owner.
//!
//! Resolution order in CLI mode (per #582): `GITHUB_TOKEN` → `GH_TOKEN` →
//! `gh auth token` (shell out) → clear `MissingToken` error.
//!
//! Test: `select_*` cover auto-selection + override; `cli_token_*` cover the
//! env precedence and `gh auth token` fallback path via an injected resolver;
//! `app_strategy_requires_credentials` covers the service-mode guard.

use crate::config::ReviewConfig;
use crate::integrations::github::auth::app::resolve_app_token;
use crate::integrations::github::{GithubClient, GithubError};

// ─── Run mode ──────────────────────────────────────────────────────────────────

/// The invocation surface the process is running under.
///
/// Why: auth strategy auto-selection keys off whether we are a developer-facing
/// CLI subcommand or the long-lived webhook daemon, so the caller declares its
/// mode once at the entry point instead of threading credential choices
/// through every GitHub call.
/// What: `Cli` is the local developer path (`run`/`compare`/`profile`); `Serve`
/// is the deployed webhook service.
/// Test: `select_cli_defaults_to_cli_strategy`, `select_serve_defaults_to_app`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Local developer CLI (`run`, `compare`, `profile`).
    Cli,
    /// Deployed webhook service (`serve`).
    Serve,
}

// ─── Strategy ───────────────────────────────────────────────────────────────────

/// The resolved GitHub authentication strategy.
///
/// Why: a single typed enum lets every downstream GitHub operation ask for a
/// token without knowing whether the process is App-backed or PAT-backed.
/// What: `Cli` resolves a developer token (PAT env vars or `gh auth token`);
/// `App` resolves a per-installation GitHub App token by org owner.
/// Test: `resolve_token` behaviour is covered per-variant below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStrategy {
    /// Developer token: `GITHUB_TOKEN` → `GH_TOKEN` → `gh auth token`.
    Cli,
    /// GitHub App: JWT → per-installation access token (by org owner).
    App,
}

/// Environment-variable name to force a specific auth strategy.
///
/// Why: #582 requires an explicit override so a developer can force App auth in
/// CLI for testing, or force PAT auth in `serve` for a single-repo deployment.
/// What: set to `cli`/`pat`/`gh` to force the CLI strategy, or `app`/`github_app`
/// to force the App strategy.  Any other value is ignored (auto-select wins).
/// Test: `select_override_forces_app`, `select_override_forces_cli`.
pub const AUTH_MODE_ENV: &str = "TRUSTY_REVIEW_AUTH_MODE";

impl AuthStrategy {
    /// Select the auth strategy from the run mode, honouring an explicit override.
    ///
    /// Why: the auto-by-mode default (CLI→PAT, serve→App) covers the common
    /// case, but #582 mandates an escape hatch for testing and single-repo
    /// deploys.  Centralising the precedence here keeps it consistent.
    /// What: an explicit `override_flag` (from a CLI flag) wins; otherwise the
    /// `TRUSTY_REVIEW_AUTH_MODE` env var is consulted; otherwise the strategy is
    /// derived from `mode` (`Cli`→`Cli`, `Serve`→`App`).
    /// Test: `select_cli_defaults_to_cli_strategy`, `select_serve_defaults_to_app`,
    /// `select_override_forces_app`, `select_override_forces_cli`,
    /// `select_env_forces_app`.
    pub fn select(mode: RunMode, override_flag: Option<&str>) -> Self {
        if let Some(forced) = override_flag.and_then(Self::parse_override) {
            return forced;
        }
        if let Ok(env_val) = std::env::var(AUTH_MODE_ENV)
            && let Some(forced) = Self::parse_override(&env_val)
        {
            return forced;
        }
        match mode {
            RunMode::Cli => AuthStrategy::Cli,
            RunMode::Serve => AuthStrategy::App,
        }
    }

    /// Parse an override string into a forced strategy.
    ///
    /// Why: both the env var and the CLI flag accept the same vocabulary, so the
    /// parsing rule lives in one place.
    /// What: `cli`/`pat`/`gh`/`token` → `Cli`; `app`/`github_app`/`github-app`
    /// → `App`; anything else (incl. empty) → `None` (no override).
    /// Test: covered transitively by the `select_*` tests.
    fn parse_override(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "cli" | "pat" | "gh" | "token" => Some(AuthStrategy::Cli),
            "app" | "github_app" | "github-app" => Some(AuthStrategy::App),
            _ => None,
        }
    }

    /// Resolve a bearer token for GitHub API calls against `owner`.
    ///
    /// Why: this is the single funnel every downstream GitHub operation uses, so
    /// the credential source is decided exactly once per call regardless of mode.
    /// What: in `Cli` mode resolves a developer token via env vars then
    /// `gh auth token`; in `App` mode mints + exchanges a per-installation App
    /// token for `owner`.  Returns a typed `GithubError` on failure.
    /// Test: `cli_token_prefers_github_token`, `cli_token_falls_back_to_gh`,
    /// `app_strategy_requires_credentials`.
    pub async fn resolve_token(
        self,
        client: &GithubClient,
        config: &ReviewConfig,
        owner: &str,
    ) -> Result<String, GithubError> {
        match self {
            AuthStrategy::Cli => resolve_cli_token(config, &SystemGhResolver),
            AuthStrategy::App => {
                resolve_app_token(
                    client,
                    config.github_app_id.as_deref(),
                    config.github_app_private_key.as_deref(),
                    &config.github_installations,
                    owner,
                )
                .await
            }
        }
    }
}

// ─── CLI token resolution ───────────────────────────────────────────────────────

/// Source of the `gh auth token` value (injectable for testing).
///
/// Why: the `gh auth token` fallback shells out to an external binary, which
/// must be stubbed in unit tests so the resolution-order logic is testable
/// without a real `gh` install or login.
/// What: a single method returning the token `gh` would print, or `None` if
/// `gh` is unavailable / not logged in.
/// Test: implemented by `SystemGhResolver` (real) and by fakes in unit tests.
pub trait GhTokenResolver {
    /// Return the token from `gh auth token`, or `None` if unavailable.
    fn gh_auth_token(&self) -> Option<String>;
}

/// Production `GhTokenResolver` that shells out to the `gh` CLI.
///
/// Why: developers commonly authenticate via `gh auth login` rather than
/// exporting a PAT; honouring that keeps the local UX friction-free (#582).
/// What: runs `gh auth token`, capturing stdout; returns the trimmed token on
/// success, `None` on any spawn/exit/empty-output failure (logged at debug).
/// Test: not unit-tested directly (requires a real `gh`); the resolution-order
/// logic is tested with a fake resolver.
pub struct SystemGhResolver;

impl GhTokenResolver for SystemGhResolver {
    fn gh_auth_token(&self) -> Option<String> {
        let output = std::process::Command::new("gh")
            .args(["auth", "token"])
            .output()
            .map_err(|e| tracing::debug!("`gh auth token` could not be spawned: {e}"))
            .ok()?;
        if !output.status.success() {
            tracing::debug!(
                status = ?output.status.code(),
                "`gh auth token` exited non-zero (not logged in?)"
            );
            return None;
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if token.is_empty() { None } else { Some(token) }
    }
}

/// Resolve a developer token in CLI mode using the #582 precedence order.
///
/// Why: factoring the precedence out of `AuthStrategy::resolve_token` lets the
/// `gh` shell-out be injected so the env→`gh` ordering is unit-testable.
/// What: returns `config.github_token` (populated from `GITHUB_TOKEN`) if set,
/// then `GH_TOKEN`, then `gh auth token` via `gh`, else `GithubError::MissingToken`
/// with a developer-actionable message.
/// Test: `cli_token_prefers_github_token`, `cli_token_uses_gh_token_env`,
/// `cli_token_falls_back_to_gh`, `cli_token_missing_errors`.
fn resolve_cli_token(
    config: &ReviewConfig,
    gh: &impl GhTokenResolver,
) -> Result<String, GithubError> {
    // 1. GITHUB_TOKEN (already captured into config.github_token at load time).
    if !config.github_token.is_empty() {
        tracing::debug!("using GITHUB_TOKEN for CLI GitHub auth");
        return Ok(config.github_token.clone());
    }
    // 2. GH_TOKEN (the `gh` CLI's own env var).
    if let Ok(gh_token) = std::env::var("GH_TOKEN")
        && !gh_token.trim().is_empty()
    {
        tracing::debug!("using GH_TOKEN for CLI GitHub auth");
        return Ok(gh_token.trim().to_string());
    }
    // 3. `gh auth token` shell-out.
    if let Some(token) = gh.gh_auth_token() {
        tracing::debug!("using `gh auth token` for CLI GitHub auth");
        return Ok(token);
    }
    Err(GithubError::MissingToken)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// A `GhTokenResolver` that returns a fixed value, for deterministic tests.
    struct FakeGh(Option<String>);
    impl GhTokenResolver for FakeGh {
        fn gh_auth_token(&self) -> Option<String> {
            self.0.clone()
        }
    }

    fn config_with_token(token: &str) -> ReviewConfig {
        let mut c = ReviewConfig::load(None);
        c.github_token = token.to_string();
        c
    }

    // ── Strategy selection ────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn select_cli_defaults_to_cli_strategy() {
        // SAFETY: test-only env mutation, serialised via #[serial].
        unsafe { std::env::remove_var(AUTH_MODE_ENV) };
        assert_eq!(AuthStrategy::select(RunMode::Cli, None), AuthStrategy::Cli);
    }

    #[test]
    #[serial]
    fn select_serve_defaults_to_app() {
        unsafe { std::env::remove_var(AUTH_MODE_ENV) };
        assert_eq!(
            AuthStrategy::select(RunMode::Serve, None),
            AuthStrategy::App
        );
    }

    #[test]
    #[serial]
    fn select_override_forces_app() {
        unsafe { std::env::remove_var(AUTH_MODE_ENV) };
        // Explicit flag forces App even in CLI mode (for local App testing).
        assert_eq!(
            AuthStrategy::select(RunMode::Cli, Some("app")),
            AuthStrategy::App
        );
    }

    #[test]
    #[serial]
    fn select_override_forces_cli() {
        unsafe { std::env::remove_var(AUTH_MODE_ENV) };
        // Explicit flag forces PAT even in serve mode (single-repo deploy).
        assert_eq!(
            AuthStrategy::select(RunMode::Serve, Some("pat")),
            AuthStrategy::Cli
        );
    }

    #[test]
    #[serial]
    fn select_env_forces_app() {
        unsafe { std::env::set_var(AUTH_MODE_ENV, "github_app") };
        assert_eq!(AuthStrategy::select(RunMode::Cli, None), AuthStrategy::App);
        unsafe { std::env::remove_var(AUTH_MODE_ENV) };
    }

    #[test]
    #[serial]
    fn select_flag_beats_env() {
        // The explicit flag must win over the env var.
        unsafe { std::env::set_var(AUTH_MODE_ENV, "app") };
        assert_eq!(
            AuthStrategy::select(RunMode::Serve, Some("cli")),
            AuthStrategy::Cli
        );
        unsafe { std::env::remove_var(AUTH_MODE_ENV) };
    }

    #[test]
    fn select_garbage_override_falls_through_to_mode() {
        // An unrecognised override is ignored; mode default applies.
        assert_eq!(
            AuthStrategy::select(RunMode::Cli, Some("nonsense")),
            AuthStrategy::Cli
        );
    }

    // ── CLI token resolution precedence ───────────────────────────────────────

    #[test]
    #[serial]
    fn cli_token_prefers_github_token() {
        unsafe { std::env::remove_var("GH_TOKEN") };
        let config = config_with_token("ghp_from_github_token"); // pragma: allowlist secret
        // Even if `gh` would return a token, GITHUB_TOKEN wins.
        let gh = FakeGh(Some("ghp_from_gh_cli".to_string())); // pragma: allowlist secret
        let token = resolve_cli_token(&config, &gh).expect("should resolve");
        assert_eq!(token, "ghp_from_github_token");
    }

    #[test]
    #[serial]
    fn cli_token_uses_gh_token_env() {
        // No GITHUB_TOKEN, but GH_TOKEN is set → GH_TOKEN wins over `gh` shell-out.
        unsafe { std::env::set_var("GH_TOKEN", "ghp_from_gh_token_env") }; // pragma: allowlist secret
        let config = config_with_token("");
        let gh = FakeGh(Some("ghp_from_gh_cli".to_string())); // pragma: allowlist secret
        let token = resolve_cli_token(&config, &gh).expect("should resolve");
        assert_eq!(token, "ghp_from_gh_token_env");
        unsafe { std::env::remove_var("GH_TOKEN") };
    }

    #[test]
    #[serial]
    fn cli_token_falls_back_to_gh() {
        // No GITHUB_TOKEN, no GH_TOKEN → `gh auth token` is consulted.
        unsafe { std::env::remove_var("GH_TOKEN") };
        let config = config_with_token("");
        let gh = FakeGh(Some("ghp_from_gh_cli".to_string())); // pragma: allowlist secret
        let token = resolve_cli_token(&config, &gh).expect("should resolve via gh");
        assert_eq!(token, "ghp_from_gh_cli");
    }

    #[test]
    #[serial]
    fn cli_token_missing_errors() {
        // Nothing available anywhere → MissingToken with a clear message.
        unsafe { std::env::remove_var("GH_TOKEN") };
        let config = config_with_token("");
        let gh = FakeGh(None);
        match resolve_cli_token(&config, &gh) {
            Err(GithubError::MissingToken) => {}
            other => panic!("expected MissingToken, got {other:?}"),
        }
    }

    // ── App strategy guard (network-free) ─────────────────────────────────────

    #[tokio::test]
    async fn app_strategy_requires_credentials() {
        // App strategy with no App credentials configured → Auth error.
        let mut config = ReviewConfig::load(None);
        config.github_app_id = None;
        config.github_app_private_key = None;
        let client = GithubClient::new();
        let result = AuthStrategy::App
            .resolve_token(&client, &config, "acme")
            .await;
        match result {
            Err(GithubError::Auth(_)) => {}
            other => panic!("expected Auth error without App creds, got {other:?}"),
        }
    }
}
