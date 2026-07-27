//! Tests for the JIRA REST client, changelog/comment parsing, and payload types.
//!
//! Loaded via `#[cfg(test)] #[path = "client_tests.rs"] mod tests;` in `client.rs`
//! (kept out-of-line so `client.rs` stays under the 500-SLOC production cap;
//! see `scripts/check_line_cap.sh`).

use super::*;
// The changelog/comment wire shapes and their parsing moved to
// `collect::jira::model` when `client.rs` reached that cap; the fixtures
// exercising them stayed here.
use crate::collect::jira::model::*;

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

/// `GET /issue/{key}/comment` response shape parses end-to-end.
///
/// The envelope's bookkeeping fields (`startAt`, `maxResults`, `total`) are
/// deliberately unmodelled — the walk terminates on a short page — so this
/// also pins that an envelope carrying them still parses rather than erroring
/// on unknown fields.
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
            max_total_delay: Duration::from_millis(100),
        }
    }

    fn client_for(server: &MockServer) -> JiraClient {
        let config = JiraConfig {
            url: Some(server.uri()),
            // Pinned so this fixture needs no /myself route; discovery has
            // its own tests below.
            timezone: Some("UTC".to_string()),
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
        let walk = client_for(&server)
            .search_with_changelog(&scope, 60)
            .await
            .expect("walk succeeds");

        let keys: Vec<&str> = walk.issues.iter().map(|i| i.key.as_str()).collect();
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

    // ---- account timezone discovery (PR #4067 review round 2) ----------

    fn myself_body(tz: &str) -> serde_json::Value {
        json!({"accountId": "abc", "displayName": "Bot", "timeZone": tz})
    }

    /// An explicitly configured `jira.timezone` wins and issues no request —
    /// the escape hatch for unauthenticated instances and for hosts that
    /// cannot reach `/myself`.
    #[tokio::test]
    async fn account_timezone_prefers_configured_value() {
        let server = MockServer::start().await;
        let config = JiraConfig {
            url: Some(server.uri()),
            timezone: Some("Asia/Kolkata".to_string()),
            ..Default::default()
        };
        let client = JiraClient::new(&config).expect("builds");
        assert_eq!(
            client.account_timezone().await.expect("resolves"),
            chrono_tz::Tz::Asia__Kolkata
        );
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "a pinned timezone must not cost a round-trip"
        );
    }

    /// With nothing configured, the zone comes from `/myself` — and is cached,
    /// so a 10,000-ticket run costs exactly one probe.
    #[tokio::test]
    async fn account_timezone_discovers_from_myself() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_json(myself_body("America/New_York")))
            .mount(&server)
            .await;

        let client = client_for_discovery(&server);
        assert_eq!(
            client.account_timezone().await.expect("resolves"),
            chrono_tz::Tz::America__New_York
        );
        assert_eq!(
            client.account_timezone().await.expect("cached"),
            chrono_tz::Tz::America__New_York
        );
        let probes = server
            .received_requests()
            .await
            .expect("recorded")
            .into_iter()
            .filter(|r| r.url.path().ends_with("/myself"))
            .count();
        assert_eq!(probes, 1, "the zone must be cached like story_point_field");
    }

    /// When the zone cannot be determined the run FAILS rather than assuming
    /// UTC. Silently defaulting to UTC is the exact unstated assumption that
    /// made this a defect; the error names the config key that fixes it.
    #[tokio::test]
    async fn account_timezone_errors_when_undiscoverable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let Err(err) = client_for_discovery(&server).account_timezone().await else {
            panic!("must not silently assume UTC");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("jira.timezone"),
            "the error must name the remediation: {msg}"
        );
    }

    /// An unparseable zone is a config error naming the value, not a silent
    /// fallback.
    #[tokio::test]
    async fn account_timezone_rejects_an_unknown_zone() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_json(myself_body("Nowhere/Fake")))
            .mount(&server)
            .await;

        let Err(err) = client_for_discovery(&server).account_timezone().await else {
            panic!("must reject an unknown zone");
        };
        assert!(err.to_string().contains("Nowhere/Fake"));
    }

    /// A client with no pinned timezone, for the discovery tests above.
    fn client_for_discovery(server: &MockServer) -> JiraClient {
        let config = JiraConfig {
            url: Some(server.uri()),
            ..Default::default()
        };
        JiraClient::new(&config)
            .expect("client builds")
            .with_retry_policy(fast_retry())
    }

    /// A 429 must be retried *and* its `Retry-After` honoured. Round 1
    /// claimed the header was unreachable; it is not.
    #[tokio::test]
    async fn a_429_is_retried_and_its_retry_after_is_read() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        struct Throttling {
            calls: Arc<AtomicUsize>,
        }
        impl Respond for Throttling {
            fn respond(&self, _request: &Request) -> ResponseTemplate {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    return ResponseTemplate::new(429).insert_header("Retry-After", "0");
                }
                ResponseTemplate::new(200).set_body_json(json!({
                    "startAt": 0, "maxResults": 100, "total": 1,
                    "comments": [
                        {"id": "1", "created": "2026-01-05T09:30:00.000+0000", "body": "ok"}
                    ]
                }))
            }
        }
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-2/comment"))
            .respond_with(Throttling {
                calls: Arc::clone(&calls),
            })
            .mount(&server)
            .await;

        let comments = client_for(&server)
            .fetch_comments("PROJ-2")
            .await
            .expect("the 429 must be retried");
        assert_eq!(comments.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
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

    // ---- Comment pagination termination (PR #4067 review round 3) -------
    //
    // The loop used to terminate on `start_at >= parsed.total`, with `total`
    // a `#[serde(default)] u64`. A response omitting `total` therefore read
    // as `0` and ended the walk after page 1 — ingesting a prefix of the
    // ticket's comments and returning `Ok`, so the failed-ticket cursor
    // clamp let the cursor advance and the loss became permanent. These pin
    // the short-page termination that replaced it.

    /// One comment page of `n` entries, with NO `total` field at all.
    fn comment_page(start: usize, n: usize) -> serde_json::Value {
        let comments: Vec<serde_json::Value> = (start..start + n)
            .map(|i| json!({"id": i.to_string(), "created": "2026-01-05T09:30:00.000+0000", "body": "x"}))
            .collect();
        json!({"startAt": start, "maxResults": 100, "comments": comments})
    }

    /// Serves pages keyed off the `startAt` query parameter.
    struct PagedComments {
        /// Entry count for each successive page, in order.
        pages: Vec<usize>,
        calls: Arc<AtomicUsize>,
    }

    impl Respond for PagedComments {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let start_at: usize = request
                .url
                .query_pairs()
                .find(|(k, _)| k == "startAt")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Walk the declared page sizes to find which page `startAt` names.
            let mut offset = 0usize;
            for n in &self.pages {
                if offset == start_at {
                    return ResponseTemplate::new(200).set_body_json(comment_page(offset, *n));
                }
                offset += n;
            }
            ResponseTemplate::new(200).set_body_json(comment_page(start_at, 0))
        }
    }

    /// THE regression: a server that omits `total` must still yield every
    /// comment. Demonstrated shortfall before the fix was 100 of 150.
    #[tokio::test]
    async fn fetch_comments_pages_every_comment_when_total_is_absent() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-7/comment"))
            .respond_with(PagedComments {
                pages: vec![100, 50],
                calls: Arc::clone(&calls),
            })
            .mount(&server)
            .await;

        let comments = client_for(&server)
            .fetch_comments("PROJ-7")
            .await
            .expect("walk succeeds");

        assert_eq!(
            comments.len(),
            150,
            "every comment must be ingested even with no `total` to terminate on"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the 50-entry short page ends the walk; no third request"
        );
    }

    /// When the comment count is an exact multiple of the page size there is
    /// no short page to stop on, so the walk must spend one more request to
    /// see the empty page rather than guessing it is done.
    #[tokio::test]
    async fn fetch_comments_stops_after_an_empty_page_on_an_exact_multiple() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-8/comment"))
            .respond_with(PagedComments {
                pages: vec![100],
                calls: Arc::clone(&calls),
            })
            .mount(&server)
            .await;

        let comments = client_for(&server)
            .fetch_comments("PROJ-8")
            .await
            .expect("walk succeeds");

        assert_eq!(comments.len(), 100);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a full final page costs one empty probe to confirm the end"
        );
    }

    /// One comment page of `n` entries echoing the page size the server
    /// applied, which may be smaller than the one requested.
    fn comment_page_with_max(start: usize, n: usize, max: usize) -> serde_json::Value {
        let comments: Vec<serde_json::Value> = (start..start + n)
            .map(|i| json!({"id": i.to_string(), "created": "2026-01-05T09:30:00.000+0000", "body": "x"}))
            .collect();
        json!({"startAt": start, "maxResults": max, "comments": comments})
    }

    /// Serves fixed-size pages at `max` per page, up to `total` comments,
    /// echoing `maxResults: max` — a server applying a smaller page size than
    /// the client asked for. `max_results: None` omits the field entirely.
    struct ShrunkPages {
        max: usize,
        total: usize,
        echo_max: bool,
        calls: Arc<AtomicUsize>,
    }

    impl Respond for ShrunkPages {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let start_at: usize = request
                .url
                .query_pairs()
                .find(|(k, _)| k == "startAt")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let n = self.total.saturating_sub(start_at).min(self.max);
            let body = if self.echo_max {
                comment_page_with_max(start_at, n, self.max)
            } else {
                let mut b = comment_page_with_max(start_at, n, self.max);
                b.as_object_mut().expect("object").remove("maxResults");
                b
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    /// HIGH-1 regression (PR #4155 review, reproduced by the critic as
    /// "ingested 50 of 150 comments in 1 request(s), returned Ok").
    ///
    /// The walk asks for 100 per page; the server applies 50 and says so.
    /// Terminating on `n < COMMENT_PAGE_SIZE` compares against the size we
    /// REQUESTED, so page 1 reads as short and the walk ends after 50 of 150
    /// comments — with an `Ok` return, which means `run_sync` resets the
    /// failure streak, the cursor advances past the ticket, and the other 100
    /// comments are gone permanently. Exactly the fail-open this PR exists to
    /// remove, re-entered through its own replacement termination condition.
    #[tokio::test]
    async fn fetch_comments_honours_the_page_size_the_server_applied() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-11/comment"))
            .respond_with(ShrunkPages {
                max: 50,
                total: 150,
                echo_max: true,
                calls: Arc::clone(&calls),
            })
            .mount(&server)
            .await;

        let comments = client_for(&server)
            .fetch_comments("PROJ-11")
            .await
            .expect("walk succeeds");

        assert_eq!(
            comments.len(),
            150,
            "a server paging smaller than requested must not end the walk after \
             page 1; every comment must still be ingested"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "three 50-entry pages plus the empty probe that confirms the end"
        );
    }

    /// The companion to the above: with NO `maxResults` echoed there is no
    /// server-stated page size to trust, so the walk must fall back to the one
    /// claim that needs no server bookkeeping — an empty page — rather than
    /// guessing with the requested size.
    #[tokio::test]
    async fn fetch_comments_pages_to_empty_when_the_server_omits_max_results() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-12/comment"))
            .respond_with(ShrunkPages {
                max: 50,
                total: 120,
                echo_max: false,
                calls: Arc::clone(&calls),
            })
            .mount(&server)
            .await;

        let comments = client_for(&server)
            .fetch_comments("PROJ-12")
            .await
            .expect("walk succeeds");

        assert_eq!(
            comments.len(),
            120,
            "an unstated page size must never be treated as proof the walk is done"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    // ---- Changelog pagination fallback (issue #4084) --------------------
    //
    // These exercise the bug directly: JIRA's search-embedded changelog is
    // itself paged, and the entries it drops are the OLDEST — the exact
    // transitions a historical backfill exists to capture. Every test below
    // asserts either that the shortfall self-heals via the dedicated
    // `/issue/{key}/changelog` walk, or that it is surfaced loudly. None of
    // them may pass by returning a short history quietly.

    /// One changelog history entry in JIRA wire form. The same entry shape is
    /// used by the search-embedded `histories` array and the dedicated
    /// endpoint's `values` array.
    fn history_entry(created: &str, from: &str, to: &str) -> serde_json::Value {
        json!({
            "author": {"displayName": "Jane Doe"},
            "created": created,
            "items": [{"field": "status", "fromString": from, "toString": to}]
        })
    }

    /// The three-entry history used across these tests, oldest first. Only the
    /// last entry fits in a truncated embedded changelog.
    fn oldest() -> serde_json::Value {
        history_entry("2026-01-01T10:00:00.000+0000", "To Do", "In Progress")
    }
    fn middle() -> serde_json::Value {
        history_entry("2026-02-01T10:00:00.000+0000", "In Progress", "In Review")
    }
    fn newest() -> serde_json::Value {
        history_entry("2026-03-01T10:00:00.000+0000", "In Review", "Done")
    }

    /// A one-issue search response whose embedded changelog reports `total`
    /// entries while carrying only `histories`.
    fn search_body(total: u64, histories: Vec<serde_json::Value>) -> serde_json::Value {
        json!({
            "issues": [{
                "key": "PROJ-1",
                "fields": {"project": {"key": "PROJ"}, "updated": "2026-03-01T10:00:00.000+0000"},
                "changelog": {"total": total, "histories": histories}
            }]
        })
    }

    fn scope() -> SyncScope {
        SyncScope {
            project_key: "PROJ".into(),
            since: None,
        }
    }

    /// Count how many requests the mock server saw for a path suffix.
    async fn hits(server: &MockServer, suffix: &str) -> usize {
        server
            .received_requests()
            .await
            .expect("request recording enabled")
            .iter()
            .filter(|r| r.url.path().ends_with(suffix))
            .count()
    }

    /// The core regression: an embedded changelog reporting more entries than
    /// it carries must be FLAGGED, carrying the count that proves it short, so
    /// `run_sync` can repair it inside its per-ticket failure isolation.
    ///
    /// The search walk itself must issue no request for the repair — that is
    /// what stops one unreachable ticket from aborting the whole run before a
    /// single row is written (PR #4155 review).
    #[tokio::test]
    async fn search_with_changelog_flags_a_truncated_embedded_changelog() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            // Claims 3 entries; embeds only the newest.
            .respond_with(ResponseTemplate::new(200).set_body_json(search_body(3, vec![newest()])))
            .mount(&server)
            .await;
        // Deliberately NOT mounted: any call from the search walk would 404.

        let walk = client_for(&server)
            .search_with_changelog(&scope(), 10)
            .await
            .expect("search succeeds");

        assert_eq!(walk.issues.len(), 1);
        assert_eq!(
            walk.issues[0].truncated_history_total,
            Some(3),
            "the shortfall must be recorded with the count that proves it"
        );
        assert_eq!(
            hits(&server, "/changelog").await,
            0,
            "the repair is the caller's to make, inside per-ticket isolation"
        );
    }

    /// A complete embedded changelog must not be flagged, so it costs nothing.
    #[tokio::test]
    async fn search_with_changelog_does_not_flag_a_complete_embedded_changelog() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(search_body(2, vec![middle(), newest()])),
            )
            .mount(&server)
            .await;

        let walk = client_for(&server)
            .search_with_changelog(&scope(), 10)
            .await
            .expect("search succeeds");

        assert_eq!(walk.issues[0].transitions.len(), 2);
        assert_eq!(
            walk.issues[0].truncated_history_total, None,
            "a complete embedded changelog must not be sent for repair"
        );
        assert_eq!(hits(&server, "/changelog").await, 0);
    }

    /// A missing `changelog.total` cannot prove truncation, so it must not be
    /// flagged (it is warned about instead — see
    /// `embedded_changelog_is_truncated`).
    #[tokio::test]
    async fn missing_changelog_total_does_not_flag_truncation() {
        let server = MockServer::start().await;
        let body = json!({
            "issues": [{
                "key": "PROJ-1",
                "fields": {"project": {"key": "PROJ"}},
                "changelog": {"histories": [newest()]}
            }]
        });
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let walk = client_for(&server)
            .search_with_changelog(&scope(), 10)
            .await
            .expect("search succeeds");

        assert_eq!(walk.issues[0].transitions.len(), 1);
        assert_eq!(walk.issues[0].truncated_history_total, None);
        assert_eq!(hits(&server, "/changelog").await, 0);
    }

    /// The dedicated endpoint is itself paged; the walk must exhaust it,
    /// advancing `startAt` by the number of entries actually received.
    #[tokio::test]
    async fn fetch_changelog_pages_to_exhaustion() {
        use wiremock::matchers::query_param;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-9/changelog"))
            .and(query_param("startAt", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"startAt": 0, "maxResults": 100, "total": 3, "values": [oldest(), middle()]}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-9/changelog"))
            .and(query_param("startAt", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"startAt": 2, "maxResults": 100, "total": 3, "values": [newest()]}),
            ))
            .mount(&server)
            .await;

        let transitions = client_for(&server)
            .fetch_changelog("PROJ-9", None)
            .await
            .expect("full walk succeeds");

        assert_eq!(transitions.len(), 3);
        assert_eq!(transitions[0].to_status, "In Progress");
        assert_eq!(transitions[1].to_status, "In Review");
        assert_eq!(transitions[2].to_status, "Done");
        assert_eq!(hits(&server, "/changelog").await, 2, "both pages fetched");
    }

    /// A fallback that comes up short must ERROR with the ticket key and the
    /// expected-vs-retrieved counts — never return the partial history it did
    /// manage to collect. Replacing one silent truncation with another would
    /// be no fix at all.
    #[tokio::test]
    async fn fetch_changelog_errors_when_server_returns_fewer_than_total() {
        use wiremock::matchers::query_param;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-9/changelog"))
            .and(query_param("startAt", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"startAt": 0, "maxResults": 100, "total": 5, "values": [oldest(), middle()]}),
            ))
            .mount(&server)
            .await;
        // Server claims 5 entries but has no more to give.
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-9/changelog"))
            .and(query_param("startAt", "2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"startAt": 2, "maxResults": 100, "total": 5, "values": []}),
                ),
            )
            .mount(&server)
            .await;

        let err = client_for(&server)
            .fetch_changelog("PROJ-9", None)
            .await
            .expect_err("a short walk must not pass as a complete history");

        match &err {
            CollectError::IncompleteChangelog {
                key,
                expected,
                retrieved,
            } => {
                assert_eq!(key, "PROJ-9");
                assert_eq!(*expected, 5);
                assert_eq!(*retrieved, 2);
            }
            other => panic!("expected IncompleteChangelog, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains("PROJ-9"),
            "message must name the ticket: {msg}"
        );
        assert!(
            msg.contains('5') && msg.contains('2'),
            "counts missing: {msg}"
        );
    }

    /// HIGH-2 regression, part 1 (PR #4155 review, reproduced).
    ///
    /// `ChangelogPageResponse::total` was a `#[serde(default)] u64`, so an
    /// endpoint that omits `total` deserialized to `0`, `start_at >= 0` ended
    /// the walk after page 1, and `retrieved < 0` — false for every possible
    /// walk — let the completeness check pass vacuously. Page 1 was returned
    /// as a complete history. That is the identical `#[serde(default)] u64`
    /// fail-open `model.rs` argues against at length for the comment
    /// endpoint, re-entered two files away.
    #[tokio::test]
    async fn fetch_changelog_keeps_paging_when_the_endpoint_omits_total() {
        use wiremock::matchers::query_param;

        let server = MockServer::start().await;
        // No `total` on any page — the server volunteers nothing.
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-10/changelog"))
            .and(query_param("startAt", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"startAt": 0, "maxResults": 100, "values": [oldest(), middle()]}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-10/changelog"))
            .and(query_param("startAt", "2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"startAt": 2, "maxResults": 100, "values": [newest()]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-10/changelog"))
            .and(query_param("startAt", "3"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"startAt": 3, "maxResults": 100, "values": []})),
            )
            .mount(&server)
            .await;

        let transitions = client_for(&server)
            .fetch_changelog("PROJ-10", None)
            .await
            .expect("walk succeeds");

        assert_eq!(
            transitions.len(),
            3,
            "an absent `total` must not read as zero-entries-remaining and end \
             the walk after page 1"
        );
        assert_eq!(
            transitions[0].from_status.as_deref(),
            Some("To Do"),
            "the OLDEST entry is precisely what a page-1 stop drops"
        );
    }

    /// HIGH-2 regression, part 2 — the harm the critic actually reproduced:
    /// "fallback returned 1 transition(s), Ok, replacing the embedded set".
    ///
    /// The search proved this ticket has 3 history entries. The dedicated
    /// endpoint hands back 1 and states no `total`. Returning `Ok` here means
    /// the repair REPLACES the embedded history with something no larger than
    /// the truncated copy it was invoked to fix — a fallback that can lose
    /// data. The search's own count is what makes the shortfall provable, so
    /// it is passed in and the walk must error.
    #[tokio::test]
    async fn fetch_changelog_errors_when_an_absent_total_hides_a_shortfall() {
        use wiremock::matchers::query_param;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-10/changelog"))
            .and(query_param("startAt", "0"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"startAt": 0, "maxResults": 100, "values": [newest()]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-10/changelog"))
            .and(query_param("startAt", "1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"startAt": 1, "maxResults": 100, "values": []})),
            )
            .mount(&server)
            .await;

        let err = client_for(&server)
            .fetch_changelog("PROJ-10", Some(3))
            .await
            .expect_err("a repair that recovers less than the search proved exists must error");

        match &err {
            CollectError::IncompleteChangelog {
                key,
                expected,
                retrieved,
            } => {
                assert_eq!(key, "PROJ-10");
                assert_eq!(*expected, 3, "the search's count is the standing bound");
                assert_eq!(*retrieved, 1);
            }
            other => panic!("expected IncompleteChangelog, got {other:?}"),
        }
    }

    /// A ticket whose changelog endpoint is broken must NOT take the search
    /// walk down with it. The walk is what `run_sync` awaits in full before it
    /// writes anything, so an abort here means nothing is ingested for ANY
    /// ticket, the cursor never moves, and the next run reproduces it
    /// identically at the same ticket (PR #4155 review, HIGH-3).
    ///
    /// The end-to-end proof that the failure is isolated and progress is kept
    /// lives in `commands/jira/tests.rs::
    /// a_broken_changelog_repair_does_not_take_the_whole_run_down`.
    #[tokio::test]
    async fn a_broken_changelog_endpoint_does_not_abort_the_search_walk() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(search_body(3, vec![newest()])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-1/changelog"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let walk = client_for(&server)
            .search_with_changelog(&scope(), 10)
            .await
            .expect("a broken per-ticket endpoint must not abort the walk");

        assert_eq!(walk.issues.len(), 1);
        assert_eq!(
            walk.issues[0].truncated_history_total,
            Some(3),
            "the ticket is still flagged for repair; the caller decides what a \
             failed repair costs"
        );
    }

    /// The fallback REPLACES the embedded transitions rather than merging with
    /// them, so the entry present in both copies cannot be emitted twice — and
    /// the resulting rows cannot collide on `fact_ticket_transitions`'s
    /// `(ticket_key, transitioned_at, to_status)` primary key.
    #[tokio::test]
    async fn fallback_transitions_replace_embedded_without_duplication() {
        use crate::core::db::{upsert_ticket_transition, Database, TicketTransitionRow};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search"))
            // `newest()` appears in BOTH the embedded page and the full walk.
            .respond_with(ResponseTemplate::new(200).set_body_json(search_body(3, vec![newest()])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-1/changelog"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "startAt": 0, "maxResults": 100, "total": 3,
                "values": [oldest(), middle(), newest()]
            })))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let walk = client
            .search_with_changelog(&scope(), 10)
            .await
            .expect("search succeeds");
        let issue = &walk.issues[0];
        // The repair `run_sync` performs, exercised against the real endpoint.
        let transitions = client
            .fetch_changelog(&issue.key, issue.truncated_history_total)
            .await
            .expect("repair succeeds");
        assert_eq!(
            transitions.len(),
            3,
            "the shared entry must appear once, not twice"
        );
        assert_eq!(
            transitions[0].from_status.as_deref(),
            Some("To Do"),
            "the OLDEST transition is the one truncation drops; it must be present"
        );

        // Persist through the real writer and confirm the grain key holds.
        let db = Database::open_in_memory().expect("open");
        for t in &transitions {
            upsert_ticket_transition(
                db.connection(),
                &TicketTransitionRow {
                    ticket_key: issue.key.clone(),
                    project_key: issue.project_key.clone(),
                    from_status: t.from_status.clone(),
                    to_status: t.to_status.clone(),
                    transitioned_at: t.created.to_rfc3339(),
                    author: t.author.clone(),
                },
            )
            .expect("upsert");
        }
        let count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_ticket_transitions WHERE ticket_key = 'PROJ-1'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            count, 3,
            "every recovered transition is a distinct grain row"
        );
    }
}
