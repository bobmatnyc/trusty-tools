//! Handler for `trusty-search config get|set`.
//!
//! Why: operators need to retune memory limits on a live daemon without
//! paying the 86 MB embedder reload + warm-boot cost a full restart implies.
//! The daemon serves `search.config.get` and `search.config.set` on its
//! socket (the twins of `GET /config` and `PATCH /config`); this CLI surface
//! makes them discoverable from the shell.
//! What: two sub-subcommands.
//! - `trusty-search config get [<key>]` → `search.config.get`, print all keys
//!   or one.
//! - `trusty-search config set <key> <value>` → `search.config.set` with a
//!   single field; `0` / `off` / `none` / `disable` / `unlimited` disables the
//!   limit.
//!
//! Neither starts the daemon: retuning a stopped daemon has nothing to act on,
//! so a missing socket is an error naming it (#9214).
//!
//! Test: `config_get_reads_the_config_over_the_socket`,
//! `config_set_patches_one_key_over_the_socket`,
//! `config_fails_closed_when_the_socket_is_absent`.

use super::daemon_rpc;
use anyhow::{anyhow, Result};
use clap::{Subcommand, ValueEnum};
use colored::Colorize;
use serde_json::{json, Value};
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::admin::METHOD_CONFIG_SET;
use trusty_search::service::rpc::reads::METHOD_CONFIG_GET;

/// `trusty-search config` sub-subcommands.
///
/// Why: kept narrow on purpose — only `get` and `set` ship today. Future
/// keys (e.g. `embedding-cache`, `max-batch-size`) can be added without
/// changing the surface by extending [`ConfigKey`] and the daemon-side
/// `PATCH /config` handler.
/// What: clap derives the `config get` / `config set` argument tables.
#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Print the daemon's current configuration.
    ///
    /// Examples:
    ///   trusty-search config get
    ///   trusty-search config get memory-limit
    Get {
        /// Optional key to print (default: all keys)
        key: Option<ConfigKey>,
    },

    /// Update one configuration key on the running daemon.
    ///
    /// Value is the new limit in MB, or `0` / `off` / `none` / `disable` /
    /// `unlimited` to remove the limit.
    ///
    /// Examples:
    ///   trusty-search config set memory-limit 16384
    ///   trusty-search config set index-memory-limit 65536
    ///   trusty-search config set memory-limit off
    Set {
        /// Key to update
        key: ConfigKey,
        /// New value in MB, or 0/off/none/disable/unlimited to disable
        value: String,
    },

    /// Manage inference provider configuration (API keys) — the universal
    /// `config keys set/list/test/unset` surface shared by every trusty-*
    /// binary (epic #2400 Wave 1, #2405). Nested here (rather than as a
    /// top-level `Config(ConfigCommand)` mount) because `trusty-search`
    /// already owns `config` for daemon runtime settings (`get`/`set`
    /// memory-limit above) — see the "mount `keys` under an existing
    /// `config`" recipe in `trusty_common::inference::config`.
    Keys(trusty_common::inference::config::ConfigKeysCommand),
}

/// Supported configuration keys.
///
/// Why: keep the CLI surface narrow and typo-resistant. Clap's `ValueEnum`
/// derive gives us tab-completion and a clear error message for unknown keys.
/// What: kebab-case CLI tokens (`memory-limit`, `index-memory-limit`) that
/// map onto the JSON field names the daemon uses (`memory_limit_mb`,
/// `index_memory_limit_mb`).
/// Test: `tests::config_key_json_field`.
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum ConfigKey {
    /// Global daemon RSS soft ceiling (MB)
    MemoryLimit,
    /// Indexing-pipeline RSS soft ceiling (MB); falls back to memory-limit
    IndexMemoryLimit,
}

impl ConfigKey {
    /// JSON field name as exposed by the daemon's `/config` endpoint.
    ///
    /// Why: keeps the kebab-case CLI surface decoupled from the snake_case
    /// wire format so renaming one side never silently breaks the other.
    /// What: returns the exact key the daemon expects in the PATCH body and
    /// emits in the GET response.
    /// Test: `tests::config_key_json_field`.
    fn json_field(self) -> &'static str {
        match self {
            ConfigKey::MemoryLimit => "memory_limit_mb",
            ConfigKey::IndexMemoryLimit => "index_memory_limit_mb",
        }
    }

    /// User-facing label printed by `config get` output.
    fn display_name(self) -> &'static str {
        match self {
            ConfigKey::MemoryLimit => "memory-limit",
            ConfigKey::IndexMemoryLimit => "index-memory-limit",
        }
    }
}

/// Parse a CLI value string into `Option<u64>` (None = disable limit).
///
/// Why: operators want to disable a limit without remembering "0 means off",
/// so we also accept `off`, `none`, `disable`, and `unlimited`. Numeric
/// values are interpreted as megabytes.
/// What: case-insensitive match against the disable tokens; otherwise
/// `parse::<u64>()`. Returns a typed error so the caller can print a
/// friendly message that mentions the valid disable tokens.
/// Test: `tests::parse_value`.
fn parse_value(raw: &str) -> Result<Option<u64>> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "0" | "off" | "none" | "disable" | "disabled" | "unlimited"
    ) {
        return Ok(None);
    }
    trimmed
        .parse::<u64>()
        .map(Some)
        .map_err(|_| anyhow!("value must be a number in MB, or 0/off/none/disable/unlimited"))
}

/// Format an `Option<u64>` as human-friendly text.
///
/// An absent field is not a disabled limit: `null` is how the daemon says
/// "unlimited", so a missing key reads as unreported rather than as no cap.
fn fmt_mb(v: Option<&Value>) -> String {
    match v {
        // #9214: an absent field used to print "unlimited".
        None => "not reported by the daemon".to_string(),
        Some(Value::Null) => "unlimited".to_string(),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(mb) => format!("{mb} MB"),
            None => n.to_string(),
        },
        Some(other) => other.to_string(),
    }
}

/// Top-level handler for `trusty-search config`.
///
/// Why: mirrors the dispatch shape every other CLI subcommand uses, so
/// `main.rs` stays a thin clap-to-handler shim.
/// What: routes to [`handle_config_get`] or [`handle_config_set`] depending
/// on the parsed action. Both helpers return user-friendly errors via
/// `anyhow::bail!`.
/// Test: `config_get_reads_the_config_over_the_socket`,
/// `config_set_patches_one_key_over_the_socket`.
pub async fn handle_config(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Get { key } => handle_config_get(key).await,
        ConfigAction::Set { key, value } => handle_config_set(key, &value).await,
        ConfigAction::Keys(cmd) => cmd.run().await,
    }
}

/// The daemon's config — the body `GET /config` answered.
///
/// # Errors
///
/// When the socket is unreachable, or the daemon refuses the call.
async fn fetch_config(client: &DaemonClient) -> Result<Value> {
    daemon_rpc::call(client, METHOD_CONFIG_GET, json!({})).await
}

/// Patch the daemon's config with `body` and return the post-update values.
///
/// # Errors
///
/// When the socket is unreachable, or the daemon refuses the call.
async fn patch_config(client: &DaemonClient, body: Value) -> Result<Value> {
    daemon_rpc::call(client, METHOD_CONFIG_SET, body).await
}

/// `trusty-search config get` — print the daemon's current configuration.
async fn handle_config_get(key: Option<ConfigKey>) -> Result<()> {
    // #9214: the socket only, and no auto-start.
    let body = fetch_config(&DaemonClient::resolve()?).await?;

    if let Some(k) = key {
        let field = k.json_field();
        let v = body.get(field);
        println!("{}: {}", k.display_name().bold(), fmt_mb(v));
    } else {
        for k in [ConfigKey::MemoryLimit, ConfigKey::IndexMemoryLimit] {
            let v = body.get(k.json_field());
            println!("{}: {}", k.display_name().bold(), fmt_mb(v));
        }
    }
    Ok(())
}

/// `trusty-search config set <key> <value>` — patch the daemon's
/// configuration and print the post-update values.
async fn handle_config_set(key: ConfigKey, raw_value: &str) -> Result<()> {
    let parsed = parse_value(raw_value)?;
    // #9214: the socket only, and no auto-start.
    let client = DaemonClient::resolve()?;
    let new = patch_config(&client, patch_body(key, parsed)).await?;

    let pretty_after = match parsed {
        Some(n) => format!("{n} MB"),
        None => "unlimited".to_string(),
    };
    println!(
        "{} {} → {}",
        "✓".green(),
        key.display_name().bold(),
        pretty_after
    );
    println!();
    println!("{}", "Daemon configuration:".dimmed());
    for k in [ConfigKey::MemoryLimit, ConfigKey::IndexMemoryLimit] {
        let v = new.get(k.json_field());
        println!("  {}: {}", k.display_name(), fmt_mb(v));
    }
    Ok(())
}

/// The `search.config.set` body for one key: `None` (disable) is JSON `null`,
/// `Some(n)` a number.
fn patch_body(key: ConfigKey, value: Option<u64>) -> Value {
    match value {
        Some(n) => json!({ key.json_field(): n }),
        None => json!({ key.json_field(): Value::Null }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::mock_socket::mock_daemon;
    use std::sync::{Arc, Mutex};

    #[test]
    fn parse_value_accepts_numbers_and_disable_tokens() {
        // Why: the parser is the boundary between freeform CLI input and the
        // typed `Option<u64>` we send over the wire. Cover every disable
        // alias so a future contributor cannot regress one of them.
        assert_eq!(parse_value("16384").unwrap(), Some(16384));
        assert_eq!(parse_value(" 4096 ").unwrap(), Some(4096));
        for tok in [
            "0",
            "off",
            "OFF",
            "None",
            "disable",
            "disabled",
            "unlimited",
        ] {
            assert_eq!(parse_value(tok).unwrap(), None, "token: {tok}");
        }
        assert!(parse_value("not-a-number").is_err());
        assert!(parse_value("").is_err());
        assert!(parse_value("-5").is_err()); // u64 rejects negatives
    }

    #[test]
    fn config_key_json_field() {
        // Why: the CLI<->daemon contract hinges on these exact strings.
        // A typo here would silently desync the two sides.
        assert_eq!(ConfigKey::MemoryLimit.json_field(), "memory_limit_mb");
        assert_eq!(
            ConfigKey::IndexMemoryLimit.json_field(),
            "index_memory_limit_mb"
        );
        assert_eq!(ConfigKey::MemoryLimit.display_name(), "memory-limit");
        assert_eq!(
            ConfigKey::IndexMemoryLimit.display_name(),
            "index-memory-limit"
        );
    }

    #[test]
    fn fmt_mb_renders_null_as_unlimited() {
        assert_eq!(fmt_mb(None), "not reported by the daemon");
        assert_eq!(fmt_mb(Some(&Value::Null)), "unlimited");
        assert_eq!(fmt_mb(Some(&json!(4096))), "4096 MB");
    }

    /// #9214: `config get` reads `search.config.get` with empty params — the
    /// `GET /config` twin — and hands back the daemon's body.
    #[tokio::test]
    async fn config_get_reads_the_config_over_the_socket() {
        let daemon = mock_daemon(|method, params| {
            assert_eq!(method, METHOD_CONFIG_GET);
            assert_eq!(params, json!({}));
            Ok(json!({ "memory_limit_mb": 4096, "index_memory_limit_mb": null }))
        })
        .await;
        let body = fetch_config(&daemon.client).await.expect("read");
        assert_eq!(body["memory_limit_mb"], json!(4096));
    }

    /// #9214: `config set` sends the one-key patch as `search.config.set`'s
    /// params — a number, or `null` to disable — exactly the `PATCH /config`
    /// body.
    #[tokio::test]
    async fn config_set_patches_one_key_over_the_socket() {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log = Arc::clone(&seen);
        let daemon = mock_daemon(move |method, params| {
            assert_eq!(method, METHOD_CONFIG_SET);
            log.lock().expect("log").push(params);
            Ok(json!({ "memory_limit_mb": null, "index_memory_limit_mb": null }))
        })
        .await;
        for (key, value) in [
            (ConfigKey::MemoryLimit, Some(16384)),
            (ConfigKey::IndexMemoryLimit, None),
        ] {
            patch_config(&daemon.client, patch_body(key, value))
                .await
                .expect("patched");
        }
        assert_eq!(
            *seen.lock().expect("log"),
            vec![
                json!({ "memory_limit_mb": 16384 }),
                json!({ "index_memory_limit_mb": null }),
            ]
        );
    }

    /// #9214: an absent socket fails closed for both verbs, naming the socket
    /// and no URL; nothing is started.
    #[tokio::test]
    async fn config_fails_closed_when_the_socket_is_absent() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("absent.sock");
        let client = DaemonClient::at(&socket);
        let errors = [
            fetch_config(&client).await.expect_err("get"),
            patch_config(&client, json!({ "memory_limit_mb": 1 }))
                .await
                .expect_err("set"),
        ];
        for err in errors {
            let text = err.to_string();
            assert!(text.starts_with("could not reach daemon"), "{text}");
            assert!(text.contains(&socket.display().to_string()), "{text}");
            assert!(!text.contains("http://"), "{text}");
        }
        assert!(!socket.exists(), "no daemon may be started");
    }
}
