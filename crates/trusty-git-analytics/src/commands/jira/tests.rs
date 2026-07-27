//! Tests for `tga jira sync` / `tga jira freshness` (issue #3966).

use super::*;
use tga::core::config::JiraConfig;

fn base_config(jira_url: Option<String>) -> Config {
    Config {
        jira: Some(JiraConfig {
            url: jira_url,
            username: Some("bot@example.com".to_string()),
            token: Some("test-token".to_string()),
            project_key: Some("PROJ".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// ---- resolve_project_key ------------------------------------------------

#[test]
fn resolve_project_key_prefers_cli_override() {
    let config = base_config(Some("https://x.atlassian.net".into()));
    let key = resolve_project_key(&config, Some("OTHER")).expect("resolves");
    assert_eq!(key, "OTHER");
}

#[test]
fn resolve_project_key_falls_back_to_config() {
    let config = base_config(Some("https://x.atlassian.net".into()));
    let key = resolve_project_key(&config, None).expect("resolves");
    assert_eq!(key, "PROJ");
}

#[test]
fn resolve_project_key_errors_when_neither_is_set() {
    let mut config = base_config(Some("https://x.atlassian.net".into()));
    config.jira.as_mut().unwrap().project_key = None;
    let err = resolve_project_key(&config, None);
    assert!(err.is_err(), "must error without any project scope");
}

// ---- parse_cli_date ------------------------------------------------------

#[test]
fn parse_cli_date_accepts_iso_date() {
    let d = parse_cli_date("2026-01-15").expect("parses");
    assert_eq!(d.to_rfc3339(), "2026-01-15T00:00:00+00:00");
}

#[test]
fn parse_cli_date_rejects_garbage() {
    assert!(parse_cli_date("not-a-date").is_err());
}

// ---- run_freshness --------------------------------------------------------

#[test]
fn run_freshness_fails_loudly_on_empty_tables() {
    let db = Database::open_in_memory().expect("open");
    let args = JiraFreshnessArgs {
        max_age_days: 2,
        report_only: false,
    };
    let err = run_freshness(&db, args);
    assert!(err.is_err(), "empty tables must fail the freshness check");
}

#[test]
fn run_freshness_report_only_never_fails() {
    let db = Database::open_in_memory().expect("open");
    let args = JiraFreshnessArgs {
        max_age_days: 2,
        report_only: true,
    };
    let result = run_freshness(&db, args);
    assert!(result.is_ok(), "--report-only must never return an error");
}

#[test]
fn run_freshness_passes_after_a_fresh_write() {
    let db = Database::open_in_memory().expect("open");
    tga::core::db::upsert_ticket_transition(
        db.connection(),
        &TicketTransitionRow {
            ticket_key: "PROJ-1".into(),
            project_key: "PROJ".into(),
            from_status: None,
            to_status: "Open".into(),
            transitioned_at: chrono::Utc::now().to_rfc3339(),
            author: None,
        },
    )
    .expect("upsert transition");
    tga::core::db::upsert_comment_detail(
        db.connection(),
        &CommentDetailRow {
            ticket_key: "PROJ-1".into(),
            comment_id: "1".into(),
            project_key: "PROJ".into(),
            author: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            body_len: 5,
        },
    )
    .expect("upsert comment");

    let args = JiraFreshnessArgs {
        max_age_days: 2,
        report_only: false,
    };
    assert!(run_freshness(&db, args).is_ok());
}

// ---- run_sync end-to-end (wiremock) ---------------------------------------

mod sync_e2e {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn search_response_body() -> serde_json::Value {
        serde_json::json!({
            "startAt": 0,
            "total": 1,
            "issues": [
                {
                    "key": "PROJ-1",
                    "fields": {
                        "project": {"key": "PROJ"},
                        "updated": "2026-01-05T10:00:00.000+0000"
                    },
                    "changelog": {
                        "histories": [
                            {
                                "author": {"displayName": "Jane Doe"},
                                "created": "2026-01-05T09:00:00.000+0000",
                                "items": [
                                    {"field": "status", "fromString": "To Do", "toString": "Done"}
                                ]
                            }
                        ]
                    }
                }
            ]
        })
    }

    fn comments_response_body() -> serde_json::Value {
        serde_json::json!({
            "startAt": 0,
            "maxResults": 100,
            "total": 1,
            "comments": [
                {
                    "id": "9001",
                    "author": {"displayName": "Jane Doe"},
                    "created": "2026-01-05T09:30:00.000+0000",
                    "body": "looks good to me"
                }
            ]
        })
    }

    /// End-to-end: `run_sync` against a mocked JIRA server writes exactly
    /// one transition row and one comment row, and advances the cursor to
    /// the observed `updated` timestamp.
    #[tokio::test]
    async fn run_sync_writes_transitions_comments_and_advances_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(search_response_body()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-1/comment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(comments_response_body()))
            .mount(&server)
            .await;

        let config = base_config(Some(server.uri()));
        let mut db = Database::open_in_memory().expect("open");

        let args = JiraSyncArgs {
            project: None,
            since: None,
            backfill: false,
            max_tickets: None,
            dry_run: false,
        };
        run_sync(config, &mut db, args).await.expect("sync ok");

        let transitions: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_ticket_transitions", [], |r| {
                r.get(0)
            })
            .expect("count transitions");
        assert_eq!(transitions, 1);

        let comments: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_jira_comment_detail", [], |r| {
                r.get(0)
            })
            .expect("count comments");
        assert_eq!(comments, 1);

        let cursor = get_cursor(db.connection(), "PROJ")
            .expect("query cursor")
            .expect("cursor recorded");
        assert_eq!(cursor.last_synced_at, "2026-01-05T10:00:00+00:00");
        assert_eq!(cursor.tickets_synced, 1);
    }

    /// `--dry-run` must fetch from JIRA (to report accurate counts) but
    /// write nothing to the database and leave the cursor untouched.
    #[tokio::test]
    async fn run_sync_dry_run_writes_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(search_response_body()))
            .mount(&server)
            .await;

        let config = base_config(Some(server.uri()));
        let mut db = Database::open_in_memory().expect("open");

        let args = JiraSyncArgs {
            project: None,
            since: None,
            backfill: false,
            max_tickets: None,
            dry_run: true,
        };
        run_sync(config, &mut db, args).await.expect("sync ok");

        let transitions: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_ticket_transitions", [], |r| {
                r.get(0)
            })
            .expect("count transitions");
        assert_eq!(transitions, 0, "dry-run must not write transitions");

        assert!(
            get_cursor(db.connection(), "PROJ")
                .expect("query cursor")
                .is_none(),
            "dry-run must not advance the cursor"
        );
    }

    /// A second sync run with no new tickets (empty issues list) must leave
    /// the previously-recorded cursor untouched rather than regressing it.
    #[tokio::test]
    async fn run_sync_with_no_matching_tickets_leaves_cursor_untouched() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "startAt": 0,
                "total": 0,
                "issues": []
            })))
            .mount(&server)
            .await;

        let config = base_config(Some(server.uri()));
        let mut db = Database::open_in_memory().expect("open");
        set_cursor(db.connection(), "PROJ", "2026-01-01T00:00:00+00:00", 5).expect("seed cursor");

        let args = JiraSyncArgs {
            project: None,
            since: None,
            backfill: false,
            max_tickets: None,
            dry_run: false,
        };
        run_sync(config, &mut db, args).await.expect("sync ok");

        let cursor = get_cursor(db.connection(), "PROJ")
            .expect("query cursor")
            .expect("still present");
        assert_eq!(
            cursor.last_synced_at, "2026-01-01T00:00:00+00:00",
            "an empty-result run must not regress the stored cursor"
        );
    }
}
