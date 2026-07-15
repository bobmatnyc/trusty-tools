//! BaseClient: authenticated HTTP wrapper over the Slack Web API (scaffold).
//!
//! Why: Every future Slack method (`chat.postMessage`, `conversations.history`,
//! …) needs the same token + JSON-envelope handling. Centralising it here
//! mirrors `trusty-gworkspace::api::client::BaseClient` and lets us add
//! tracing / rate-limiting / retry in one place once live calls land.
//! What: Holds a `reqwest::Client` and an optional resolved token. This is a
//! *stub*: `new()` builds the HTTP client and records whether a token is
//! present, but performs no network I/O and no interactive auth. The live
//! request methods are intentionally absent until the token decision is made.
//! Test: `new_succeeds_without_token` asserts construction works with no
//! credentials configured (CI-safe, matching the gworkspace smoke test).

use anyhow::{Context, Result};

/// Authenticated Slack Web API client (scaffold).
///
/// Why: One handle per process; a future live implementation will send bearer
/// tokens on every request and unwrap Slack's `{"ok": bool, ...}` envelope.
/// What: Wraps a `reqwest::Client` and an `Option<String>` token placeholder.
/// `token` is `None` when no credential is configured — construction still
/// succeeds so `tools/list` and the MCP handshake work without secrets.
/// Test: Smoke test asserts construction succeeds with no env vars set.
pub struct BaseClient {
    /// Shared HTTP client. Unused until live methods land; retained so the
    /// constructor signature and connection-pool ownership match the final
    /// shape and future handlers need no wiring changes.
    #[allow(dead_code)]
    http: reqwest::Client,
    /// Resolved Slack token, or `None` when unconfigured.
    #[allow(dead_code)]
    token: Option<String>,
}

impl BaseClient {
    /// Construct with a shared HTTP client and a best-effort token lookup.
    ///
    /// Why: The binary builds exactly one client at startup; keeping the
    /// constructor infallible-on-missing-token (returns `Ok` with `token:
    /// None`) means the server boots for `tools/list` without credentials.
    /// What: Builds the `reqwest::Client`; resolves the token as a
    /// placeholder. Returns `Err` only if the HTTP client fails to build.
    ///
    /// TODO(auth, deferred): resolve the token via
    /// `trusty_common::inference::credentials` (its `resolve_key` /
    /// `default_store` resolver — env var / file store / OS keyring) once the
    /// bot-vs-user token decision is made. Do NOT implement live auth here yet;
    /// the current lookup is a documented placeholder only. See ADR-0014.
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("trusty-slack/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("build reqwest client")?;
        // Placeholder token resolution — deferred, see the TODO above.
        let token = std::env::var(crate::api::constants::SLACK_TOKEN_ENV).ok();
        Ok(Self { http, token })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_succeeds_without_token() {
        // Constructing must not require any Slack credentials, so the MCP
        // handshake and tools/list work in CI without secrets.
        let client = BaseClient::new().expect("construct base client");
        // Field is intentionally private; presence is asserted via Debug-free
        // construction success. Nothing else to assert on a stub.
        let _ = client;
    }
}
