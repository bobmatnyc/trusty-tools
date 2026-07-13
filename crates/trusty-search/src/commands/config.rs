//! Handler for `trusty-search config get|set`.
//!
//! Why: operators need to retune memory limits on a live daemon without
//! paying the 86 MB embedder reload + warm-boot cost a full restart implies.
//! The daemon exposes `GET /config` and `PATCH /config`; this CLI surface
//! makes those endpoints discoverable from the shell.
//! What: two sub-subcommands.
//! - `trusty-search config get [<key>]` → `GET /config`, print all keys or one.
//! - `trusty-search config set <key> <value>` → `PATCH /config` with a single
//!   field; `0` / `off` / `none` / `disable` / `unlimited` disables the limit.
//!
//! Test: covered by `tests::parse_value` and end-to-end via
//! `cargo run -- config get` against a live daemon.

use super::daemon_utils::daemon_base_url;
use anyhow::{anyhow, bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use colored::Colorize;
use serde_json::{json, Value};

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
fn fmt_mb(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "unlimited".to_string(),
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
/// Test: end-to-end via `cargo run -- config get` against a running daemon.
pub async fn handle_config(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Get { key } => handle_config_get(key).await,
        ConfigAction::Set { key, value } => handle_config_set(key, &value).await,
        ConfigAction::Keys(cmd) => cmd.run().await,
    }
}

/// `trusty-search config get` — print the daemon's current configuration.
async fn handle_config_get(key: Option<ConfigKey>) -> Result<()> {
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;
    let url = format!("{base}/config");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(daemon_unreachable_hint)?;
    if !resp.status().is_success() {
        bail!("daemon returned HTTP {} from {}", resp.status(), url);
    }
    let body: Value = resp
        .json()
        .await
        .context("daemon returned invalid JSON from /config")?;

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
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;
    let url = format!("{base}/config");

    // Serialise `None` as JSON null (disable) and `Some(n)` as a number.
    // Build the body with serde_json::json! so the field name stays in one
    // place per ConfigKey arm.
    let body = match parsed {
        Some(n) => json!({ key.json_field(): n }),
        None => json!({ key.json_field(): Value::Null }),
    };

    let resp = client
        .patch(&url)
        .json(&body)
        .send()
        .await
        .with_context(daemon_unreachable_hint)?;
    if !resp.status().is_success() {
        bail!("daemon returned HTTP {} from {}", resp.status(), url);
    }
    let new: Value = resp
        .json()
        .await
        .context("daemon returned invalid JSON from /config")?;

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

/// Construct the canonical "daemon is not running" hint used when reqwest
/// cannot reach the configured base URL.
///
/// Why: a connection-refused error from reqwest is opaque on its own; the
/// most common cause by far is that the daemon is not running. Surface the
/// remediation (`trusty-search start`) directly in the error chain so the
/// operator does not have to guess.
fn daemon_unreachable_hint() -> &'static str {
    "daemon is not running (no port.lock found, or daemon unreachable). \
     Start it with `trusty-search start`."
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(fmt_mb(None), "unlimited");
        assert_eq!(fmt_mb(Some(&Value::Null)), "unlimited");
        assert_eq!(fmt_mb(Some(&json!(4096))), "4096 MB");
    }
}
