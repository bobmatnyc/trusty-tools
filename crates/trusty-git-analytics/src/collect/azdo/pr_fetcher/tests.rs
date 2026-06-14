//! Tests for the ADO PR fetcher (unit + wiremock integration).
use super::*;
use crate::core::db::Database;
use rusqlite::params;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn multi_project_config(server_url: &str, projects: Vec<&str>) -> AzureDevOpsConfig {
    AzureDevOpsConfig {
        organization_url: server_url.to_string(),
        pat: "secret-pat".into(),
        project: None,
        projects: projects.iter().map(|s| s.to_string()).collect(),
        ticket_regex: r"AB#(\d+)".into(),
        team_keys: vec![],
        fetch_on_reference: true,
        fetch_prs: true,
    }
}

fn pr_body_json(pr_id: i64) -> serde_json::Value {
    serde_json::json!({
        "pullRequestId": pr_id,
        "title": "feat: multi-project",
        "status": "completed",
        "createdBy": {
            "uniqueName": "alice@contoso.com",
            "displayName": "Alice"
        },
        "creationDate": "2024-01-15T10:30:00Z",
        "closedDate": "2024-01-16T14:00:00Z",
        "sourceRefName": "refs/heads/feature/x",
        "targetRefName": "refs/heads/main",
        "reviewers": []
    })
}

fn sample_pr() -> AdoPullRequest {
    AdoPullRequest {
        pr_number: 12345,
        title: "feat: add widget".into(),
        description: Some("body".into()),
        author: "alice@contoso.com".into(),
        created_at: "2024-01-15T10:30:00Z".parse().unwrap(),
        closed_at: Some("2024-01-16T14:00:00Z".parse().unwrap()),
        source_branch: "refs/heads/feature/widget".into(),
        target_branch: "refs/heads/main".into(),
        status: "completed".into(),
        reviewers: vec![AdoPrReviewer {
            reviewer_id: "bob@contoso.com".into(),
            display_name: "Bob".into(),
            vote: 10,
            is_required: true,
            is_container: false,
        }],
        merge_commit_sha: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
    }
}

#[test]
fn ado_pr_fetcher_new_rejects_empty_projects() {
    let cfg = AzureDevOpsConfig {
        organization_url: "http://localhost".to_string(),
        pat: "secret-pat".into(),
        project: None,
        projects: vec![],
        ticket_regex: r"AB#(\d+)".into(),
        team_keys: vec![],
        fetch_on_reference: true,
        fetch_prs: true,
    };
    match AdoPrFetcher::new(cfg) {
        Ok(_) => panic!("empty projects must be rejected"),
        Err(AzdoError::Config(msg)) => assert!(
            msg.contains("project"),
            "expected message to mention project, got: {msg}"
        ),
        Err(other) => panic!("expected AzdoError::Config, got: {other:?}"),
    }
}

#[test]
fn extracts_unique_pr_ids() {
    let messages = vec![
        "Merged PR 100: do thing",
        "Some other commit",
        "merged pr 200: another (case-insensitive)",
        "Merged PR 100: duplicate",
        "Refactored: Merged PR 300: nested phrase",
    ];
    let ids = extract_pr_ids(messages);
    assert_eq!(ids, vec![100, 200, 300]);
}

#[test]
fn ignores_non_merge_lines() {
    let messages = vec!["fix: typo", "PR #42", "merge branch 'foo'"];
    let ids = extract_pr_ids(messages);
    assert!(ids.is_empty(), "no merge-PR pattern should match: {ids:?}");
}

#[test]
fn extract_pr_ids_handles_empty_input() {
    let ids: Vec<i64> = extract_pr_ids(Vec::<&str>::new());
    assert!(ids.is_empty());
}

#[test]
fn upsert_pr_round_trips_basic_fields() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let pr = sample_pr();
    let row_id = upsert_pr(conn, &pr, "MyProject").expect("first upsert");
    assert!(row_id > 0);

    let row_id2 = upsert_pr(conn, &pr, "MyProject").expect("second upsert");
    assert!(row_id2 > 0);

    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pull_requests \
             WHERE provider = 'azdo' AND repository = 'MyProject' AND pr_number = ?1",
            params![pr.pr_number],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(
        n, 1,
        "should have exactly one row per (provider, repository, pr_number)"
    );
}

#[test]
fn upsert_pr_reviewer_round_trips() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let pr = sample_pr();
    let pr_db_id = upsert_pr(conn, &pr, "MyProject").expect("pr upsert");
    for r in &pr.reviewers {
        upsert_pr_reviewer(conn, pr_db_id, r).expect("reviewer upsert");
    }
    for r in &pr.reviewers {
        upsert_pr_reviewer(conn, pr_db_id, r).expect("reviewer upsert (2)");
    }
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pr_reviewers WHERE pr_id = ?1",
            params![pr_db_id],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(n, pr.reviewers.len() as i64);

    let (vote, required): (i32, i32) = conn
        .query_row(
            "SELECT vote, is_required FROM pr_reviewers WHERE pr_id = ?1 AND reviewer_id = ?2",
            params![pr_db_id, "bob@contoso.com"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query reviewer");
    assert_eq!(vote, 10);
    assert_eq!(required, 1);
}

#[test]
fn get_existing_pr_numbers_returns_persisted_ids() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let pr = sample_pr();
    upsert_pr(conn, &pr, "MyProject").expect("upsert");

    let ids = get_existing_pr_numbers(conn, "azdo", "MyProject").expect("query");
    assert!(ids.contains(&pr.pr_number));

    let ids_gh = get_existing_pr_numbers(conn, "github", "MyProject").expect("query gh");
    assert!(
        !ids_gh.contains(&pr.pr_number),
        "provider scoping must hold"
    );

    let ids_other = get_existing_pr_numbers(conn, "azdo", "OtherProject").expect("query other");
    assert!(
        !ids_other.contains(&pr.pr_number),
        "repository scoping must hold for #88"
    );
}

#[test]
fn upsert_pr_allows_same_pr_number_in_different_repositories() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let pr = sample_pr();

    let id_a = upsert_pr(conn, &pr, "ProjectA").expect("upsert A");
    let id_b = upsert_pr(conn, &pr, "ProjectB").expect("upsert B");
    assert_ne!(id_a, id_b, "different repos must produce different rows");

    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pull_requests WHERE provider = 'azdo' AND pr_number = ?1",
            params![pr.pr_number],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(
        total, 2,
        "same pr_number across two repos must yield two rows"
    );
}

#[test]
fn upsert_pr_writes_commit_shas_when_merge_sha_present() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let pr = sample_pr();
    upsert_pr(conn, &pr, "MyProject").expect("upsert");

    let stored: String = conn
        .query_row(
            "SELECT commit_shas FROM pull_requests \
             WHERE provider = 'azdo' AND repository = 'MyProject' AND pr_number = ?1",
            params![pr.pr_number],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(
        stored, r#"["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]"#,
        "merge commit SHA must be persisted as a JSON array"
    );
}

#[test]
fn upsert_pr_writes_empty_commit_shas_when_no_merge_sha() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let mut pr = sample_pr();
    pr.merge_commit_sha = None;
    upsert_pr(conn, &pr, "MyProject").expect("upsert");

    let stored: String = conn
        .query_row(
            "SELECT commit_shas FROM pull_requests \
             WHERE provider = 'azdo' AND repository = 'MyProject' AND pr_number = ?1",
            params![pr.pr_number],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(stored, "[]");
}

#[test]
fn pr_raw_deserializes_full_payload() {
    let json = r#"{
        "pullRequestId": 12345,
        "title": "feat: add widget",
        "description": "body",
        "status": "completed",
        "createdBy": {
            "uniqueName": "alice@contoso.com",
            "displayName": "Alice"
        },
        "creationDate": "2024-01-15T10:30:00Z",
        "closedDate": "2024-01-16T14:00:00Z",
        "sourceRefName": "refs/heads/feature/widget",
        "targetRefName": "refs/heads/main",
        "reviewers": [
            {
                "uniqueName": "bob@contoso.com",
                "displayName": "Bob",
                "vote": 10,
                "isRequired": true,
                "isContainer": false
            }
        ],
        "lastMergeCommit": {
            "commitId": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "url": "https://dev.azure.com/.../commits/deadbeef..."
        }
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse");
    let pr: AdoPullRequest = raw.into();
    assert_eq!(pr.pr_number, 12345);
    assert_eq!(pr.title, "feat: add widget");
    assert_eq!(pr.author, "alice@contoso.com");
    assert_eq!(pr.status, "completed");
    assert_eq!(pr.target_branch, "refs/heads/main");
    assert_eq!(pr.reviewers.len(), 1);
    assert_eq!(pr.reviewers[0].vote, 10);
    assert!(pr.reviewers[0].is_required);
    assert_eq!(
        pr.merge_commit_sha.as_deref(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "lastMergeCommit.commitId should be threaded through"
    );
}

#[test]
fn pr_raw_treats_empty_last_merge_commit_as_none() {
    let json = r#"{
        "pullRequestId": 7,
        "creationDate": "2024-01-15T10:30:00Z",
        "status": "completed",
        "lastMergeCommit": {}
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse");
    let pr: AdoPullRequest = raw.into();
    assert!(pr.merge_commit_sha.is_none());

    let json = r#"{
        "pullRequestId": 8,
        "creationDate": "2024-01-15T10:30:00Z",
        "status": "completed",
        "lastMergeCommit": {"commitId": ""}
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse");
    let pr: AdoPullRequest = raw.into();
    assert!(pr.merge_commit_sha.is_none());
}

#[test]
fn pr_raw_drops_merge_sha_for_non_completed_status() {
    for status in ["active", "abandoned", "notSet", "", "ACTIVE"] {
        let json = format!(
            r#"{{
                "pullRequestId": 42,
                "creationDate": "2024-01-15T10:30:00Z",
                "status": "{status}",
                "mergeStrategy": "noFastForward",
                "lastMergeCommit": {{"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}
            }}"#
        );
        let raw: PrRaw = serde_json::from_str(&json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert!(
            pr.merge_commit_sha.is_none(),
            "non-completed status {status:?} must not expose a merge SHA"
        );
    }

    for status in ["completed", "Completed", "COMPLETED"] {
        let json = format!(
            r#"{{
                "pullRequestId": 43,
                "creationDate": "2024-01-15T10:30:00Z",
                "status": "{status}",
                "mergeStrategy": "noFastForward",
                "lastMergeCommit": {{"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}
            }}"#
        );
        let raw: PrRaw = serde_json::from_str(&json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert_eq!(
            pr.merge_commit_sha.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "completed status {status:?} should pass the gate (case-insensitive)",
        );
    }
}

#[test]
fn pr_raw_emits_merge_sha_for_no_fast_forward_strategy() {
    for strategy in ["noFastForward", "NOFASTFORWARD", "nofastforward"] {
        let json = format!(
            r#"{{
                "pullRequestId": 100,
                "creationDate": "2024-01-15T10:30:00Z",
                "status": "completed",
                "mergeStrategy": "{strategy}",
                "lastMergeCommit": {{"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}
            }}"#
        );
        let raw: PrRaw = serde_json::from_str(&json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert_eq!(
            pr.merge_commit_sha.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "noFastForward variant {strategy:?} must pass the gate",
        );
    }
}

#[test]
fn pr_raw_emits_merge_sha_when_strategy_absent() {
    let json = r#"{
        "pullRequestId": 101,
        "creationDate": "2024-01-15T10:30:00Z",
        "status": "completed",
        "lastMergeCommit": {"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse");
    let pr: AdoPullRequest = raw.into();
    assert_eq!(
        pr.merge_commit_sha.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        "absent mergeStrategy must default to allowed (pre-#96 behavior)",
    );
}

#[test]
fn pr_raw_drops_merge_sha_for_squash_strategy() {
    for strategy in ["squash", "SQUASH", "Squash"] {
        let json = format!(
            r#"{{
                "pullRequestId": 102,
                "creationDate": "2024-01-15T10:30:00Z",
                "status": "completed",
                "mergeStrategy": "{strategy}",
                "lastMergeCommit": {{"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}
            }}"#
        );
        let raw: PrRaw = serde_json::from_str(&json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert!(
            pr.merge_commit_sha.is_none(),
            "squash variant {strategy:?} must drop the merge SHA",
        );
    }
}

#[test]
fn pr_raw_drops_merge_sha_for_rebase_strategy() {
    let json = r#"{
        "pullRequestId": 103,
        "creationDate": "2024-01-15T10:30:00Z",
        "status": "completed",
        "mergeStrategy": "rebase",
        "lastMergeCommit": {"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse");
    let pr: AdoPullRequest = raw.into();
    assert!(pr.merge_commit_sha.is_none());
}

#[test]
fn pr_raw_drops_merge_sha_for_rebase_merge_strategy() {
    let json = r#"{
        "pullRequestId": 104,
        "creationDate": "2024-01-15T10:30:00Z",
        "status": "completed",
        "mergeStrategy": "rebaseMerge",
        "lastMergeCommit": {"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse");
    let pr: AdoPullRequest = raw.into();
    assert!(pr.merge_commit_sha.is_none());
}

#[test]
fn pr_raw_reads_merge_strategy_from_completion_options_fallback() {
    let json = r#"{
        "pullRequestId": 105,
        "creationDate": "2024-01-15T10:30:00Z",
        "status": "completed",
        "completionOptions": {"mergeStrategy": "squash"},
        "lastMergeCommit": {"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse");
    let pr: AdoPullRequest = raw.into();
    assert!(
        pr.merge_commit_sha.is_none(),
        "squash via completionOptions fallback must drop the merge SHA",
    );
}

#[test]
fn pr_raw_prefers_top_level_merge_strategy_over_completion_options() {
    let json = r#"{
        "pullRequestId": 106,
        "creationDate": "2024-01-15T10:30:00Z",
        "status": "completed",
        "mergeStrategy": "noFastForward",
        "completionOptions": {"mergeStrategy": "squash"},
        "lastMergeCommit": {"commitId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse");
    let pr: AdoPullRequest = raw.into();
    assert_eq!(
        pr.merge_commit_sha.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        "top-level mergeStrategy must take precedence over completionOptions",
    );
}

#[test]
fn pr_raw_tolerates_missing_optional_fields() {
    let json = r#"{
        "pullRequestId": 7,
        "creationDate": "2024-01-15T10:30:00Z"
    }"#;
    let raw: PrRaw = serde_json::from_str(json).expect("parse minimal");
    let pr: AdoPullRequest = raw.into();
    assert_eq!(pr.pr_number, 7);
    assert!(pr.author.is_empty());
    assert!(pr.reviewers.is_empty());
    assert!(pr.closed_at.is_none());
    assert!(pr.description.is_none());
}

#[test]
fn fetch_prs_config_deserializes_with_fetch_prs_true() {
    let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "MyProject"
fetch_prs: true
"#;
    let parsed: AzureDevOpsConfig = serde_yaml::from_str(yaml).expect("should deserialize cleanly");
    assert!(parsed.fetch_prs);
}

#[test]
fn fetch_prs_defaults_to_false() {
    let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "MyProject"
"#;
    let parsed: AzureDevOpsConfig = serde_yaml::from_str(yaml).expect("should deserialize cleanly");
    assert!(!parsed.fetch_prs, "fetch_prs default must be false");
}

fn select_to_fetch(
    conn: &rusqlite::Connection,
    project: &str,
    ids: Vec<i64>,
    force_refresh: bool,
) -> Vec<i64> {
    if force_refresh {
        ids
    } else {
        let existing = get_existing_pr_numbers(conn, "azdo", project).expect("query existing");
        ids.into_iter()
            .filter(|id| !existing.contains(id))
            .collect()
    }
}

#[test]
fn force_refresh_false_skips_existing_pr_ids() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let pr = sample_pr();
    upsert_pr(conn, &pr, "MyProject").expect("upsert");

    let to_fetch = select_to_fetch(conn, "MyProject", vec![12345, 999], false);
    assert_eq!(
        to_fetch,
        vec![999],
        "cached PR 12345 must be skipped when force_refresh is false"
    );
}

#[test]
fn force_refresh_true_re_fetches_existing_pr_ids() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let pr = sample_pr();
    upsert_pr(conn, &pr, "MyProject").expect("upsert");

    let to_fetch = select_to_fetch(conn, "MyProject", vec![12345, 999], true);
    assert_eq!(
        to_fetch,
        vec![12345, 999],
        "force_refresh must NOT skip already-cached PR IDs"
    );
}

#[test]
fn status_maps_to_pr_state_string() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let mut pr = sample_pr();
    pr.status = "abandoned".into();
    let id = upsert_pr(conn, &pr, "MyProject").expect("upsert");
    let state: String = conn
        .query_row(
            "SELECT state FROM pull_requests WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(state, "closed");

    pr.status = "active".into();
    upsert_pr(conn, &pr, "MyProject").expect("upsert");
    let state: String = conn
        .query_row(
            "SELECT state FROM pull_requests \
             WHERE provider = 'azdo' AND repository = 'MyProject' AND pr_number = ?1",
            params![pr.pr_number],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(state, "open");
}

// ----- Issue #91: multi-project HTTP tests via wiremock -----

#[tokio::test]
async fn fetch_pr_single_project_hit() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ProjectA/_apis/git/pullrequests/100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(100)))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["ProjectA"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
    let result = fetcher.fetch_pr(100).await.expect("fetch ok");
    let (pr, project) = result.expect("PR present");
    assert_eq!(pr.pr_number, 100);
    assert_eq!(project, "ProjectA");
    drop(server);
}

#[tokio::test]
async fn fetch_pr_falls_through_404_to_next_project() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ProjectA/_apis/git/pullrequests/200"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ProjectB/_apis/git/pullrequests/200"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(200)))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
    let result = fetcher.fetch_pr(200).await.expect("fetch ok");
    let (pr, project) = result.expect("PR present");
    assert_eq!(pr.pr_number, 200);
    assert_eq!(
        project, "ProjectB",
        "must report the project where PR was found"
    );
    drop(server);
}

#[tokio::test]
async fn fetch_pr_all_projects_404_returns_none() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/A/_apis/git/pullrequests/300"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/B/_apis/git/pullrequests/300"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["A", "B"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
    let result = fetcher.fetch_pr(300).await.expect("fetch ok");
    assert!(result.is_none(), "all 404s must produce Ok(None)");
    drop(server);
}

#[tokio::test]
async fn fetch_pr_first_hit_wins_no_query_to_subsequent_projects() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ProjectA/_apis/git/pullrequests/400"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(400)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ProjectB/_apis/git/pullrequests/400"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(400)))
        .expect(0)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
    let (pr, project) = fetcher
        .fetch_pr(400)
        .await
        .expect("fetch ok")
        .expect("PR present");
    assert_eq!(pr.pr_number, 400);
    assert_eq!(project, "ProjectA");
    drop(server);
}

#[tokio::test]
async fn run_persists_pr_under_project_where_found() {
    let server = MockServer::start().await;
    let pr_id: i64 = 500;

    Mock::given(method("GET"))
        .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/ProjectB/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();

    let commit_messages = vec![format!("Merged PR {pr_id}: feat: multi-project test")];
    let stored = fetcher.run(conn, commit_messages).await.expect("run ok");
    assert_eq!(stored, 1, "exactly one PR should be persisted");

    let repo: String = conn
        .query_row(
            "SELECT repository FROM pull_requests \
             WHERE provider = 'azdo' AND pr_number = ?1",
            params![pr_id],
            |row| row.get(0),
        )
        .expect("query persisted repository");
    assert_eq!(
        repo, "ProjectB",
        "persisted repository must be the project where the PR was found"
    );
    drop(server);
}

#[tokio::test]
async fn run_multi_project_does_not_skip_cached_pr_under_other_project() {
    let server = MockServer::start().await;
    let pr_id: i64 = 123;

    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let mut existing = sample_pr();
    existing.pr_number = pr_id;
    existing.title = "stale: ProjectA copy".into();
    upsert_pr(conn, &existing, "ProjectA").expect("seed ProjectA row");

    Mock::given(method("GET"))
        .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/ProjectB/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");

    let commit_messages = vec![format!("Merged PR {pr_id}: feat: collision case")];
    let stored = fetcher.run(conn, commit_messages).await.expect("run ok");
    assert_eq!(stored, 1, "ProjectB hit must produce one persisted PR");

    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pull_requests \
             WHERE provider = 'azdo' AND pr_number = ?1",
            params![pr_id],
            |row| row.get(0),
        )
        .expect("count rows");
    assert_eq!(
        total, 2,
        "expected two rows for pr_number=123 (one per project)"
    );
    let repos: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT repository FROM pull_requests \
                 WHERE provider = 'azdo' AND pr_number = ?1 ORDER BY repository",
            )
            .expect("prepare");
        let rows = stmt
            .query_map(params![pr_id], |row| row.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        rows
    };
    assert_eq!(repos, vec!["ProjectA".to_string(), "ProjectB".to_string()]);
    drop(server);
}

#[tokio::test]
async fn run_single_project_still_uses_cache_short_circuit() {
    let server = MockServer::start().await;
    let pr_id: i64 = 123;

    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    let mut existing = sample_pr();
    existing.pr_number = pr_id;
    upsert_pr(conn, &existing, "ProjectA").expect("seed ProjectA row");

    Mock::given(method("GET"))
        .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
        .expect(0)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["ProjectA"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");

    let commit_messages = vec![format!("Merged PR {pr_id}: cached case")];
    let stored = fetcher.run(conn, commit_messages).await.expect("run ok");
    assert_eq!(stored, 0, "cached PR must not be re-fetched");
    drop(server);
}

#[tokio::test]
async fn fetch_pr_aborts_iteration_on_401_does_not_query_next_project() {
    let server = MockServer::start().await;
    let pr_id: i64 = 901;

    Mock::given(method("GET"))
        .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/ProjectB/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
        .expect(0)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
    let err = fetcher
        .fetch_pr(pr_id)
        .await
        .expect_err("401 must surface as Err");
    assert!(
        matches!(err, AzdoError::Unauthorized),
        "expected AzdoError::Unauthorized, got: {err:?}"
    );
    drop(server);
}

#[tokio::test]
async fn fetch_pr_aborts_iteration_on_500_does_not_query_next_project() {
    let server = MockServer::start().await;
    let pr_id: i64 = 902;

    Mock::given(method("GET"))
        .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/ProjectB/_apis/git/pullrequests/{pr_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
        .expect(0)
        .mount(&server)
        .await;

    let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
    let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
    let err = fetcher
        .fetch_pr(pr_id)
        .await
        .expect_err("500 must surface as Err");
    assert!(
        matches!(err, AzdoError::Http { status: 500, .. }),
        "expected AzdoError::Http {{ status: 500, .. }}, got: {err:?}"
    );
    drop(server);
}
