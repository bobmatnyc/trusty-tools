//! Unit tests for the `storage` module.
//!
//! Why: split out of `storage/mod.rs` to keep the production file under the
//! 500-SLOC cap (mirrors the `oauth/flow/mod.rs` + `flow/tests.rs` split).
//! What: exercises `TokenStorage` load/save/permissions, the stale-shadow
//! warning, `TokenStorage::update`'s concurrency guard, and the
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
/// already expired) for the stale-shadow-warning tests below.
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

#[test]
fn warns_when_project_stale_and_user_fresh() {
    let project = make_stored(-3600);
    let user = make_stored(3600);
    let info = stale_shadow_warning(
        &project,
        &user,
        Path::new("/proj/.gworkspace-mcp/tokens.json"),
        Path::new("/home/user/.gworkspace-mcp/tokens.json"),
    )
    .expect("must warn: project stale, user fresh");
    assert_eq!(
        info.project_path,
        Path::new("/proj/.gworkspace-mcp/tokens.json")
    );
    assert_eq!(
        info.user_path,
        Path::new("/home/user/.gworkspace-mcp/tokens.json")
    );
    assert_eq!(info.project_expires_at, project.token.expires_at);
    assert_eq!(info.user_expires_at, user.token.expires_at);
}

#[test]
fn no_warning_when_both_fresh() {
    let project = make_stored(3600);
    let user = make_stored(7200);
    assert!(stale_shadow_warning(&project, &user, Path::new("p"), Path::new("u")).is_none());
}

#[test]
fn no_warning_when_both_stale() {
    // Both stale is not the silent-shadow failure mode — the caller gets
    // an expired token either way, so no extra signal is needed here.
    let project = make_stored(-3600);
    let user = make_stored(-7200);
    assert!(stale_shadow_warning(&project, &user, Path::new("p"), Path::new("u")).is_none());
}

#[test]
fn no_warning_when_project_is_fresher() {
    let project = make_stored(3600);
    let user = make_stored(-3600);
    assert!(stale_shadow_warning(&project, &user, Path::new("p"), Path::new("u")).is_none());
}

/// Build a two-tier temp `TokenStorage` (separate user/project dirs) with
/// a `work` profile present on both sides, and write the given
/// user/project token maps to disk. Shared by the precedence and
/// warn-capture tests below.
fn temp_shadowed_storage(
    label: &str,
    user_expires_in_secs: i64,
    project_expires_in_secs: i64,
) -> TokenStorage {
    let dir = std::env::temp_dir().join(format!("gw-storage-{label}-{}", uuid::Uuid::new_v4()));
    let user_dir = dir.join("user");
    let project_dir = dir.join("project");
    std::fs::create_dir_all(&user_dir).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    let user_path = user_dir.join("tokens.json");
    let project_path = project_dir.join("tokens.json");

    let mut user_tokens = HashMap::new();
    user_tokens.insert("work".to_string(), make_stored(user_expires_in_secs));
    std::fs::write(&user_path, serde_json::to_string(&user_tokens).unwrap()).unwrap();

    let mut project_tokens = HashMap::new();
    let mut project_stored = make_stored(project_expires_in_secs);
    project_stored.token.access_token = "stale-access-token".into();
    project_tokens.insert("work".to_string(), project_stored);
    std::fs::write(
        &project_path,
        serde_json::to_string(&project_tokens).unwrap(),
    )
    .unwrap();

    TokenStorage {
        user_path,
        project_path: Some(project_path),
        warned_stale_shadows: Arc::new(Mutex::new(HashSet::new())),
        write_guard: Arc::new(Mutex::new(())),
    }
}

#[test]
fn load_still_prefers_project_override_after_warning() {
    // Regression guard for the precedence contract: this fix only adds a
    // warning, it does not change which entry wins (see PR body) — a
    // stale project override must still be served, just noisily.
    let storage = temp_shadowed_storage("precedence", 3600, -3600);

    let loaded = storage.load().unwrap();
    assert_eq!(
        loaded["work"].token.access_token, "stale-access-token",
        "project override must still win per the documented contract \
             (warn, don't change precedence)"
    );
}

#[test]
#[tracing_test::traced_test]
fn load_warns_when_project_shadow_is_stale() {
    let storage = temp_shadowed_storage("warns", 3600, -3600);

    storage.load().unwrap();

    assert!(logs_contain("work"));
    assert!(logs_contain("STALE"));
}

#[test]
#[tracing_test::traced_test]
fn load_does_not_rewarn_on_second_load() {
    // PR #2949 review (HIGH): load() sits on the per-request hot path —
    // an unthrottled warn would repeat on every single MCP tool call.
    // Two loads on the same TokenStorage must produce exactly one
    // stale-shadow warning, not two.
    let storage = temp_shadowed_storage("norewarn", 3600, -3600);

    storage.load().unwrap();
    storage.load().unwrap();

    logs_assert(|lines: &[&str]| {
        let count = lines
            .iter()
            .filter(|l| l.contains("STALE") && l.contains("work"))
            .count();
        if count == 1 {
            Ok(())
        } else {
            Err(format!(
                "expected exactly 1 stale-shadow warning after two load() calls, found {count}"
            ))
        }
    });
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
