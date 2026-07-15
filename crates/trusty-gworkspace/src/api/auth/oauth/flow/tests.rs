//! Unit tests for the `flow` consent orchestration module.
//!
//! Why: split out of `flow/mod.rs` to keep the production file under the
//! 500-SLOC cap (the pure helpers here carry a large, valuable test body).
//! What: exercises the offline-testable helpers — scope assembly, auth-URL
//! construction, client-creds parsing, default-mode decisions, and the
//! browser-vs-print-url consent prompt.
//! Test: this file IS the test module for `flow`.

use super::*;

#[test]
fn scope_string_matches_constant_set() {
    let s = assemble_scope_string();
    assert!(s.starts_with("openid "));
    assert!(s.contains("https://www.googleapis.com/auth/gmail.modify"));
    assert!(s.contains("https://www.googleapis.com/auth/presentations"));
    assert_eq!(s.split(' ').count(), OAUTH_SCOPES.len());
}

#[test]
fn build_auth_url_contains_all_params() {
    let url = build_auth_url(
        "cid.apps.googleusercontent.com",
        "http://127.0.0.1:5000",
        "openid https://www.googleapis.com/auth/calendar",
        "CHALLENGE",
        "STATE",
    );
    assert!(url.starts_with(OAUTH_AUTH_URL));
    assert!(url.contains("client_id=cid.apps.googleusercontent.com"));
    assert!(url.contains("code_challenge=CHALLENGE"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("state=STATE"));
    assert!(url.contains("access_type=offline"));
    assert!(url.contains("prompt=consent"));
    // redirect_uri and scope must be percent-encoded.
    assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"));
    assert!(url.contains("scope=openid%20https"));
}

#[test]
fn parses_flat_creds() {
    let c = parse_client_creds_json(r#"{"client_id":"a","client_secret":"b"}"#).unwrap();
    assert_eq!(c.client_id, "a");
    assert_eq!(c.client_secret, "b");
}

#[test]
fn parses_installed_creds() {
    let c = parse_client_creds_json(
            r#"{"installed":{"client_id":"x","client_secret":"y","redirect_uris":["http://localhost"]}}"#,
        )
        .unwrap();
    assert_eq!(c.client_id, "x");
    assert_eq!(c.client_secret, "y");
}

#[test]
fn default_profile_falls_back() {
    assert_eq!(effective_profile(None), DEFAULT_PROFILE);
    assert_eq!(effective_profile(Some("  ")), DEFAULT_PROFILE);
    assert_eq!(effective_profile(Some("work")), "work");
}

fn make_stored(name: &str, is_default: bool) -> StoredToken {
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

#[test]
fn persist_marks_single_default() {
    let dir = std::env::temp_dir().join(format!("gw-persist-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = TokenStorage::with_path(dir.join("tokens.json"));

    persist(&storage, "first", make_stored("first", false), true).unwrap();
    persist(&storage, "second", make_stored("second", false), true).unwrap();

    let all = storage.load().unwrap();
    assert_eq!(all.len(), 2);
    assert!(!all["first"].metadata.is_default, "old default cleared");
    assert!(all["second"].metadata.is_default, "new entry is default");
}

#[test]
fn persist_false_does_not_steal_existing_default() {
    // Regression test: a second `setup` run with set_default=false (the
    // outcome of DefaultMode::Auto when a default already exists on a
    // different profile) must leave the existing default untouched.
    let dir = std::env::temp_dir().join(format!("gw-persist2-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = TokenStorage::with_path(dir.join("tokens.json"));

    persist(&storage, "first", make_stored("first", false), true).unwrap();
    persist(&storage, "second", make_stored("second", false), false).unwrap();

    let all = storage.load().unwrap();
    assert_eq!(all.len(), 2);
    assert!(
        all["first"].metadata.is_default,
        "first must remain default — it was not stolen"
    );
    assert!(!all["second"].metadata.is_default);
}

#[test]
fn auto_sets_default_only_when_absent() {
    let existing: HashMap<String, StoredToken> = HashMap::new();
    assert!(should_set_default(DefaultMode::Auto, "first", &existing));
}

#[test]
fn auto_does_not_steal_existing_default() {
    let mut existing = HashMap::new();
    existing.insert("first".to_string(), make_stored("first", true));
    assert!(
        !should_set_default(DefaultMode::Auto, "second", &existing),
        "second account must not silently steal the default"
    );
}

#[test]
fn auto_keeps_default_on_reauth_of_default_profile() {
    let mut existing = HashMap::new();
    existing.insert("first".to_string(), make_stored("first", true));
    assert!(
        should_set_default(DefaultMode::Auto, "first", &existing),
        "re-authorizing the existing default profile should keep it default"
    );
}

#[test]
fn force_overrides_existing_default() {
    let mut existing = HashMap::new();
    existing.insert("first".to_string(), make_stored("first", true));
    assert!(should_set_default(DefaultMode::Force, "second", &existing));
}

#[test]
fn never_keeps_existing_default() {
    let existing: HashMap<String, StoredToken> = HashMap::new();
    assert!(!should_set_default(DefaultMode::Never, "first", &existing));
    let mut with_default = HashMap::new();
    with_default.insert("first".to_string(), make_stored("first", true));
    assert!(!should_set_default(
        DefaultMode::Never,
        "second",
        &with_default
    ));
}

#[test]
fn consent_prompt_url_is_identical_regardless_of_browser_mode() {
    // Item 3 contract: the --print-url (no-browser) path must show the
    // EXACT same consent URL as the auto-open path. The URL is built once
    // and only the browser-launch decision is gated by the flag, so the
    // rendered prompt embeds a byte-identical URL either way.
    let url = build_auth_url(
        "cid.apps.googleusercontent.com",
        "http://127.0.0.1:5000",
        "openid https://www.googleapis.com/auth/calendar",
        "CHALLENGE",
        "STATE",
    );
    let opened = consent_prompt_lines(&url, true, 300).join("\n");
    let printed = consent_prompt_lines(&url, false, 300).join("\n");

    assert!(
        opened.contains(&url),
        "auto-open prompt must embed the consent URL"
    );
    assert!(
        printed.contains(&url),
        "print-url prompt must embed the consent URL"
    );
    // The same URL appears in both — the flag never changes what is built.
    let extract = |s: &str| -> String {
        s.lines()
            .find(|l| l.contains(OAUTH_AUTH_URL))
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(
        extract(&opened),
        extract(&printed),
        "print-url URL must match the auto-open URL exactly"
    );
}

#[test]
fn consent_prompt_wording_differs_by_mode() {
    let url = "https://accounts.google.com/o/oauth2/v2/auth?x=1";
    let opened = consent_prompt_lines(url, true, 300).join("\n");
    let printed = consent_prompt_lines(url, false, 300).join("\n");
    assert!(
        opened.to_lowercase().contains("opening your browser"),
        "auto-open must announce the browser launch: {opened}"
    );
    assert!(
        !printed.to_lowercase().contains("opening your browser"),
        "print-url must NOT claim to open a browser: {printed}"
    );
    assert!(
        printed.to_lowercase().contains("open this url"),
        "print-url must instruct the user to open the URL: {printed}"
    );
}
