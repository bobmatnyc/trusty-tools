//! Tests for the JIRA REST client, changelog/comment parsing, and payload types.
//!
//! Loaded via `#[cfg(test)] #[path = "client_tests.rs"] mod tests;` in `client.rs`
//! (kept out-of-line so `client.rs` stays under the 500-SLOC production cap;
//! see `scripts/check_line_cap.sh`).

use super::*;

/// Confirm a JQL search response shape parses end-to-end.
///
/// Why: `search_issues` terminates on `total`; if JIRA renames it our loop
/// terminates incorrectly. `startAt` is deliberately not modelled (the
/// client tracks its own offset), so an unmodelled `startAt` in the payload
/// must simply be ignored rather than breaking the parse.
/// What: parse a representative search payload with one issue.
/// Test: assert `total` and inner issue fields all populate.
#[test]
fn jira_search_response_deserializes() {
    let json = r#"{
        "startAt": 0,
        "total": 1,
        "issues": [
            {
                "key": "PROJ-1",
                "fields": {
                    "summary": "Fix bug",
                    "status": {"name": "Done"},
                    "issuetype": {"name": "Bug"},
                    "customfield_10016": 5.0
                }
            }
        ]
    }"#;
    let resp: SearchResponse = serde_json::from_str(json).expect("parses");
    assert_eq!(resp.total, 1);
    assert_eq!(resp.issues.len(), 1);
    let issue = JiraClient::convert_issue(
        resp.issues.into_iter().next().expect("one"),
        Some("customfield_10016"),
    );
    assert_eq!(issue.key, "PROJ-1");
    assert_eq!(issue.summary, "Fix bug");
    assert_eq!(issue.status, "Done");
    assert_eq!(issue.issue_type, "Bug");
    assert_eq!(issue.story_points, Some(5.0));
}

/// Confirm field descriptor wire shape deserializes.
///
/// Why: cache discovery hinges on this exact shape.
/// What: parse a representative `/rest/api/3/field` element.
/// Test: assert both fields extract.
#[test]
fn field_descriptor_deserializes() {
    let json = r#"[
        {"id": "customfield_10016", "name": "Story Points"},
        {"id": "summary", "name": "Summary"}
    ]"#;
    let fields: Vec<FieldDescriptor> = serde_json::from_str(json).expect("parses");
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].id, "customfield_10016");
    assert_eq!(fields[0].name, "Story Points");
}

/// Story points should be `None` when the custom field is absent.
///
/// Why: not every JIRA instance has a configured story-point field;
/// missing fields must degrade gracefully.
/// What: convert an issue payload that omits the custom field.
/// Test: assert `story_points` is `None`.
#[test]
fn convert_issue_returns_none_when_field_missing() {
    let json = r#"{
        "key": "PROJ-2",
        "fields": {
            "summary": "x",
            "status": {"name": "Open"},
            "issuetype": {"name": "Task"}
        }
    }"#;
    let api: ApiIssue = serde_json::from_str(json).expect("parses");
    let issue = JiraClient::convert_issue(api, Some("customfield_10016"));
    assert!(issue.story_points.is_none());
}

// ---- Changelog / comment tests (issue #3966) ----------------------

/// A changelog search response with one status transition parses into a
/// single `JiraTransition` with the expected `from`/`to`/author/created.
#[test]
fn changelog_search_response_parses_status_transition() {
    let json = r#"{
        "startAt": 0,
        "total": 1,
        "issues": [
            {
                "key": "PROJ-1",
                "fields": {"project": {"key": "PROJ"}},
                "changelog": {
                    "histories": [
                        {
                            "author": {"displayName": "Jane Doe"},
                            "created": "2026-01-01T10:00:00.000+0000",
                            "items": [
                                {"field": "status", "fromString": "To Do", "toString": "In Progress"}
                            ]
                        }
                    ]
                }
            }
        ]
    }"#;
    let resp: ChangelogSearchResponse = serde_json::from_str(json).expect("parses");
    assert_eq!(resp.issues.len(), 1);
    let issue = ChangelogIssue::from_api(resp.issues.into_iter().next().expect("one"));
    assert_eq!(issue.key, "PROJ-1");
    assert_eq!(issue.project_key, "PROJ");
    assert_eq!(issue.transitions.len(), 1);
    let t = &issue.transitions[0];
    assert_eq!(t.from_status.as_deref(), Some("To Do"));
    assert_eq!(t.to_status, "In Progress");
    assert_eq!(t.author.as_deref(), Some("Jane Doe"));
}

/// Non-`status` changelog items (e.g. `assignee`) must be dropped —
/// only status transitions belong in `fact_ticket_transitions`.
#[test]
fn changelog_ignores_non_status_fields() {
    let json = r#"{
        "key": "PROJ-2",
        "fields": {"project": {"key": "PROJ"}},
        "changelog": {
            "histories": [
                {
                    "created": "2026-01-01T10:00:00.000+0000",
                    "items": [
                        {"field": "assignee", "fromString": "Alice", "toString": "Bob"},
                        {"field": "status", "fromString": "Open", "toString": "Closed"}
                    ]
                }
            ]
        }
    }"#;
    let api: ChangelogApiIssue = serde_json::from_str(json).expect("parses");
    let issue = ChangelogIssue::from_api(api);
    assert_eq!(
        issue.transitions.len(),
        1,
        "assignee changes must be filtered out"
    );
    assert_eq!(issue.transitions[0].to_status, "Closed");
}

/// The very first status-touching history entry has no `fromString`
/// (the ticket's creation state) — this must surface as `from_status:
/// None`, not be dropped or error.
#[test]
fn changelog_initial_transition_has_no_from_status() {
    let json = r#"{
        "key": "PROJ-3",
        "fields": {"project": {"key": "PROJ"}},
        "changelog": {
            "histories": [
                {
                    "created": "2026-01-01T10:00:00.000+0000",
                    "items": [
                        {"field": "status", "toString": "Open"}
                    ]
                }
            ]
        }
    }"#;
    let api: ChangelogApiIssue = serde_json::from_str(json).expect("parses");
    let issue = ChangelogIssue::from_api(api);
    assert_eq!(issue.transitions.len(), 1);
    assert!(issue.transitions[0].from_status.is_none());
    assert_eq!(issue.transitions[0].to_status, "Open");
}

/// When the `project` field is absent, the project key falls back to
/// the issue key's prefix rather than panicking.
#[test]
fn changelog_falls_back_to_key_prefix_when_project_missing() {
    let json = r#"{
        "key": "INFRA-42",
        "fields": {},
        "changelog": {"histories": []}
    }"#;
    let api: ChangelogApiIssue = serde_json::from_str(json).expect("parses");
    let issue = ChangelogIssue::from_api(api);
    assert_eq!(issue.project_key, "INFRA");
}

/// An unparseable changelog `created` timestamp must skip that history
/// entry (no transitions extracted from it) rather than panicking or
/// erroring the whole batch.
#[test]
fn changelog_skips_unparseable_timestamp() {
    let json = r#"{
        "key": "PROJ-4",
        "fields": {"project": {"key": "PROJ"}},
        "changelog": {
            "histories": [
                {"created": "not-a-date", "items": [{"field": "status", "toString": "Done"}]}
            ]
        }
    }"#;
    let api: ChangelogApiIssue = serde_json::from_str(json).expect("parses");
    let issue = ChangelogIssue::from_api(api);
    assert!(issue.transitions.is_empty());
}

/// A plain-string comment body's length is measured directly in bytes.
#[test]
fn comment_body_len_for_plain_string() {
    let json = r#"{"id": "1001", "author": {"displayName": "Jane Doe"}, "created": "2026-01-01T10:00:00.000+0000", "body": "hello world"}"#;
    let api: ApiComment = serde_json::from_str(json).expect("parses");
    let comment = JiraComment::from_api(api).expect("valid timestamp parses");
    assert_eq!(comment.id, "1001");
    assert_eq!(comment.author.as_deref(), Some("Jane Doe"));
    assert_eq!(comment.body_len, "hello world".len() as i64);
}

/// An Atlassian Document Format (object) comment body is measured as
/// the byte length of its JSON-serialized form (documented
/// approximation — see `JiraClient::fetch_comments` doc comment).
#[test]
fn comment_body_len_for_adf_object() {
    let json = r#"{
        "id": "1002",
        "created": "2026-01-01T10:00:00.000+0000",
        "body": {"type": "doc", "version": 1, "content": []}
    }"#;
    let api: ApiComment = serde_json::from_str(json).expect("parses");
    let comment = JiraComment::from_api(api).expect("valid timestamp parses");
    assert!(comment.author.is_none());
    let expected_len = serde_json::to_string(&json!({"type": "doc", "version": 1, "content": []}))
        .unwrap()
        .len() as i64;
    assert_eq!(comment.body_len, expected_len);
}

/// A comment with an unparseable `created` timestamp must be skipped
/// (returns `None`) rather than fabricating a fallback timestamp that
/// would corrupt downstream ordering/freshness queries.
#[test]
fn comment_with_unparseable_timestamp_is_skipped() {
    let json = r#"{"id": "1003", "created": "not-a-date", "body": "x"}"#;
    let api: ApiComment = serde_json::from_str(json).expect("parses");
    assert!(JiraComment::from_api(api).is_none());
}

/// `GET /issue/{key}/comment` response shape parses end-to-end,
/// including `total` for pagination termination.
#[test]
fn comment_search_response_deserializes() {
    let json = r#"{
        "startAt": 0,
        "maxResults": 100,
        "total": 2,
        "comments": [
            {"id": "1", "created": "2026-01-01T00:00:00.000+0000", "body": "a"},
            {"id": "2", "created": "2026-01-02T00:00:00.000+0000", "body": "b"}
        ]
    }"#;
    let resp: CommentSearchResponse = serde_json::from_str(json).expect("parses");
    assert_eq!(resp.total, 2);
    assert_eq!(resp.comments.len(), 2);
}

/// `parse_jira_datetime` accepts both strict RFC3339 and JIRA's
/// colonless-offset flavour.
#[test]
fn parse_jira_datetime_accepts_both_shapes() {
    assert!(parse_jira_datetime("2026-01-01T00:00:00Z").is_some());
    assert!(parse_jira_datetime("2026-01-01T00:00:00.000+0000").is_some());
    assert!(parse_jira_datetime("garbage").is_none());
}

/// A JIRA Server/DC timestamp carries a colonless offset with **no**
/// fractional seconds. Every fixture in this file uses the millisecond
/// Cloud shape, so nothing else pins this: if the `%.3f` fallback did not
/// accept an empty fractional part, `ChangelogIssue::from_api` would drop
/// the whole history entry behind nothing louder than a `warn!`.
#[test]
fn parse_jira_datetime_accepts_second_precision_colonless_offset() {
    assert!(
        parse_jira_datetime("2026-01-01T10:00:00+0000").is_some(),
        "the JIRA Server/DC shape must not be silently dropped"
    );
}

// ---- Credential expansion (PR #4067 review round 1) --------------------

/// A `${VAR}` credential whose variable is unset must fail at construction
/// with a message naming the config field, not silently become an empty
/// password that surfaces as an opaque HTTP 401 hours into a cron run.
#[test]
fn unset_credential_env_var_is_a_config_error() {
    let config = JiraConfig {
        url: Some("https://example.atlassian.net".into()),
        username: Some("bot@example.com".into()),
        token: Some("${TGA_TEST_JIRA_TOKEN_DEFINITELY_UNSET_4067}".into()),
        ..Default::default()
    };
    let Err(err) = JiraClient::new(&config) else {
        panic!("must reject an unresolved credential");
    };
    let msg = err.to_string();
    assert!(msg.contains("jira.token"), "must name the field: {msg}");
    assert!(
        msg.contains("TGA_TEST_JIRA_TOKEN_DEFINITELY_UNSET_4067"),
        "must name the unresolved variable: {msg}"
    );
}

/// Omitting credentials entirely is still legal (unauthenticated access to a
/// public instance) — only a *present but unresolvable* one is an error.
#[test]
fn absent_credentials_are_not_an_error() {
    let config = JiraConfig {
        url: Some("https://example.atlassian.net".into()),
        ..Default::default()
    };
    assert!(JiraClient::new(&config).is_ok());
}

// ---- Paged HTTP behaviour (PR #4067 review round 1) --------------------

mod paged_http {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use crate::collect::jira::retry::RetryPolicy;

    fn fast_retry() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        }
    }

    fn client_for(server: &MockServer) -> JiraClient {
        let config = JiraConfig {
            url: Some(server.uri()),
            ..Default::default()
        };
        JiraClient::new(&config)
            .expect("client builds")
            .with_retry_policy(fast_retry())
    }

    fn issue(key: &str, updated: &str) -> serde_json::Value {
        json!({
            "key": key,
            "fields": {"project": {"key": "PROJ"}, "updated": updated},
            "changelog": {"histories": []}
        })
    }

    /// `2026-01-01T00:MM:00.000+0000` for a minute offset.
    fn at_minute(minute: usize) -> String {
        format!("2026-01-01T00:{minute:02}:00.000+0000")
    }

    /// Serves a changelog walk over 53 tickets whose `updated` values are one
    /// minute apart, and simulates a ticket being edited mid-walk.
    ///
    /// Page 1 (no `updated >=` bound) returns PROJ-1..PROJ-50. Between pages,
    /// PROJ-3 is "edited" and re-sorts to the very end of the result set —
    /// the exact mutation that made offset pagination skip a ticket.
    struct ShiftingChangelog {
        seen: Arc<Mutex<Vec<(String, u64)>>>,
    }

    impl Respond for ShiftingChangelog {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
            let jql = body["jql"].as_str().unwrap_or_default().to_string();
            let start_at = body["startAt"].as_u64().unwrap_or_default();
            self.seen
                .lock()
                .expect("lock")
                .push((jql.clone(), start_at));

            if !jql.contains("updated >=") {
                // First page: the unbounded window, 50 issues (a full page).
                let issues: Vec<serde_json::Value> = (1..=50)
                    .map(|i| issue(&format!("PROJ-{i}"), &at_minute(i)))
                    .collect();
                return ResponseTemplate::new(200).set_body_json(json!({"issues": issues}));
            }

            // Second page: the re-anchored window `updated >= 00:50`. Its
            // full contents are PROJ-50 (the boundary minute we already
            // hold), the unread tail, and PROJ-3 — which now sorts at the
            // very end because of its mid-walk edit. The client's offset
            // skips the boundary item; PROJ-3 must be deduplicated.
            let mut issues: Vec<serde_json::Value> = vec![
                issue("PROJ-50", &at_minute(50)),
                issue("PROJ-51", &at_minute(51)),
                issue("PROJ-52", &at_minute(52)),
                issue("PROJ-53", &at_minute(53)),
                issue("PROJ-3", "2026-01-01T09:00:00.000+0000"),
            ];
            issues.drain(..(start_at as usize).min(issues.len()));
            ResponseTemplate::new(200).set_body_json(json!({"issues": issues}))
        }
    }

    /// The HIGH finding from PR #4067 review: a ticket edited mid-walk must
    /// not push an unread ticket across the read boundary.
    ///
    /// Under the old `startAt`-offset walk, PROJ-3 leaving the front of the
    /// result set shifted everything down one index, so `startAt = 50` landed
    /// on PROJ-52 and PROJ-51 was never read — and, sitting below the run's
    /// max `updated`, was excluded from every later incremental run too.
    #[tokio::test]
    async fn changelog_walk_survives_a_ticket_edited_mid_walk() {
        let server = MockServer::start().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(ShiftingChangelog {
                seen: Arc::clone(&seen),
            })
            .mount(&server)
            .await;

        let scope = SyncScope {
            project_key: "PROJ".into(),
            since: None,
        };
        let issues = client_for(&server)
            .search_with_changelog(&scope, 60)
            .await
            .expect("walk succeeds");

        let keys: Vec<&str> = issues.iter().map(|i| i.key.as_str()).collect();
        assert!(
            keys.contains(&"PROJ-51"),
            "the ticket after the page boundary must still be read; got {} issues",
            keys.len()
        );
        assert_eq!(
            keys.len(),
            53,
            "every distinct ticket exactly once (re-read boundary deduplicated)"
        );
        assert_eq!(
            keys.iter().filter(|k| **k == "PROJ-3").count(),
            1,
            "the re-sorted ticket must not be emitted twice"
        );

        let requests = seen.lock().expect("lock").clone();
        assert_eq!(requests.len(), 2, "walk should take two pages");
        assert!(
            !requests[0].0.contains("updated >="),
            "first page is the unbounded window: {}",
            requests[0].0
        );
        assert!(
            requests[1].0.contains("updated >= \"2026-01-01 00:50\""),
            "the second page must re-anchor on the first page's max updated, \
             not page by absolute offset: {}",
            requests[1].0
        );
        assert_eq!(
            requests[1].1, 1,
            "only the single item already held from the 00:50 minute is skipped"
        );
    }

    /// A transient 500 on the per-ticket comment fetch must be retried, not
    /// turned into a ticket-level ingestion failure that holds the cursor.
    struct FlakyComments {
        calls: Arc<AtomicUsize>,
    }

    impl Respond for FlakyComments {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                return ResponseTemplate::new(500);
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "startAt": 0,
                "maxResults": 100,
                "total": 1,
                "comments": [
                    {"id": "9001", "created": "2026-01-05T09:30:00.000+0000", "body": "ok"}
                ]
            }))
        }
    }

    #[tokio::test]
    async fn fetch_comments_retries_a_transient_500() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-1/comment"))
            .respond_with(FlakyComments {
                calls: Arc::clone(&calls),
            })
            .mount(&server)
            .await;

        let comments = client_for(&server)
            .fetch_comments("PROJ-1")
            .await
            .expect("the retry must recover the transient failure");
        assert_eq!(comments.len(), 1);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "one failed attempt plus one successful retry"
        );
    }

    /// A permanent 404 must NOT be retried — retrying it wastes the attempt
    /// budget and hides the real (permanent) problem.
    #[tokio::test]
    async fn fetch_comments_does_not_retry_a_404() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        struct AlwaysMissing {
            calls: Arc<AtomicUsize>,
        }
        impl Respond for AlwaysMissing {
            fn respond(&self, _request: &Request) -> ResponseTemplate {
                self.calls.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(404)
            }
        }
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-9/comment"))
            .respond_with(AlwaysMissing {
                calls: Arc::clone(&calls),
            })
            .mount(&server)
            .await;

        let err = client_for(&server).fetch_comments("PROJ-9").await;
        assert!(err.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a 404 is permanent for the life of the run"
        );
    }
}
