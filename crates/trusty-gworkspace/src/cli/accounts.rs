//! `accounts` subcommand handlers: list / default / remove.
//!
//! Why: Once tokens can be minted natively, users need a way to inspect and
//! manage the profiles in `tokens.json` without hand-editing JSON.
//! What: Free functions over [`TokenStorage`] that load the map, mutate it,
//! and save. All output goes to stdout (human-facing CLI), logs to stderr.
//! Test: `set_default_moves_flag` and `remove_deletes_profile` exercise the
//! mutation logic against a temp storage.

use anyhow::{Result, anyhow};

use crate::api::auth::TokenStorage;

/// Print all stored profiles as an aligned table.
///
/// Why: The primary discovery command — shows which `account` values the MCP
/// tools accept and which one is the default.
/// What: Loads via [`TokenStorage::list_accounts`] and prints one row per
/// profile; prints a hint when empty.
/// Test: Output is cosmetic; the underlying listing is covered by storage
/// round-trip tests.
pub fn list(storage: &TokenStorage) -> Result<()> {
    let rows = storage.list_accounts()?;
    if rows.is_empty() {
        println!("No accounts found. Run `trusty-gworkspace-mcp setup` to authorize one.");
        return Ok(());
    }
    println!("{:<24} {:<32} DEFAULT", "PROFILE", "EMAIL");
    for (name, email, is_default) in rows {
        println!(
            "{:<24} {:<32} {}",
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
/// What: Loads the map, errors if `name` is absent, clears all defaults, sets
/// the target's `is_default`, and saves.
/// Test: `set_default_moves_flag`.
pub fn set_default(storage: &TokenStorage, name: &str) -> Result<()> {
    let mut all = storage.load()?;
    if !all.contains_key(name) {
        return Err(anyhow!("no profile named '{name}'"));
    }
    for entry in all.values_mut() {
        entry.metadata.is_default = false;
    }
    if let Some(entry) = all.get_mut(name) {
        entry.metadata.is_default = true;
    }
    storage.save(&all)?;
    println!("Default profile set to '{name}'.");
    Ok(())
}

/// Remove the named profile from storage.
///
/// Why: Revoking or cleaning up an account should not require editing JSON.
/// What: Loads the map, errors if absent, removes the entry, and saves. Note:
/// this only forgets the local token; it does not revoke Google's grant.
/// Test: `remove_deletes_profile`.
pub fn remove(storage: &TokenStorage, name: &str) -> Result<()> {
    let mut all = storage.load()?;
    if all.remove(name).is_none() {
        return Err(anyhow!("no profile named '{name}'"));
    }
    storage.save(&all)?;
    println!("Removed profile '{name}'. (Google's grant is not revoked.)");
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
