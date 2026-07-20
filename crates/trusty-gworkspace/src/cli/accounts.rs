//! `accounts` subcommand handlers: list / default / remove.
//!
//! Why: Once tokens can be minted natively, users need a way to inspect and
//! manage the profiles in `tokens.json` without hand-editing JSON.
//! What: Thin stdout-printing wrappers over the shared, lock-guarded
//! [`TokenStorage::set_default_profile`] / [`TokenStorage::remove_profile`]
//! (the same methods the `set_default_account` / `remove_account` MCP tools
//! use — see `api::services::accounts`) so both surfaces get identical
//! mutation semantics, including the remove-the-default reassignment
//! (#3502). All output goes to stdout (human-facing CLI), logs to stderr.
//! Test: `set_default_moves_flag` and `remove_deletes_profile` exercise this
//! wrapper end-to-end against a temp storage; the mutation logic itself is
//! unit-tested in `api::auth::storage::tests`.

use anyhow::Result;

use crate::api::auth::TokenStorage;
use crate::api::auth::oauth::profile_client_source;

/// Print all stored profiles as an aligned table.
///
/// Why: The primary discovery command — shows which `account` values the MCP
/// tools accept, which one is the default, and (issue #3518) which OAuth
/// client each profile authorizes/refreshes with, so a per-profile-client
/// misconfiguration is diagnosable from this one command.
/// What: Loads via [`TokenStorage::list_accounts`] and prints one row per
/// profile (name, email, default marker, client source); prints a hint when
/// empty.
/// Test: Output is cosmetic; the underlying listing is covered by storage
/// round-trip tests.
pub fn list(storage: &TokenStorage) -> Result<()> {
    let rows = storage.list_accounts()?;
    if rows.is_empty() {
        println!("No accounts found. Run `trusty-gworkspace-mcp setup` to authorize one.");
        return Ok(());
    }
    println!("{:<24} {:<32} {:<8} CLIENT", "PROFILE", "EMAIL", "DEFAULT");
    for (name, email, is_default) in rows {
        let client = profile_client_source(&name).label();
        println!(
            "{:<24} {:<32} {:<8} {client}",
            name,
            email.unwrap_or_else(|| "-".to_string()),
            if is_default { "*" } else { "" }
        );
    }
    Ok(())
}

/// Mark `name` as the default profile (clearing any other default).
///
/// Why: Users with multiple accounts need to choose which one tools use when
/// no explicit `account` is passed.
/// What: Delegates to [`TokenStorage::set_default_profile`] (errors if
/// `name` is absent) and prints the confirmation.
/// Test: `set_default_moves_flag`.
pub fn set_default(storage: &TokenStorage, name: &str) -> Result<()> {
    storage.set_default_profile(name)?;
    println!("Default profile set to '{name}'.");
    Ok(())
}

/// Remove the named profile from storage.
///
/// Why: Revoking or cleaning up an account should not require editing JSON.
/// What: Delegates to [`TokenStorage::remove_profile`] (errors if absent;
/// reassigns the default to another remaining profile if the removed one was
/// it) and prints the outcome. Note: this only forgets the local token; it
/// does not revoke Google's grant.
/// Test: `remove_deletes_profile`.
pub fn remove(storage: &TokenStorage, name: &str) -> Result<()> {
    let outcome = storage.remove_profile(name)?;
    match &outcome.reassigned_default {
        Some(new_default) => println!(
            "Removed profile '{name}'. (Google's grant is not revoked.) \
             Default profile reassigned to '{new_default}'."
        ),
        None => println!("Removed profile '{name}'. (Google's grant is not revoked.)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::auth::models::{OAuthToken, StoredToken, TokenMetadata};
    use chrono::{Duration, Utc};
    use std::collections::HashMap;

    fn temp_storage() -> TokenStorage {
        let dir = std::env::temp_dir().join(format!("gw-accounts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        TokenStorage::with_path(dir.join("tokens.json"))
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
                expires_at: Utc::now() + Duration::seconds(3600),
                scopes: vec!["openid".into()],
                token_type: "Bearer".into(),
            },
        }
    }

    fn seed(storage: &TokenStorage) {
        let mut map = HashMap::new();
        map.insert("a".to_string(), entry("a", true));
        map.insert("b".to_string(), entry("b", false));
        storage.save(&map).unwrap();
    }

    #[test]
    fn set_default_moves_flag() {
        let s = temp_storage();
        seed(&s);
        set_default(&s, "b").unwrap();
        let all = s.load().unwrap();
        assert!(!all["a"].metadata.is_default);
        assert!(all["b"].metadata.is_default);
    }

    #[test]
    fn set_default_rejects_unknown() {
        let s = temp_storage();
        seed(&s);
        assert!(set_default(&s, "missing").is_err());
    }

    #[test]
    fn remove_deletes_profile() {
        let s = temp_storage();
        seed(&s);
        remove(&s, "b").unwrap();
        let all = s.load().unwrap();
        assert!(!all.contains_key("b"));
        assert!(all.contains_key("a"));
        assert!(remove(&s, "b").is_err(), "second remove must error");
    }
}
