//! Unit tests for the `storage` module.
//!
//! Why: split out of `storage/mod.rs` to keep the production file under the
//! 500-SLOC cap (mirrors the `oauth/flow/mod.rs` + `flow/tests.rs` split).
//! What: exercises `TokenStorage` load/save/permissions, the two-store
//! precedence and shadow warning (#8539), `TokenStorage::update`'s write
//! routing and concurrency guard, and the
//! remove/default-reassignment pure helper.
//! Test: this file IS the test module for `storage`.

use super::*;

#[test]
#[cfg(unix)]
fn save_restricts_permissions_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("gw-storage-perms-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tokens.json");
    let storage = TokenStorage::with_path(path.clone());

    storage.save(&HashMap::new()).expect("save");

    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "tokens.json must be owner-read/write only, got {:o}",
        mode & 0o777
    );
}

#[test]
fn save_still_round_trips_content() {
    // Guards against the permissions change accidentally altering the
    // byte-serde-compatible wire format.
    let dir = std::env::temp_dir().join(format!("gw-storage-rt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = TokenStorage::with_path(dir.join("tokens.json"));

    let mut tokens = HashMap::new();
    tokens.insert(
        "primary".to_string(),
        StoredToken {
            version: 1,
            metadata: crate::api::auth::models::TokenMetadata {
                service_name: "primary".into(),
                provider: "google".into(),
                created_at: chrono::Utc::now(),
                last_refreshed: None,
                email: Some("user@example.com".into()),
                is_default: true,
            },
            token: crate::api::auth::models::OAuthToken {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at: chrono::Utc::now() + chrono::Duration::seconds(3600),
                scopes: vec!["openid".into()],
                token_type: "Bearer".into(),
            },
        },
    );

    storage.save(&tokens).expect("save");
    let loaded = storage.load().expect("load");
    assert_eq!(
        loaded["primary"].metadata.email.as_deref(),
        Some("user@example.com")
    );
    assert!(loaded["primary"].metadata.is_default);
}

/// Build a `StoredToken` expiring `expires_in_secs` from now (negative =
/// already expired).
fn make_stored(expires_in_secs: i64) -> StoredToken {
    StoredToken {
        version: 1,
        metadata: crate::api::auth::models::TokenMetadata {
            service_name: "test".into(),
            provider: "google".into(),
            created_at: chrono::Utc::now(),
            last_refreshed: None,
            email: Some("user@example.com".into()),
            is_default: false,
        },
        token: crate::api::auth::models::OAuthToken {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(expires_in_secs),
            scopes: vec!["openid".into()],
            token_type: "Bearer".into(),
        },
    }
}

const GMAIL_MODIFY: &str = "https://www.googleapis.com/auth/gmail.modify";
const GMAIL_SETTINGS: &str = "https://www.googleapis.com/auth/gmail.settings.basic";
const PROJECT_ACCESS: &str = "project-fixture-access";
const USER_ACCESS: &str = "user-fixture-access";

/// A `work` entry holding `scopes`, issued `issued_secs_ago` seconds ago and
/// still valid, with fake (non-provider-shaped) token values.
fn scoped_entry(scopes: &[&str], issued_secs_ago: i64, access: &str) -> StoredToken {
    let mut entry = make_stored(3600 - issued_secs_ago);
    entry.metadata.created_at = chrono::Utc::now() - chrono::Duration::seconds(issued_secs_ago);
    entry.token.access_token = access.into();
    entry.token.refresh_token = Some(format!("{access}-refresh"));
    entry.token.scopes = scopes.iter().map(|s| (*s).to_string()).collect();
    entry
}

/// A two-tier temp `TokenStorage` (separate user and project dirs), each
/// store seeded with the given map. Returns the storage and both paths.
fn two_tier(
    label: &str,
    user_tokens: HashMap<String, StoredToken>,
    project_tokens: HashMap<String, StoredToken>,
) -> (TokenStorage, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("gw-storage-{label}-{}", uuid::Uuid::new_v4()));
    let user_path = dir.join("user").join("tokens.json");
    let project_path = dir.join("project").join("tokens.json");
    TokenStorage::with_path(user_path.clone())
        .save(&user_tokens)
        .unwrap();
    TokenStorage::with_path(project_path.clone())
        .save(&project_tokens)
        .unwrap();
    let mut storage = TokenStorage::with_path(user_path.clone());
    storage.project_path = Some(project_path.clone());
    (storage, user_path, project_path)
}

/// The #8539 shape: an unexpired project entry minted before a re-consent,
/// lacking `gmail.settings.basic`, beside a fresh user entry that has it.
fn scope_shadowed(label: &str) -> (TokenStorage, PathBuf, PathBuf) {
    let user = HashMap::from([
        (
            "work".to_string(),
            scoped_entry(&[GMAIL_MODIFY, GMAIL_SETTINGS], 600, USER_ACCESS),
        ),
        (
            "personal".to_string(),
            scoped_entry(&[GMAIL_MODIFY], 600, "personal-fixture-access"),
        ),
    ]);
    let project = HashMap::from([(
        "work".to_string(),
        scoped_entry(&[GMAIL_MODIFY], 1800, PROJECT_ACCESS),
    )]);
    two_tier(label, user, project)
}

#[test]
fn project_entry_lacking_scope_no_longer_shadows_fresh_user_entry() {
    let (storage, _, _) = scope_shadowed("scope-shadow");

    let loaded = storage.load().unwrap();
    assert_eq!(
        loaded["work"].token.access_token, USER_ACCESS,
        "the wider, newer user-level entry must win over a narrower project entry (#8539)"
    );
    assert!(
        loaded["work"]
            .token
            .scopes
            .iter()
            .any(|s| s == GMAIL_SETTINGS)
    );
}

#[test]
#[tracing_test::traced_test]
fn load_warns_once_naming_winner_without_token_values() {
    let (storage, _, _) = scope_shadowed("scope-warn");

    storage.load().unwrap();
    storage.load().unwrap();

    logs_assert(|lines: &[&str]| {
        let warnings: Vec<&&str> = lines
            .iter()
            .filter(|l| l.contains("WARN") && l.contains("work"))
            .collect();
        if warnings.len() != 1 {
            return Err(format!(
                "expected exactly 1 shadow warning after two loads, found {}",
                warnings.len()
            ));
        }
        let w = warnings[0];
        if !(w.contains("user-level") && w.contains("gmail.settings.basic")) {
            return Err(format!("warning must name the winner and why: {w}"));
        }
        if lines.iter().any(|l| l.contains("fixture-access")) {
            return Err("a token value reached the log".into());
        }
        Ok(())
    });
}

#[test]
fn refresh_write_back_targets_the_winning_store() {
    // Mirrors `OAuthManager::refresh`: read the winner, replace its token,
    // write it back through `update`.
    let (storage, user_path, project_path) = scope_shadowed("scope-refresh");
    let mut stored = storage.get_profile("work").unwrap().unwrap();
    stored.token.access_token = "refreshed-fixture-access".into();
    stored.metadata.last_refreshed = Some(chrono::Utc::now());

    storage
        .update(|all| {
            all.insert("work".to_string(), stored);
            Ok(())
        })
        .unwrap();

    let user = TokenStorage::with_path(user_path).load().unwrap();
    let project = TokenStorage::with_path(project_path).load().unwrap();
    assert_eq!(
        user["work"].token.access_token, "refreshed-fixture-access",
        "a refreshed user-level winner must be written to the user store"
    );
    assert_eq!(
        project["work"].token.access_token, PROJECT_ACCESS,
        "the project store must not receive the user-level winner's token"
    );
    assert!(
        !project.contains_key("personal"),
        "an unchanged user-level entry must not be copied into the project store"
    );
}

#[test]
fn remove_profile_clears_both_stores() {
    let (storage, _, _) = scope_shadowed("scope-remove");

    storage.remove_profile("work").unwrap();

    assert!(
        !storage.load().unwrap().contains_key("work"),
        "removing a profile must not let its losing entry resurface"
    );
}

#[test]
fn remove_profile_keeps_a_different_account_user_entry() {
    let mut other = scoped_entry(&[GMAIL_MODIFY], 600, USER_ACCESS);
    other.metadata.email = Some("other@example.com".into());
    let (storage, user_path, _) = two_tier(
        "remove-other",
        HashMap::from([("work".to_string(), other)]),
        HashMap::from([(
            "work".to_string(),
            scoped_entry(&[GMAIL_MODIFY], 1800, PROJECT_ACCESS),
        )]),
    );

    let outcome = storage.remove_profile("work").unwrap();

    let user = TokenStorage::with_path(user_path).load().unwrap();
    assert_eq!(
        user["work"].metadata.email.as_deref(),
        Some("other@example.com"),
        "removing the project override must not delete another account's credential"
    );
    assert!(outcome.user_entry_remains);
}

#[test]
fn update_does_not_deadlock_when_project_and_user_are_the_same_file() {
    // cwd = $HOME makes both paths one file; two flocks on it self-deadlock.
    let dir = std::env::temp_dir().join(format!("gw-storage-same-{}", uuid::Uuid::new_v4()));
    let path = dir.join("tokens.json");
    TokenStorage::with_path(path.clone())
        .save(&HashMap::from([("work".to_string(), make_stored(3600))]))
        .unwrap();
    let mut storage = TokenStorage::with_path(path.clone());
    storage.project_path = Some(path);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(storage.set_default_profile("work").is_ok());
    });
    let ok = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("update deadlocked when project and user paths name one file");
    assert!(ok, "set_default_profile failed");
}

#[test]
fn update_refuses_to_overwrite_an_unparsable_store() {
    let (storage, user_path, project_path) = scope_shadowed("corrupt");
    std::fs::write(&project_path, "{not json").unwrap();
    let user_before = std::fs::read(&user_path).unwrap();

    let result = storage.update(|all| {
        all.insert("new".to_string(), make_stored(3600));
        Ok(())
    });

    assert!(result.is_err(), "update must fail on an unreadable store");
    assert_eq!(std::fs::read(&project_path).unwrap(), b"{not json");
    assert_eq!(std::fs::read(&user_path).unwrap(), user_before);
}

#[test]
#[tracing_test::traced_test]
fn load_warns_on_unparsable_store_without_echoing_it() {
    let (storage, _, project_path) = scope_shadowed("corrupt-load");
    // serde's own message would quote this value back.
    std::fs::write(
        &project_path,
        r#"{"work": {"version": "echo-fixture-value"}}"#,
    )
    .unwrap();

    let loaded = storage.load().unwrap();

    assert_eq!(loaded["work"].token.access_token, USER_ACCESS);
    assert!(logs_contain("unreadable"));
    assert!(logs_contain("line 1"));
    assert!(!logs_contain("echo-fixture-value"));
}

/// Best-effort regression test for issue #3502: two threads racing a
/// load-mutate-save cycle through [`TokenStorage::update`] on clones of
/// the same storage (the in-process half of the guard; the file lock
/// additionally protects separate processes, which a unit test can't
/// easily spin up) must not lose either writer's profile.
#[test]
fn concurrent_updates_do_not_lose_writes() {
    let dir = std::env::temp_dir().join(format!("gw-storage-concurrent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = TokenStorage::with_path(dir.join("tokens.json"));
    storage.save(&HashMap::new()).expect("seed empty map");

    let writers: Vec<_> = (0..8)
        .map(|i| {
            let s = storage.clone();
            std::thread::spawn(move || {
                let name = format!("profile-{i}");
                s.update(|all| {
                    all.insert(name.clone(), make_stored(3600));
                    Ok(())
                })
                .expect("update");
            })
        })
        .collect();
    for w in writers {
        w.join().expect("writer thread panicked");
    }

    let all = storage.load().expect("load");
    assert_eq!(
        all.len(),
        8,
        "all 8 concurrent writers' profiles must be present, got {} — a write was lost",
        all.len()
    );
    for i in 0..8 {
        assert!(
            all.contains_key(&format!("profile-{i}")),
            "profile-{i} missing after concurrent updates"
        );
    }
}

#[test]
fn remove_default_reassigns_to_next_profile() {
    let mut all = HashMap::new();
    all.insert("zeta".to_string(), make_stored(3600));
    all.insert("alpha".to_string(), make_stored(3600));
    all.get_mut("zeta").unwrap().metadata.is_default = true;

    let outcome = remove_and_reassign_default(&mut all, "zeta").expect("remove");
    assert_eq!(outcome.removed, "zeta");
    assert_eq!(outcome.reassigned_default.as_deref(), Some("alpha"));
    assert!(
        all["alpha"].metadata.is_default,
        "alpha must become default"
    );
    assert!(!all.contains_key("zeta"));
}

#[test]
fn remove_default_leaves_none_when_last_profile() {
    let mut all = HashMap::new();
    all.insert("only".to_string(), make_stored(3600));
    all.get_mut("only").unwrap().metadata.is_default = true;

    let outcome = remove_and_reassign_default(&mut all, "only").expect("remove");
    assert_eq!(outcome.reassigned_default, None);
    assert!(all.is_empty());
}

#[test]
fn remove_non_default_does_not_reassign() {
    let mut all = HashMap::new();
    all.insert("keep".to_string(), make_stored(3600));
    all.get_mut("keep").unwrap().metadata.is_default = true;
    all.insert("drop".to_string(), make_stored(3600));

    let outcome = remove_and_reassign_default(&mut all, "drop").expect("remove");
    assert_eq!(outcome.reassigned_default, None);
    assert!(
        all["keep"].metadata.is_default,
        "unrelated default untouched"
    );
}

#[test]
fn remove_missing_profile_errors() {
    let mut all: HashMap<String, StoredToken> = HashMap::new();
    assert!(remove_and_reassign_default(&mut all, "missing").is_err());
}
