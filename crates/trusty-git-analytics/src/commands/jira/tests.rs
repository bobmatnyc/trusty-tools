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

/// A key that would widen or break the JQL must be rejected locally, with a
/// message naming it — not interpolated into the query and bounced back as
/// an opaque JIRA 400 (or, worse, silently matched against another project
/// whose tickets would then advance *this* project's cursor).
#[test]
fn resolve_project_key_rejects_jql_unsafe_keys() {
    let config = base_config(Some("https://x.atlassian.net".into()));
    let err = resolve_project_key(&config, Some("PROJ OR project = OTHER"))
        .expect_err("must reject a key carrying JQL syntax");
    assert!(
        err.to_string().contains("PROJ OR project = OTHER"),
        "the error must name the offending value: {err}"
    );
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
        project: None,
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
        project: None,
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
        project: None,
    };
    assert!(run_freshness(&db, args).is_ok());
}

/// The HIGH finding from PR #4067 review, at the command level: with two
/// projects on a schedule, the default (no `--project`) check must fail
/// because it checks each cursor-bearing project individually — the
/// table-wide aggregate would report OK on the strength of the healthy
/// project alone.
#[test]
fn run_freshness_default_fails_when_any_single_project_is_stale() {
    let db = Database::open_in_memory().expect("open");

    for (project, ticket) in [("A", "A-1"), ("B", "B-1")] {
        tga::core::db::upsert_ticket_transition(
            db.connection(),
            &TicketTransitionRow {
                ticket_key: ticket.into(),
                project_key: project.into(),
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
                ticket_key: ticket.into(),
                comment_id: "1".into(),
                project_key: project.into(),
                author: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                body_len: 5,
            },
        )
        .expect("upsert comment");
        set_cursor(db.connection(), project, "2026-01-01T00:00:00+00:00", 1).expect("cursor");
    }

    // Project B's sync stopped running 10 days ago; A's is still healthy.
    let ten_days_ago = chrono::Utc::now().timestamp() - 10 * 86_400;
    for table in ["fact_ticket_transitions", "fact_jira_comment_detail"] {
        db.connection()
            .execute(
                &format!("UPDATE {table} SET synced_at = ?1 WHERE project_key = 'B'"),
                rusqlite::params![ten_days_ago],
            )
            .expect("backdate B");
    }

    assert!(
        run_freshness(
            &db,
            JiraFreshnessArgs {
                max_age_days: 2,
                report_only: false,
                project: None,
            }
        )
        .is_err(),
        "a dead per-project sync must fail the default check, not hide behind \
         the other project's writes"
    );
    assert!(
        run_freshness(
            &db,
            JiraFreshnessArgs {
                max_age_days: 2,
                report_only: false,
                project: Some("A".into()),
            }
        )
        .is_ok(),
        "the healthy project must still pass when checked on its own"
    );
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

    /// `--dry-run` must fetch from JIRA — *including* comments, so the counts
    /// it reports are the counts a real run would produce — while writing
    /// nothing and leaving the cursor untouched.
    ///
    /// Before PR #4067's review round 1, dry-run `continue`d before the
    /// comment fetch, so it structurally printed `0 comment(s)` for every
    /// preview and this test could pass without a comment mock mounted at
    /// all. Asserting the comment endpoint was actually hit is what makes
    /// the report-honesty claim testable.
    #[tokio::test]
    async fn run_sync_dry_run_fetches_comments_but_writes_nothing() {
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
            dry_run: true,
        };
        run_sync(config, &mut db, args).await.expect("sync ok");

        let comment_requests = server
            .received_requests()
            .await
            .expect("recorded requests")
            .into_iter()
            .filter(|r| r.url.path().ends_with("/comment"))
            .count();
        assert_eq!(
            comment_requests, 1,
            "dry-run must fetch comments so its reported count is real"
        );

        let transitions: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_ticket_transitions", [], |r| {
                r.get(0)
            })
            .expect("count transitions");
        assert_eq!(transitions, 0, "dry-run must not write transitions");

        let comments: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_jira_comment_detail", [], |r| {
                r.get(0)
            })
            .expect("count comments");
        assert_eq!(comments, 0, "dry-run must not write comments");

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

// ---- Partial-failure handling (the CRITICAL from PR #4067 review) --------
//
// This is the arm that had no test at all, which is why green CI said
// nothing about a defect that permanently and silently discarded a ticket's
// comments.

mod partial_failure {
    use super::*;
    use std::sync::{Arc, Mutex};

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// `PROJ-1` sorts well below `PROJ-2` on `updated` — it is the ticket a
    /// batch-maximum cursor would step over.
    const EARLY_UPDATED: &str = "2026-01-03T10:00:00.000+0000";
    const LATE_UPDATED: &str = "2026-06-01T10:00:00.000+0000";

    fn issue(key: &str, updated: &str) -> serde_json::Value {
        serde_json::json!({
            "key": key,
            "fields": {"project": {"key": "PROJ"}, "updated": updated},
            "changelog": {
                "histories": [
                    {
                        "author": {"displayName": "Jane Doe"},
                        "created": "2026-01-01T09:00:00.000+0000",
                        "items": [{"field": "status", "fromString": "To Do", "toString": "Done"}]
                    }
                ]
            }
        })
    }

    /// A `/search` responder that actually honours the JQL `updated >=`
    /// bound, so a test can prove what the *next* run's window contains
    /// rather than assuming it.
    struct WindowedSearch {
        jqls: Arc<Mutex<Vec<String>>>,
    }

    impl Respond for WindowedSearch {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
            let jql = body["jql"].as_str().unwrap_or_default().to_string();
            self.jqls.lock().expect("lock").push(jql.clone());

            // Extract the `updated >= "..."` literal, if any.
            let bound = jql.split("updated >= \"").nth(1).and_then(|rest| {
                rest.split('"')
                    .next()
                    .map(|s| format!("{}:00.000+0000", s.replace(' ', "T")))
            });
            let visible = |updated: &str| match &bound {
                // Both sides are fixed-width UTC strings, so lexical order
                // is chronological order here.
                Some(b) => updated >= b.as_str(),
                None => true,
            };

            let issues: Vec<serde_json::Value> =
                [("PROJ-1", EARLY_UPDATED), ("PROJ-2", LATE_UPDATED)]
                    .into_iter()
                    .filter(|(_, u)| visible(u))
                    .map(|(k, u)| issue(k, u))
                    .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"issues": issues}))
        }
    }

    fn comment_body(id: &str) -> serde_json::Value {
        serde_json::json!({
            "startAt": 0,
            "maxResults": 100,
            "total": 1,
            "comments": [
                {"id": id, "author": {"displayName": "Jane Doe"},
                 "created": "2026-01-05T09:30:00.000+0000", "body": "looks good"}
            ]
        })
    }

    fn sync_args() -> JiraSyncArgs {
        JiraSyncArgs {
            project: None,
            since: None,
            backfill: false,
            max_tickets: None,
            dry_run: false,
        }
    }

    /// Mount a server whose `PROJ-1` comment endpoint fails permanently and
    /// whose `PROJ-2` comment endpoint succeeds.
    ///
    /// The failure is a 404 rather than a 500 deliberately: a 404 is
    /// classified permanent, so this test spends no time in real retry
    /// backoff. The retryable-5xx path has its own coverage in
    /// `collect/jira/client_tests.rs::fetch_comments_retries_a_transient_500`.
    async fn server_with_failing_first_ticket(jqls: Arc<Mutex<Vec<String>>>) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(WindowedSearch { jqls })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-1/comment"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-2/comment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(comment_body("9002")))
            .mount(&server)
            .await;
        server
    }

    /// THE CRITICAL: a ticket whose comment fetch failed must not fall out
    /// of the next run's window.
    ///
    /// Previously the cursor advanced to the batch maximum (`PROJ-2`'s
    /// `updated`), which sits *after* the failed `PROJ-1` in the
    /// `updated ASC` ordering — so `PROJ-1` never matched `updated >=`
    /// again and its comments were gone for good, with the process still
    /// exiting 0.
    #[tokio::test]
    async fn comment_fetch_failure_holds_the_cursor_at_the_failed_ticket() {
        let jqls = Arc::new(Mutex::new(Vec::new()));
        let server = server_with_failing_first_ticket(Arc::clone(&jqls)).await;
        let mut db = Database::open_in_memory().expect("open");

        let err = run_sync(base_config(Some(server.uri())), &mut db, sync_args())
            .await
            .expect_err("a partial ingestion must not be reported as success");
        assert!(
            err.to_string().contains("PROJ-1"),
            "the failure must name the ticket: {err}"
        );

        // Transitions for BOTH tickets are persisted — partial progress is
        // kept, it is simply not allowed to move the cursor past the hole.
        let transitions: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_ticket_transitions", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(transitions, 2);

        let comments: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_jira_comment_detail", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(comments, 1, "only PROJ-2's comment could be ingested");

        let cursor = get_cursor(db.connection(), "PROJ")
            .expect("query")
            .expect("recorded");
        assert_eq!(
            cursor.last_synced_at, "2026-01-03T10:00:00+00:00",
            "the cursor must clamp to the failed ticket's `updated`, not advance \
             to the batch maximum (2026-06-01) which would skip it forever"
        );
    }

    /// …and the clamped cursor must actually bring that ticket back: a
    /// second run, driven by the stored cursor against a server that honours
    /// the JQL window, re-fetches `PROJ-1` and completes it.
    #[tokio::test]
    async fn the_next_run_refetches_the_failed_ticket() {
        let jqls = Arc::new(Mutex::new(Vec::new()));
        let failing = server_with_failing_first_ticket(Arc::clone(&jqls)).await;
        let mut db = Database::open_in_memory().expect("open");
        run_sync(base_config(Some(failing.uri())), &mut db, sync_args())
            .await
            .expect_err("first run fails");
        drop(failing);

        // Second run: same dataset, but PROJ-1's comments are now reachable.
        let healthy = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(WindowedSearch {
                jqls: Arc::clone(&jqls),
            })
            .mount(&healthy)
            .await;
        for (key, id) in [("PROJ-1", "9001"), ("PROJ-2", "9002")] {
            Mock::given(method("GET"))
                .and(path(format!("/rest/api/3/issue/{key}/comment")))
                .respond_with(ResponseTemplate::new(200).set_body_json(comment_body(id)))
                .mount(&healthy)
                .await;
        }

        run_sync(base_config(Some(healthy.uri())), &mut db, sync_args())
            .await
            .expect("the retry run succeeds");

        let second_jql = jqls.lock().expect("lock")[1].clone();
        assert!(
            second_jql.contains("updated >= \"2026-01-03 10:00\""),
            "the second run's window must start at the failed ticket: {second_jql}"
        );

        let recovered: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_jira_comment_detail WHERE ticket_key = 'PROJ-1'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            recovered, 1,
            "the previously-failed ticket's comments must be recovered"
        );

        let cursor = get_cursor(db.connection(), "PROJ")
            .expect("query")
            .expect("recorded");
        assert_eq!(
            cursor.last_synced_at, "2026-06-01T10:00:00+00:00",
            "with nothing left failing, the cursor is free to advance to the \
             batch maximum"
        );
    }

    /// A failure in dry-run must also surface — a preview that quietly
    /// under-reports is the same trap one level down.
    #[tokio::test]
    async fn dry_run_also_reports_comment_failures() {
        let jqls = Arc::new(Mutex::new(Vec::new()));
        let server = server_with_failing_first_ticket(jqls).await;
        let mut db = Database::open_in_memory().expect("open");

        let args = JiraSyncArgs {
            dry_run: true,
            ..sync_args()
        };
        assert!(
            run_sync(base_config(Some(server.uri())), &mut db, args)
                .await
                .is_err(),
            "an incomplete preview must not exit 0 either"
        );
        assert!(
            get_cursor(db.connection(), "PROJ")
                .expect("query")
                .is_none(),
            "dry-run still writes no cursor"
        );
    }
}
