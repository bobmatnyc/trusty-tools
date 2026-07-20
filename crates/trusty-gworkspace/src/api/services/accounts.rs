//! Account / profile management.
//!
//! Why: Users with multiple Google accounts need to discover, switch, remove,
//! and add profiles without shelling out to the `trusty-gworkspace-mcp`
//! CLI (issue #3503). `list_accounts` was the only MCP-exposed surface;
//! `set_default_account` / `remove_account` / `add_account` close that gap.
//! `add_account` also accepts an optional `oauth_client_path` (issue #3518)
//! so a profile can authorize against its OWN OAuth client instead of the
//! shared global one — see its doc comment.
//! What: `set_default_account` / `remove_account` wrap the same lock-guarded
//! `TokenStorage` methods the CLI uses (`api::auth::storage`, #3502) so both
//! surfaces share one mutation implementation. `add_account` reuses the
//! native PKCE consent flow (`api::auth::oauth::flow::run_consent_with`) —
//! see its doc comment for the blocking-call design and why. `accounts_json`
//! labels every profile with which OAuth client it uses
//! (`oauth::profile_client_source`).
//! Test: Indirect — covered by storage tests plus the argument-validation
//! smoke tests below.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::api::auth::oauth::{self, DefaultMode, flow};
use crate::api::client::BaseClient;
use crate::api::services::{opt_str, require_str};

/// Build the `[{name, email, is_default, client}, ...]` JSON array from
/// storage.
///
/// Why: Shared by `list_accounts` and every mutating tool below so each
/// response includes an up-to-date account list without repeating the
/// row-to-JSON mapping. `client` (issue #3518) reports `"global"` or
/// `"per-profile (<path>)"` so a per-profile-client misconfiguration is
/// diagnosable from the same response, without a separate `doctor` round trip.
/// What: Reads via `TokenStorage::list_accounts` plus
/// `oauth::profile_client_source` per profile (no network).
/// Test: Covered indirectly via `TokenStorage` storage round-trip tests.
fn accounts_json(client: &BaseClient) -> Result<Vec<Value>> {
    let rows = client.storage().list_accounts()?;
    Ok(rows
        .into_iter()
        .map(|(name, email, is_default)| {
            let client_label = oauth::profile_client_source(&name).label();
            json!({
                "name": name,
                "email": email,
                "is_default": is_default,
                "client": client_label,
            })
        })
        .collect())
}

/// Why: Enumerate authenticated Google profiles stored locally so the model can pick one.
/// What: Returns `{accounts: [{name, email, is_default}]}` read from `TokenStorage` — no network.
/// Test: Covered indirectly via `TokenStorage` storage round-trip test.
pub async fn list_accounts(client: &BaseClient, _args: Value) -> Result<Value> {
    Ok(json!({ "accounts": accounts_json(client)? }))
}

/// Why: Lets an agent switch which profile MCP tools use by default without
/// shelling out to `trusty-gworkspace-mcp accounts default`.
/// What: Validates `name` exists (via `TokenStorage::set_default_profile`,
/// shared with the CLI), then returns the new default plus the full account
/// list. No network call.
/// Test: `set_default_account_switches_default`,
/// `set_default_account_rejects_unknown_profile`.
pub async fn set_default_account(client: &BaseClient, args: Value) -> Result<Value> {
    let name = require_str(&args, "name")?;
    client.storage().set_default_profile(name)?;
    Ok(json!({
        "default": name,
        "accounts": accounts_json(client)?,
    }))
}

/// Why: Lets an agent clean up a stale/revoked profile without shelling out
/// to the CLI, and — unlike the CLI's original behavior — never silently
/// orphans the default (issue #3502).
/// What: Wraps `TokenStorage::remove_profile` (shared with the CLI); returns
/// which profile was removed, which remaining profile inherited the default
/// role (if any), and the updated account list. Does not revoke Google's
/// grant, matching the CLI's `remove`. No network call.
/// Test: `remove_account_reassigns_default`, `remove_account_rejects_unknown_profile`.
pub async fn remove_account(client: &BaseClient, args: Value) -> Result<Value> {
    let name = require_str(&args, "name")?;
    let outcome = client.storage().remove_profile(name)?;
    Ok(json!({
        "removed": outcome.removed,
        "reassigned_default": outcome.reassigned_default,
        "accounts": accounts_json(client)?,
    }))
}

/// Bounded wait for the `add_account` MCP tool's browser-consent step.
///
/// Why: The interactive CLI (`setup`) blocks up to 5 minutes
/// (`oauth::flow::CONSENT_TIMEOUT`) waiting for the user to finish consent in
/// a terminal session where that's expected. An MCP tool call is different:
/// many MCP clients bound how long a single call may run, and a hung call
/// blocks the whole exchange with no way for the agent to do anything else
/// meanwhile. 60s is long enough for a human who already has the URL open to
/// click through Google's consent screen, short enough to stay well inside
/// typical client-side tool-call timeouts. See `add_account`'s doc comment
/// for the full design tradeoff (flagged for owner review).
const ADD_ACCOUNT_TIMEOUT: Duration = Duration::from_secs(60);

/// Lower/upper bounds for the optional `timeout_secs` override.
const ADD_ACCOUNT_TIMEOUT_MIN_SECS: u64 = 10;
const ADD_ACCOUNT_TIMEOUT_MAX_SECS: u64 = 90;

/// Initiate the native OAuth consent flow to authorize a new (or re-auth an
/// existing) Google account profile.
///
/// Why (design choice — flagged for owner review): completing consent
/// requires a human in a browser, which an MCP tool call can't do
/// synchronously end-to-end. Two shapes were considered: (a) return the URL
/// immediately, run the callback listener in a background task, and expose a
/// separate poll/status tool; or (b) a single bounded blocking call that
/// returns the URL either way (success or timed-out) so the caller can
/// relay/retry. This picks (b): it reuses `oauth::flow::run_consent_with`
/// completely as-is (zero new machinery — no background task registry, no
/// status store, no cancellation handling to get right) and mirrors the
/// crate's existing `setup --print-url` mode, just with a much shorter,
/// MCP-call-appropriate timeout ([`ADD_ACCOUNT_TIMEOUT`], overridable via
/// `timeout_secs` within [`ADD_ACCOUNT_TIMEOUT_MIN_SECS`],
/// [`ADD_ACCOUNT_TIMEOUT_MAX_SECS`]) instead of the CLI's 5 minutes. The
/// tradeoff: the tool call itself blocks for up to that long, so a client
/// with a shorter built-in tool-call timeout would see the call fail even
/// though the flow is still safe to retry (no partial/corrupt token is ever
/// written — `run_consent_with` only persists after a successful token
/// exchange). Browser auto-launch is intentionally disabled here (unlike the
/// CLI default) since an MCP server may run in a context with no reachable
/// display for the calling human; the URL is always returned in the response
/// so the agent can relay it.
/// What: Args: `profile` (optional, defaults to the shared default profile
/// name), `make_default` / `no_default` (mutually exclusive, mirrors `setup`
/// flags; default is `DefaultMode::Auto`), `timeout_secs` (optional, clamped
/// to the bounds above), `oauth_client_path` (optional, issue #3518 — a FILE
/// PATH to a JSON file holding this profile's OWN OAuth client; never a raw
/// client_id/secret, so no secret material ever appears in a tool call or its
/// logs). When `oauth_client_path` is given it is validated and persisted to
/// `~/.gworkspace-mcp/clients/<profile>.json` BEFORE the consent flow runs
/// (fast-failing on a missing/malformed file with no network call), so the
/// authorization that follows uses that client, and every later refresh for
/// this profile reuses it automatically. Returns
/// `{"status": "authorized", "auth_url", ...}` on success, or
/// `{"status": "timed_out", "auth_url", "message"}` if the browser step
/// didn't complete in time — the caller can retry `add_account` for a fresh
/// URL. Any other failure (e.g. missing OAuth client credentials, or an
/// invalid `oauth_client_path`) propagates as a tool error.
/// Test: `add_account_rejects_conflicting_default_flags`,
/// `add_account_clamps_timeout_bounds`,
/// `add_account_rejects_invalid_oauth_client_path_before_consent`.
pub async fn add_account(client: &BaseClient, args: Value) -> Result<Value> {
    let profile = flow::effective_profile(opt_str(&args, "profile"));
    let no_default = args
        .get("no_default")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let make_default = args
        .get("make_default")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if no_default && make_default {
        return Err(anyhow!(
            "no_default and make_default are mutually exclusive"
        ));
    }
    let mode = if no_default {
        DefaultMode::Never
    } else if make_default {
        DefaultMode::Force
    } else {
        DefaultMode::Auto
    };
    let timeout = clamp_timeout(args.get("timeout_secs").and_then(Value::as_u64));

    if let Some(path) = opt_str(&args, "oauth_client_path") {
        oauth::persist_profile_client(&profile, Path::new(path))
            .with_context(|| format!("persist OAuth client for profile '{profile}'"))?;
    }

    // `std::sync::Mutex` (not `RefCell`) because the closure below is held
    // across an `.await` inside a `Send` future (`run_stdio_loop` requires
    // it) — `&Mutex<String>` is `Send`, `&RefCell<String>` is not.
    let captured_url = std::sync::Mutex::new(String::new());
    let outcome = flow::run_consent_with(client.storage(), &profile, mode, false, timeout, |url| {
        *captured_url.lock().unwrap_or_else(|p| p.into_inner()) = url.to_string();
    })
    .await;
    let auth_url = captured_url.into_inner().unwrap_or_else(|p| p.into_inner());

    match outcome {
        Ok(o) => Ok(json!({
            "status": "authorized",
            "auth_url": auth_url,
            "profile": o.profile,
            "email": o.email,
            "default_applied": o.default_applied,
        })),
        Err(e) if e.to_string().contains("timed out") => Ok(json!({
            "status": "timed_out",
            "auth_url": auth_url,
            "message": format!(
                "Consent was not completed within {}s using this URL. Call add_account again for a fresh URL and retry.",
                timeout.as_secs()
            ),
        })),
        Err(e) => Err(e),
    }
}

/// Clamp an optional caller-supplied timeout into the safe MCP-call bounds.
///
/// Why: Isolated as a pure function so the clamping rule is directly
/// unit-testable without exercising the full (network-touching) consent flow.
/// What: `None` -> [`ADD_ACCOUNT_TIMEOUT`]; otherwise clamps into
/// `[ADD_ACCOUNT_TIMEOUT_MIN_SECS, ADD_ACCOUNT_TIMEOUT_MAX_SECS]`.
/// Test: `add_account_clamps_timeout_bounds`.
fn clamp_timeout(requested_secs: Option<u64>) -> Duration {
    match requested_secs {
        None => ADD_ACCOUNT_TIMEOUT,
        Some(secs) => Duration::from_secs(
            secs.clamp(ADD_ACCOUNT_TIMEOUT_MIN_SECS, ADD_ACCOUNT_TIMEOUT_MAX_SECS),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::auth::TokenStorage;
    use crate::api::auth::models::{OAuthToken, StoredToken, TokenMetadata};
    use crate::api::auth::test_support::{EnvGuard, fresh_temp_home};
    use chrono::{Duration as ChronoDuration, Utc};
    use serial_test::serial;
    use std::collections::HashMap;

    fn test_client() -> BaseClient {
        let dir = std::env::temp_dir().join(format!("gw-accounts-svc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        BaseClient::for_test(TokenStorage::with_path(dir.join("tokens.json")))
    }

    fn entry(name: &str, is_default: bool) -> StoredToken {
        StoredToken {
            version: 1,
            metadata: TokenMetadata {
                service_name: name.to_string(),
                provider: "google".into(),
                created_at: Utc::now(),
                last_refreshed: None,
                email: Some(format!("{name}@example.com")),
                is_default,
            },
            token: OAuthToken {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at: Utc::now() + ChronoDuration::seconds(3600),
                scopes: vec!["openid".into()],
                token_type: "Bearer".into(),
            },
        }
    }

    fn seed(client: &BaseClient) {
        let mut map = HashMap::new();
        map.insert("a".to_string(), entry("a", true));
        map.insert("b".to_string(), entry("b", false));
        client.storage().save(&map).unwrap();
    }

    #[tokio::test]
    async fn set_default_account_switches_default() {
        let client = test_client();
        seed(&client);

        let result = set_default_account(&client, json!({ "name": "b" }))
            .await
            .unwrap();
        assert_eq!(result["default"], "b");
        let accounts = result["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 2);
    }

    #[tokio::test]
    async fn set_default_account_rejects_unknown_profile() {
        let client = test_client();
        seed(&client);

        let err = set_default_account(&client, json!({ "name": "missing" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[tokio::test]
    async fn remove_account_reassigns_default() {
        let client = test_client();
        seed(&client);

        let result = remove_account(&client, json!({ "name": "a" }))
            .await
            .unwrap();
        assert_eq!(result["removed"], "a");
        assert_eq!(result["reassigned_default"], "b");
        let accounts = result["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 1);
    }

    #[tokio::test]
    async fn remove_account_rejects_unknown_profile() {
        let client = test_client();
        seed(&client);

        let err = remove_account(&client, json!({ "name": "missing" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[tokio::test]
    async fn add_account_rejects_conflicting_default_flags() {
        let client = test_client();
        let err = add_account(&client, json!({ "no_default": true, "make_default": true }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn add_account_clamps_timeout_bounds() {
        assert_eq!(clamp_timeout(None), ADD_ACCOUNT_TIMEOUT);
        assert_eq!(
            clamp_timeout(Some(1)),
            Duration::from_secs(ADD_ACCOUNT_TIMEOUT_MIN_SECS)
        );
        assert_eq!(
            clamp_timeout(Some(10_000)),
            Duration::from_secs(ADD_ACCOUNT_TIMEOUT_MAX_SECS)
        );
        assert_eq!(clamp_timeout(Some(45)), Duration::from_secs(45));
    }

    #[tokio::test]
    #[serial]
    async fn add_account_rejects_invalid_oauth_client_path_before_consent() {
        // Issue #3518: an invalid `oauth_client_path` must fail fast — no
        // network call, no browser, no partial file — before entering the
        // (slow, network-touching) consent flow at all.
        let _guard = EnvGuard::capture(&["HOME"]);
        let home = fresh_temp_home("add-account-bad-client");
        // SAFETY: serialised via #[serial]; EnvGuard restores on drop.
        unsafe { std::env::set_var("HOME", &home) };

        let client = test_client();
        let missing = home.join("does-not-exist.json");
        let err = add_account(
            &client,
            json!({
                "profile": "newprof",
                "oauth_client_path": missing.to_string_lossy(),
            }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("OAuth client"), "{err}");
        assert!(
            !oauth::profile_client_path("newprof").exists(),
            "a failed persist must never leave a partial per-profile client file"
        );
    }

    #[tokio::test]
    #[serial]
    async fn list_accounts_labels_client_source() {
        let _guard = EnvGuard::capture(&["HOME"]);
        let home = fresh_temp_home("list-accounts-client-label");
        // SAFETY: see above.
        unsafe { std::env::set_var("HOME", &home) };

        let client = test_client();
        seed(&client);
        // Give profile "a" its own client; "b" keeps using the global one.
        let source_dir = std::env::temp_dir().join(format!("gw-src-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("client.json");
        std::fs::write(&source, r#"{"client_id":"id","client_secret":"secret"}"#).unwrap();
        oauth::persist_profile_client("a", &source).unwrap();

        let result = list_accounts(&client, json!({})).await.unwrap();
        let accounts = result["accounts"].as_array().unwrap();
        let a = accounts.iter().find(|v| v["name"] == "a").unwrap();
        let b = accounts.iter().find(|v| v["name"] == "b").unwrap();
        assert!(
            a["client"].as_str().unwrap().starts_with("per-profile ("),
            "{a}"
        );
        assert_eq!(b["client"], "global");
    }
}
