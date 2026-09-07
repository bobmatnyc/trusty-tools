//! Unit tests for the `gh` CLI ticketing adapter.
//!
//! Why: Construction, label extraction, and status mapping are pure and worth
//! coverage; network/CLI calls are env-dependent and out of scope here.
//! What: `gh_available_returns_bool`, label/status parsing, and edit-call
//! planning tests.
//! Test: This module is itself the test coverage.

use super::*;
use crate::ticketing::TicketingClient;

use serde_json::json;

#[test]
fn gh_cli_client_new_with_repo() {
    let c = GhCliClient::new(Some("owner/repo".to_string()));
    assert_eq!(c.repo.as_deref(), Some("owner/repo"));
}

#[test]
fn gh_cli_client_new_without_repo() {
    let c = GhCliClient::new(None);
    assert!(c.repo.is_none());
}

#[test]
fn provider_name_is_github_gh_cli() {
    let c = GhCliClient::new(None);
    assert_eq!(c.provider_name(), "github-gh-cli");
}

#[tokio::test]
async fn gh_available_returns_bool() {
    // Just assert it returns without panic — env-dependent (CI may or
    // may not have gh installed and authed).
    let _ = gh_available().await;
}

#[test]
fn ticket_state_mapping() {
    assert_eq!(parse_gh_state("OPEN"), TicketStatus::Open);
    assert_eq!(parse_gh_state("CLOSED"), TicketStatus::Closed);
    // Lowercase form (defensive) also works.
    assert_eq!(parse_gh_state("closed"), TicketStatus::Closed);
    // Anything unknown defaults to Open.
    assert_eq!(parse_gh_state("UNKNOWN"), TicketStatus::Open);
}

#[test]
fn label_extraction_from_gh_json() {
    let labels = json!([
        {"id": "1", "name": "bug", "color": "red"},
        {"id": "2", "name": "feature", "color": "blue"},
    ]);
    let extracted = extract_labels(Some(&labels));
    assert_eq!(extracted, vec!["bug".to_string(), "feature".to_string()]);
}

#[test]
fn label_extraction_handles_empty_or_missing() {
    assert!(extract_labels(None).is_empty());
    assert!(extract_labels(Some(&json!([]))).is_empty());
    // Object missing 'name' is filtered out.
    let bad = json!([{"id": "1"}, {"name": "ok"}]);
    assert_eq!(extract_labels(Some(&bad)), vec!["ok".to_string()]);
}

#[test]
fn gh_issue_to_ticket_parses_canonical_fields() {
    let v = json!({
        "number": 42,
        "title": "Fix bug",
        "body": "Repro steps…",
        "state": "OPEN",
        "labels": [{"name": "bug"}],
        "url": "https://github.com/o/r/issues/42",
        "createdAt": "2024-01-01T00:00:00Z",
        "updatedAt": "2024-01-02T00:00:00Z",
        "assignees": [{"login": "alice"}],
    });
    let t = gh_issue_to_ticket(&v).expect("parses");
    assert_eq!(t.id, "42");
    assert_eq!(t.title, "Fix bug");
    assert_eq!(t.status, TicketStatus::Open);
    assert_eq!(t.labels, vec!["bug".to_string()]);
    assert_eq!(t.assignee.as_deref(), Some("alice"));
    assert!(t.url.is_some());
    assert!(t.created_at.is_some());
    assert!(t.updated_at.is_some());
}

#[test]
fn gh_issue_to_ticket_requires_number() {
    let v = json!({"title": "no number"});
    assert!(gh_issue_to_ticket(&v).is_err());
}

/// Empty request → no `gh` calls planned.
#[test]
fn plan_gh_issue_edit_calls_empty_request_emits_nothing() {
    let req = UpdateTicketReq::default();
    let plan = plan_gh_issue_edit_calls("42", &req);
    assert!(plan.is_empty(), "expected no calls, got {:?}", plan);
}

/// #248 C2: `add_labels` produces a dedicated `--add-label` call even
/// when no other fields are set.
#[test]
fn plan_gh_issue_edit_calls_add_labels_emits_add_label_call() {
    let req = UpdateTicketReq {
        add_labels: Some(vec!["bug".into(), "p0".into()]),
        ..Default::default()
    };
    let plan = plan_gh_issue_edit_calls("42", &req);
    assert_eq!(plan.len(), 1, "expected single call, got {:?}", plan);
    assert_eq!(
        plan[0],
        vec![
            "issue".to_string(),
            "edit".into(),
            "42".into(),
            "--add-label".into(),
            "bug,p0".into(),
        ]
    );
}

/// #248 C2: `remove_labels` produces a `--remove-label` call.
#[test]
fn plan_gh_issue_edit_calls_remove_labels_emits_remove_label_call() {
    let req = UpdateTicketReq {
        remove_labels: Some(vec!["wontfix".into()]),
        ..Default::default()
    };
    let plan = plan_gh_issue_edit_calls("7", &req);
    assert_eq!(plan.len(), 1);
    assert_eq!(
        plan[0],
        vec![
            "issue".to_string(),
            "edit".into(),
            "7".into(),
            "--remove-label".into(),
            "wontfix".into(),
        ]
    );
}

/// #248 C2: `add_labels` + `remove_labels` both emit, in order.
#[test]
fn plan_gh_issue_edit_calls_add_and_remove_labels_both_emit() {
    let req = UpdateTicketReq {
        add_labels: Some(vec!["bug".into()]),
        remove_labels: Some(vec!["needs-triage".into()]),
        ..Default::default()
    };
    let plan = plan_gh_issue_edit_calls("99", &req);
    assert_eq!(plan.len(), 2, "expected 2 calls, got {:?}", plan);
    assert!(plan[0].contains(&"--add-label".to_string()));
    assert!(plan[1].contains(&"--remove-label".to_string()));
}

/// Empty `add_labels: Some(vec![])` is a no-op (no spurious empty call).
#[test]
fn plan_gh_issue_edit_calls_empty_label_vecs_are_noop() {
    let req = UpdateTicketReq {
        add_labels: Some(vec![]),
        remove_labels: Some(vec![]),
        ..Default::default()
    };
    assert!(plan_gh_issue_edit_calls("1", &req).is_empty());
}

/// #6953: `gh label list` pages at 30 by default and never says it truncated,
/// so the listing must ask for an explicit, much larger `--limit`. Against the
/// pre-fix argv (`label list --json name,color,description`) this fails: the
/// `--limit` lookup finds nothing.
#[test]
fn label_list_command_requests_more_than_the_gh_default() {
    let argv: Vec<String> = GhCliClient::new(None)
        .label_list_command()
        .argv_display()
        .split(' ')
        .map(str::to_string)
        .collect();
    let limit: usize = argv
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| argv.get(i + 1))
        .unwrap_or_else(|| panic!("`gh label list` argv must carry an explicit --limit: {argv:?}"))
        .parse()
        .expect("--limit is a number");
    assert_eq!(limit, LABEL_LIST_LIMIT);
    assert!(
        limit > GH_DEFAULT_LABEL_PAGE,
        "the listing must ask for more than gh's default {GH_DEFAULT_LABEL_PAGE}-label page"
    );
}

/// #6953: adding `--limit` must not cost the repo selector.
#[test]
fn label_list_command_carries_the_repo_selector() {
    let argv = GhCliClient::new(Some("owner/repo".to_string()))
        .label_list_command()
        .argv_display();
    assert_eq!(
        argv,
        "--repo owner/repo label list --limit 1000 --json name,color,description"
    );
}

/// #6953: the 31st label of a 35-label repo — the first one gh's default page
/// drops — must come back from the parse.
#[test]
fn labels_from_json_reads_past_the_default_label_page() {
    let page: Vec<Value> = (1..=35)
        .map(|n| json!({"name": format!("label-{n}"), "color": "ededed"}))
        .collect();
    let tags = labels_from_json(&page, LABEL_LIST_LIMIT).expect("a 35-label page is not truncated");
    assert_eq!(tags.len(), 35);
    assert!(
        tags.iter().any(|t| t.name == "label-31"),
        "the 31st label must survive the parse"
    );
    assert_eq!(tags[34].name, "label-35");
}

/// #6953: a page exactly as long as the requested limit may itself have been
/// truncated, so it is an error rather than a set treated as complete.
#[test]
fn labels_from_json_rejects_a_full_page() {
    let page: Vec<Value> = (0..LABEL_LIST_LIMIT)
        .map(|n| json!({"name": format!("label-{n}")}))
        .collect();
    let err =
        labels_from_json(&page, LABEL_LIST_LIMIT).expect_err("a full page must not be trusted");
    assert!(
        err.to_string().contains("full requested page"),
        "unexpected error: {err}"
    );
}

/// An entry with no `name` is dropped, not fatal.
#[test]
fn labels_from_json_skips_entries_without_a_name() {
    let page = vec![json!({"color": "ededed"}), json!({"name": "bug"})];
    let tags = labels_from_json(&page, LABEL_LIST_LIMIT).expect("parses");
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "bug");
}

/// Title + add_labels emits a combined main edit AND a separate
/// add-label call (deltas are never folded into the main edit).
#[test]
fn plan_gh_issue_edit_calls_combines_main_and_label_delta() {
    let req = UpdateTicketReq {
        title: Some("New title".into()),
        add_labels: Some(vec!["bug".into()]),
        ..Default::default()
    };
    let plan = plan_gh_issue_edit_calls("5", &req);
    assert_eq!(plan.len(), 2);
    // Main edit comes first, with --title.
    assert!(plan[0].contains(&"--title".to_string()));
    // Label delta is the second invocation.
    assert!(plan[1].contains(&"--add-label".to_string()));
}
