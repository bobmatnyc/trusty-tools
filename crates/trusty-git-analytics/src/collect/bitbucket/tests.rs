//! Unit tests for [`super::BitbucketClient`].
//!
//! Why: split from `client.rs` to keep that file under the 500-line cap
//! while keeping the tests adjacent to the code they exercise.
//! What: covers JSON deserialisation, PR state mapping, pagination, auth
//! resolution via [`super::resolve_auth`] with an injected env lookup
//! (including env-expansion of `app_password` — issue #842 — and the
//! env-fallback path — issue #1653), and auth rejection when credentials
//! are absent. Auth tests inject an in-memory env map rather than mutating
//! `std::env`, so they cannot race under the parallel test harness.
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

/// The per-PR commits endpoint follows its own `next` cursor across two
/// pages, returning the union of both pages' SHAs in order.
///
/// Why: this is the same cursor-pagination contract as the PR list endpoint
/// (issue #841's fix reuses it for `fetch_pr_commits`); a regression here
/// would silently truncate large PRs to their first page of commits.
#[tokio::test]
async fn fetch_pr_commits_follows_next_cursor() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let base = server.uri();

    let page2_url = format!("{base}/repositories/acme/widgets/pullrequests/9/commits?page=2");
    let page1 = serde_json::json!({
        "values": [{"hash": "1111111111111111111111111111111111111a"}],
        "next": page2_url,
    });
    let page2 = serde_json::json!({
        "values": [{"hash": "2222222222222222222222222222222222222b"}],
    });

    Mock::given(method("GET"))
        .and(path("/repositories/acme/widgets/pullrequests/9/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page1.clone()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repositories/acme/widgets/pullrequests/9/commits"))
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

    let shas = client.fetch_pr_commits(9).await.expect("fetch");
    assert_eq!(
        shas,
        vec![
            "1111111111111111111111111111111111111a".to_string(),
            "2222222222222222222222222222222222222b".to_string(),
        ]
    );
}

/// `fetch_pull_requests` persists the *full* per-PR commit list (from the
/// dedicated commits endpoint), not the abbreviated `merge_commit` hash —
/// the regression this issue (#841) was filed against.
///
/// Why: the PR list payload alone yields only a short, often-unjoinable
/// merge SHA; this proves the enrichment call overwrites it with the real,
/// fully-qualified, potentially multi-commit list.
#[tokio::test]
async fn fetch_pull_requests_persists_full_commit_list() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let base = server.uri();

    let prs_page = serde_json::json!({
        "values": [{
            "id": 5,
            "title": "multi-commit PR",
            "state": "MERGED",
            "created_on": "2024-01-01T00:00:00Z",
            "updated_on": "2024-01-02T00:00:00Z",
            "merge_commit": {"hash": "36c721d47ff0"}
        }]
    });
    let commits_page = serde_json::json!({
        "values": [
            {"hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            {"hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
        ]
    });

    Mock::given(method("GET"))
        .and(path("/repositories/acme/widgets/pullrequests"))
        .respond_with(ResponseTemplate::new(200).set_body_json(prs_page))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repositories/acme/widgets/pullrequests/5/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(commits_page))
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
    assert_eq!(prs.len(), 1);
    let shas: Vec<String> =
        serde_json::from_str(&prs[0].commit_shas).expect("commit_shas is a JSON array");
    assert_eq!(
        shas,
        vec![
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        ]
    );
    // The abbreviated merge SHA must NOT be what gets persisted.
    assert!(!prs[0].commit_shas.contains("36c721d47ff0"));
}

/// When the per-PR commits endpoint errors (e.g. transient 5xx exhausting
/// retries), `fetch_pull_requests` degrades gracefully: the PR is still
/// returned, keeping the merge-commit fallback rather than failing the
/// whole batch or dropping the PR.
///
/// Why: one PR's enrichment call hiccuping must not take down collection
/// for every other PR in the repository (same partial-success philosophy
/// as the GitHub per-repo fetch, issue #87).
#[tokio::test]
async fn fetch_pull_requests_falls_back_on_commit_fetch_error() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let base = server.uri();

    let prs_page = serde_json::json!({
        "values": [{
            "id": 6,
            "title": "commits endpoint unreachable",
            "state": "MERGED",
            "created_on": "2024-01-01T00:00:00Z",
            "updated_on": "2024-01-02T00:00:00Z",
            "merge_commit": {"hash": "deadbeefcafe"}
        }]
    });

    Mock::given(method("GET"))
        .and(path("/repositories/acme/widgets/pullrequests"))
        .respond_with(ResponseTemplate::new(200).set_body_json(prs_page))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repositories/acme/widgets/pullrequests/6/commits"))
        .respond_with(ResponseTemplate::new(404))
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
    assert_eq!(prs.len(), 1);
    assert!(prs[0].commit_shas.contains("deadbeefcafe"));
}

/// Build a `BitbucketConfig` with workspace/repo set and the given auth
/// fields, so each auth test only states what it actually cares about.
fn auth_config(
    token: Option<&str>,
    username: Option<&str>,
    app_password: Option<&str>,
) -> BitbucketConfig {
    BitbucketConfig {
        token: token.map(Into::into),
        username: username.map(Into::into),
        app_password: app_password.map(Into::into),
        workspace: Some("acme".into()),
        repo_slug: Some("widgets".into()),
        fetch_prs: true,
        ..Default::default()
    }
}

/// Build an injected env lookup from a fixed `(name, value)` table.
///
/// Why: `resolve_auth` takes the environment as a closure precisely so tests
/// never mutate process-global `std::env` — that ambient mutation is what
/// raced across parallel tests and flaked the env-fallback case (issue #1653).
/// What: returns a closure that resolves names against an owned map; any name
/// not in the table reads as unset, regardless of the real process env.
fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name: &str| {
        owned
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

/// `resolve_auth` rejects a config with neither token nor username+password,
/// even when the (empty) environment offers nothing either.
///
/// Why: missing credentials must surface as a `Config` error, never a panic
/// or a silent default.
/// What: empty config auth fields + empty env → `CollectError::Config`.
/// Test: this test (pure, no env mutation).
#[test]
fn resolve_auth_rejects_missing_auth() {
    let cfg = auth_config(None, None, None);
    match resolve_auth(&cfg, env_map(&[])) {
        Ok(_) => panic!("expected auth failure, got Ok(_)"),
        Err(CollectError::Config(_)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

/// A `username` set without any app_password (config or env) is rejected.
///
/// Why: half-specified Basic auth must error rather than send an empty
/// password.
/// What: username present, app_password absent, empty env → `Config` error.
/// Test: this test.
#[test]
fn resolve_auth_rejects_username_without_password() {
    let cfg = auth_config(None, Some("carol"), None);
    match resolve_auth(&cfg, env_map(&[])) {
        Err(CollectError::Config(_)) => {}
        other => panic!("expected Config error, got {other:?}"),
    }
}

/// A plain config `token` resolves to Bearer auth and wins over Basic fields.
///
/// Why: documents the precedence — token always supersedes username/password.
/// What: config token + username/app_password → `BbAuth::Bearer(token)`.
/// Test: this test.
#[test]
fn resolve_auth_config_token_yields_bearer() {
    let cfg = auth_config(Some("tok-123"), Some("u"), Some("p"));
    match resolve_auth(&cfg, env_map(&[])).expect("resolves") {
        BbAuth::Bearer(t) => assert_eq!(t, "tok-123"),
        BbAuth::Basic { .. } => panic!("expected Bearer, got Basic"),
    }
}

/// A `${VAR}` token placeholder is expanded from the injected env, and the
/// `BITBUCKET_TOKEN` env var is used as a fallback when config token is unset.
///
/// Why: keeps token env-expansion (#741) and the env fallback covered after
/// the refactor.
/// What: placeholder token → expanded value; absent token → env var value.
/// Test: this test.
#[test]
fn resolve_auth_token_expands_and_falls_back() {
    let cfg = auth_config(Some("${TGA_BB_TOKEN}"), None, None);
    match resolve_auth(&cfg, env_map(&[("TGA_BB_TOKEN", "expanded-tok")])).expect("resolves") {
        BbAuth::Bearer(t) => assert_eq!(t, "expanded-tok"),
        BbAuth::Basic { .. } => panic!("expected Bearer"),
    }

    let cfg = auth_config(None, None, None);
    match resolve_auth(&cfg, env_map(&[("BITBUCKET_TOKEN", "env-tok")])).expect("resolves") {
        BbAuth::Bearer(t) => assert_eq!(t, "env-tok"),
        BbAuth::Basic { .. } => panic!("expected Bearer from env fallback"),
    }
}

/// `app_password` config values written as `${VAR}` are expanded from the
/// environment before being used for Basic auth (closes #842).
///
/// Why: PR #743 fixed env-expansion for `token` but missed `app_password`,
/// so users who wrote `app_password = "${MY_SECRET}"` in their YAML config
/// got a literal `${MY_SECRET}` sent as the HTTP Basic password → silent 401.
/// What: injects an env value, configures `app_password` as a placeholder,
/// resolves auth, and asserts the resolved password equals the env value.
/// Test: this test (pure; injected env, no `std::env` mutation).
#[test]
fn resolve_auth_app_password_env_var_expanded() {
    let cfg = auth_config(None, Some("myuser"), Some("${TGA_BB_APP_PW}"));
    match resolve_auth(&cfg, env_map(&[("TGA_BB_APP_PW", "s3cr3t-expanded")])).expect("resolves") {
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
/// What: injects a different `BITBUCKET_APP_PASSWORD`, configures a plain
/// `app_password`, and asserts the config value wins.
/// Test: this test.
#[test]
fn resolve_auth_app_password_config_takes_precedence_over_env_fallback() {
    let cfg = auth_config(None, Some("bob"), Some("config-literal-pw"));
    match resolve_auth(
        &cfg,
        env_map(&[("BITBUCKET_APP_PASSWORD", "env-fallback-value")]),
    )
    .expect("resolves")
    {
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
/// Why: validates the fallback branch is still reachable after the
/// env-expansion fix — a regression here would silently break users who rely
/// on the env var. This is the case that flaked in CI (issue #1653); injecting
/// the env removes the parallel-test race entirely.
/// What: config `app_password` absent, env fallback present → env value used.
/// Test: this test (pure; injected env, no `std::env` mutation).
#[test]
fn resolve_auth_app_password_falls_back_to_env_when_config_absent() {
    let cfg = auth_config(None, Some("carol"), None);
    match resolve_auth(&cfg, env_map(&[("BITBUCKET_APP_PASSWORD", "env-only-pw")]))
        .expect("resolves")
    {
        BbAuth::Basic { password, .. } => {
            assert_eq!(password, "env-only-pw");
        }
        BbAuth::Bearer(_) => panic!("expected Basic auth, got Bearer"),
    }
}
