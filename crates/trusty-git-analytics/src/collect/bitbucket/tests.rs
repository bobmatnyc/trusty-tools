//! Unit tests for [`super::BitbucketClient`].
//!
//! Why: split from `client.rs` to keep that file under the 500-line cap
//! while keeping the tests adjacent to the code they exercise.
//! What: covers JSON deserialisation, PR state mapping, pagination, auth
//! construction (including env-expansion of `app_password` — issue #842),
//! and auth rejection when credentials are absent.
//! Test: `cargo test -p tga collect::bitbucket::tests`.

use super::*;

/// Verify the paged envelope tolerates a fully-populated PR with author,
/// merge commit, and a `next` cursor.
#[test]
fn bb_paged_full_pr_deserializes() {
    let json = r#"{
        "values": [{
            "id": 42,
            "title": "Add foo widget",
            "state": "MERGED",
            "created_on": "2024-01-02T03:04:05+00:00",
            "updated_on": "2024-01-03T00:00:00+00:00",
            "author": {
                "display_name": "Ada Lovelace",
                "nickname": "ada",
                "uuid": "{abc}"
            },
            "merge_commit": {"hash": "deadbeefcafe"}
        }],
        "next": "https://api.bitbucket.org/2.0/repositories/w/r/pullrequests?page=2"
    }"#;
    let page: BbPaged<BbPullRequest> = serde_json::from_str(json).expect("parses");
    assert_eq!(page.values.len(), 1);
    assert!(page.next.is_some());

    let mapped = map_pr(page.values.into_iter().next().unwrap(), "w/r");
    assert_eq!(mapped.pr_number, 42);
    assert_eq!(mapped.repository, "w/r");
    assert_eq!(mapped.state, PrState::Merged);
    assert_eq!(mapped.author, "ada");
    assert!(mapped.commit_shas.contains("deadbeefcafe"));
    assert!(mapped.merged_at.is_some());
}

/// A `DECLINED` PR with no merge commit should map to `Closed` and have
/// an empty `commit_shas` array, mirroring how GitHub stores unmerged PRs.
#[test]
fn bb_declined_pr_maps_to_closed_with_empty_shas() {
    let json = r#"{
        "id": 7,
        "title": "abandoned",
        "state": "DECLINED",
        "created_on": "2024-05-01T12:00:00Z",
        "author": {"display_name": "Bob"}
    }"#;
    let pr: BbPullRequest = serde_json::from_str(json).expect("parses");
    let mapped = map_pr(pr, "w/r");
    assert_eq!(mapped.pr_number, 7);
    assert_eq!(mapped.state, PrState::Closed);
    assert!(mapped.merged_at.is_none());
    assert_eq!(mapped.commit_shas, "[]");
    assert_eq!(mapped.author, "Bob");
}

/// `SUPERSEDED` should also collapse to `Closed`.
#[test]
fn bb_superseded_pr_maps_to_closed() {
    let json = r#"{
        "id": 8,
        "title": "old version",
        "state": "SUPERSEDED",
        "created_on": "2024-05-01T12:00:00Z"
    }"#;
    let pr: BbPullRequest = serde_json::from_str(json).expect("parses");
    let mapped = map_pr(pr, "w/r");
    assert_eq!(mapped.state, PrState::Closed);
    // No author block — `author` should be empty rather than panicking.
    assert_eq!(mapped.author, "");
}

/// Author fallback: missing `nickname` → `display_name`; missing both →
/// `uuid`; missing all → empty string.
#[test]
fn bb_author_best_name_priority() {
    use crate::collect::bitbucket::types::BbAuthor;
    let a = BbAuthor {
        display_name: Some("Ada Lovelace".into()),
        nickname: Some("ada".into()),
        uuid: Some("{abc}".into()),
    };
    assert_eq!(a.best_name(), "ada");

    let a = BbAuthor {
        display_name: Some("Ada Lovelace".into()),
        nickname: None,
        uuid: Some("{abc}".into()),
    };
    assert_eq!(a.best_name(), "Ada Lovelace");

    let a = BbAuthor {
        display_name: None,
        nickname: Some("  ".into()),
        uuid: Some("{abc}".into()),
    };
    assert_eq!(a.best_name(), "{abc}");

    let a = BbAuthor {
        display_name: None,
        nickname: None,
        uuid: None,
    };
    assert_eq!(a.best_name(), "");
}

/// Construct against a mock server and verify the client follows the
/// `next` cursor across two pages, returning the union.
///
/// Why: cursor pagination is the single biggest semantic difference
/// from the GitHub client; if we ever fail to follow `next`, half the
/// PR history disappears silently.
#[tokio::test]
async fn fetch_pull_requests_follows_next_cursor() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let base = server.uri();

    // Page 2 (terminal) — declare first because page 1 references its URL.
    let page2_url = format!("{base}/repositories/acme/widgets/pullrequests?page=2");
    let page1 = serde_json::json!({
        "values": [{
            "id": 1,
            "title": "first",
            "state": "OPEN",
            "created_on": "2024-01-01T00:00:00Z"
        }],
        "next": page2_url,
    });
    let page2 = serde_json::json!({
        "values": [{
            "id": 2,
            "title": "second",
            "state": "MERGED",
            "created_on": "2024-02-01T00:00:00Z",
            "updated_on": "2024-02-02T00:00:00Z",
            "merge_commit": {"hash": "abc123"}
        }]
    });

    Mock::given(method("GET"))
        .and(path("/repositories/acme/widgets/pullrequests"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page1.clone()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repositories/acme/widgets/pullrequests"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2.clone()))
        .mount(&server)
        .await;

    let client = BitbucketClient::new(&BitbucketConfig {
        token: Some("dummy".into()),
        workspace: Some("acme".into()),
        repo_slug: Some("widgets".into()),
        fetch_prs: true,
        api_base_url: Some(base),
        ..Default::default()
    })
    .expect("client builds");

    let prs = client.fetch_pull_requests().await.expect("fetch");
    assert_eq!(prs.len(), 2);
    assert_eq!(prs[0].pr_number, 1);
    assert_eq!(prs[0].state, PrState::Open);
    assert_eq!(prs[1].pr_number, 2);
    assert_eq!(prs[1].state, PrState::Merged);
    assert!(prs[1].commit_shas.contains("abc123"));
}

/// Drop guard that snapshots an env var, removes it for the test, and
/// restores it (or re-removes it if originally absent) on drop. The Drop
/// impl runs during stack unwinding, so a panic inside the test body
/// still triggers env restore — unlike a closure-based save/run/restore
/// pattern, which silently leaks mutated state when the closure panics.
pub(super) struct EnvVarGuard {
    pub(super) name: &'static str,
    pub(super) original: Option<String>,
}

impl EnvVarGuard {
    pub(super) fn remove(name: &'static str) -> Self {
        let original = std::env::var(name).ok();
        // SAFETY: 2024-edition env mutation; the guard is the sole writer
        // for `name` within the test it covers, restored unconditionally
        // on drop.
        unsafe { std::env::remove_var(name) };
        Self { name, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see [`EnvVarGuard::remove`].
        unsafe {
            match self.original.as_deref() {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

/// `new()` rejects a config that has neither token nor username+password.
#[test]
fn client_new_rejects_missing_auth() {
    // Take care not to pick up a real env credential during this
    // assertion; guards restore env even if the test below panics.
    let _t = EnvVarGuard::remove("BITBUCKET_TOKEN");
    let _p = EnvVarGuard::remove("BITBUCKET_APP_PASSWORD");

    let result = BitbucketClient::new(&BitbucketConfig {
        workspace: Some("acme".into()),
        repo_slug: Some("widgets".into()),
        fetch_prs: true,
        ..Default::default()
    });
    match result {
        Ok(_) => panic!("expected auth failure, got Ok(_)"),
        Err(CollectError::Config(_)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

/// `app_password` config values written as `${VAR}` are expanded from the
/// environment before being used for Basic auth (closes #842).
///
/// Why: PR #743 fixed env-expansion for `token` but missed `app_password`,
/// so users who wrote `app_password = "${MY_SECRET}"` in their YAML config
/// got a literal `${MY_SECRET}` sent as the HTTP Basic password → silent 401.
/// What: sets a known env var, configures `app_password` as a placeholder,
/// builds the client, and asserts the resolved password equals the env value.
/// Test: this test (unit, no network). Precedence variant (config wins over
/// env fallback) is also covered: explicit password string is used as-is.
#[test]
fn app_password_env_var_expanded() {
    // Isolate env state so parallel tests cannot interfere.
    let _t = EnvVarGuard::remove("BITBUCKET_TOKEN");
    let _fallback = EnvVarGuard::remove("BITBUCKET_APP_PASSWORD");

    // SAFETY: same pattern used by EnvVarGuard; sole writer in this test.
    unsafe { std::env::set_var("TGA_TEST_BB_APP_PW_842", "s3cr3t-expanded") };
    let _guard = EnvVarGuard {
        name: "TGA_TEST_BB_APP_PW_842",
        original: None,
    };

    let client = BitbucketClient::new(&BitbucketConfig {
        username: Some("myuser".into()),
        app_password: Some("${TGA_TEST_BB_APP_PW_842}".into()),
        workspace: Some("acme".into()),
        repo_slug: Some("widgets".into()),
        fetch_prs: true,
        ..Default::default()
    })
    .expect("client builds with expanded app_password");

    // Inspect the resolved password via the auth field on the client.
    match &client.auth {
        BbAuth::Basic { username, password } => {
            assert_eq!(username, "myuser");
            assert_eq!(
                password, "s3cr3t-expanded",
                "app_password placeholder must be expanded from env; \
                 got literal placeholder instead"
            );
        }
        BbAuth::Bearer(_) => panic!("expected Basic auth, got Bearer"),
    }
}

/// When `app_password` in config is a plain value (no `${…}` placeholder),
/// it is used as-is; the `BITBUCKET_APP_PASSWORD` env fallback is NOT used.
///
/// Why: verifies the precedence chain — config value wins over env fallback —
/// so adding env-expansion does not accidentally break callers who store the
/// literal password in YAML.
/// What: sets `BITBUCKET_APP_PASSWORD` to a different string, configures a
/// plain `app_password`, and asserts the config value wins.
/// Test: this test.
#[test]
fn app_password_config_takes_precedence_over_env_fallback() {
    let _t = EnvVarGuard::remove("BITBUCKET_TOKEN");

    // SAFETY: sole writer; EnvVarGuard restores on drop.
    unsafe { std::env::set_var("BITBUCKET_APP_PASSWORD", "env-fallback-value") };
    let _fallback = EnvVarGuard {
        name: "BITBUCKET_APP_PASSWORD",
        original: None,
    };

    let client = BitbucketClient::new(&BitbucketConfig {
        username: Some("bob".into()),
        app_password: Some("config-literal-pw".into()),
        workspace: Some("acme".into()),
        repo_slug: Some("widgets".into()),
        fetch_prs: true,
        ..Default::default()
    })
    .expect("client builds");

    match &client.auth {
        BbAuth::Basic { password, .. } => {
            assert_eq!(
                password, "config-literal-pw",
                "config app_password must win over BITBUCKET_APP_PASSWORD env fallback"
            );
        }
        BbAuth::Bearer(_) => panic!("expected Basic auth, got Bearer"),
    }
}

/// When `app_password` is absent from config, the `BITBUCKET_APP_PASSWORD`
/// env var is used as the fallback credential.
///
/// Why: validates the fallback branch is still reachable after the env-expansion
/// fix — a regression here would silently break users who rely on the env var.
/// What: removes config `app_password`, sets env fallback, asserts env value used.
/// Test: this test.
#[test]
fn app_password_falls_back_to_env_when_config_absent() {
    let _t = EnvVarGuard::remove("BITBUCKET_TOKEN");

    // SAFETY: sole writer; EnvVarGuard restores on drop.
    unsafe { std::env::set_var("BITBUCKET_APP_PASSWORD", "env-only-pw") };
    let _fallback = EnvVarGuard {
        name: "BITBUCKET_APP_PASSWORD",
        original: None,
    };

    let client = BitbucketClient::new(&BitbucketConfig {
        username: Some("carol".into()),
        app_password: None, // rely entirely on env
        workspace: Some("acme".into()),
        repo_slug: Some("widgets".into()),
        fetch_prs: true,
        ..Default::default()
    })
    .expect("client builds from env fallback");

    match &client.auth {
        BbAuth::Basic { password, .. } => {
            assert_eq!(password, "env-only-pw");
        }
        BbAuth::Bearer(_) => panic!("expected Basic auth, got Bearer"),
    }
}
