//! Tests for the JIRA REST client, changelog/comment parsing, and payload types.
//!
//! Loaded via `#[cfg(test)] #[path = "client_tests.rs"] mod tests;` in `client.rs`
//! (kept out-of-line so `client.rs` stays under the 500-SLOC production cap;
//! see `scripts/check_line_cap.sh`).

use super::*;

/// Confirm a JQL search response shape parses end-to-end.
///
/// Why: pagination logic depends on `total` and `startAt` fields; if
/// JIRA renames either, our loop terminates incorrectly.
/// What: parse a representative search payload with one issue.
/// Test: assert `total`, `startAt`, and inner issue fields all populate.
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
    assert_eq!(resp.start_at, 0);
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
