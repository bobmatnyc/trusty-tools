//! Unit tests for `tm hook --egress-guard` (#8454 S1): the family table and
//! the panic catch. The binary-level claims live in
//! `tests/tm_hook_egress_guard_8454.rs`.

use super::*;
use crate::commands::egress_guard_families::{Classification, FamilyTable};

/// The family `tool` classifies to under the built-in table, or a panic.
fn family_of(tool: &str) -> String {
    match FamilyTable::builtin().classify(tool) {
        Classification::Family(family) => family,
        other => panic!("`{tool}` must be a family tool, got {other:?}"),
    }
}

/// A guard run that panics, typed as the guard's run output.
fn panicking_run() -> Option<serde_json::Value> {
    panic!("injected egress-guard panic")
}

/// 🔴 #8454 S1 (§4, Architect Q5): a panic inside the guard still prints a
/// deny. Claude Code treats a non-zero hook exit as non-blocking, so an
/// unwinding guard would fail open.
#[tokio::test]
async fn egress_injected_panic_still_denies() {
    let response = deny_on_panic(async { panicking_run() }).await;
    let response = response.expect("a panic must produce a deny, not an allow");
    assert_eq!(response["hookSpecificOutput"]["permissionDecision"], "deny");
    let reason = response["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    assert!(reason.contains("#8454"), "{reason}");
    // A run that completes keeps its own answer.
    assert_eq!(deny_on_panic(async { None }).await, None);
}

/// 🔴 #8454 S1 (fail-open audit 5, 6): the server segment is matched after
/// lowercasing and dropping separators, so casing and separator lookalikes of
/// each family name are caught.
#[test]
fn egress_family_match_normalizes_case_and_separators() {
    for (tool, family) in [
        ("mcp__slack__x", "slack"),
        ("mcp__SLACK-MCP__x", "slack"),
        ("mcp__claude_ai_Slack__slack_send_message", "slack"),
        ("mcp__S.l.a.c.k__x", "slack"),
        ("mcp__G_Mail__x", "gmail"),
        ("mcp__claude_ai_Gmail__x", "gmail"),
        ("mcp__Not-Ion__x", "notion"),
        ("mcp__google_chat__x", "googlechat"),
        ("mcp__G-Chat__x", "gchat"),
        ("MCP__Slack__x", "slack"),
        ("mcp__google-mail__x", "googlemail"),
        // #8454 final round: a gmail tool is a mail tool whatever the server.
        ("mcp__mcp-gsuite__create_gmail_draft", "gmail"),
        ("mcp__mcp-gsuite__reply_gmail_email", "gmail"),
        ("mcp__workspace-mcp__send_gmail_message", "gmail"),
        ("mcp__claude_ai_Google_Mail__x", "googlemail"),
    ] {
        assert_eq!(family_of(tool), family, "{tool}");
    }
}

/// 🔴 #8454 S1: a gworkspace server is denied for its mail, chat and message
/// tools only; its documents, drive and calendar tools are not this family.
#[test]
fn egress_gworkspace_denies_only_mail_chat_message_tools() {
    // #8454 final round: a gmail tool reports the gmail family on any server.
    for tool in [
        "mcp__gworkspace-mcp__search_gmail_messages",
        "mcp__google_workspace__send_gmail_message",
    ] {
        assert_eq!(family_of(tool), "gmail", "{tool}");
    }
    for tool in [
        "mcp__gworkspace-mcp__compose_email",
        "mcp__gworkspace-mcp__send_draft",
        "mcp__G_Workspace__list_chat_spaces",
        "mcp__gworkspace__list_message_attachments",
        "mcp__Google-Workspace__send_chat_message",
        "mcp__mcp-gsuite__send_chat_message",
    ] {
        assert_eq!(family_of(tool), "gworkspace", "{tool}");
    }
    for tool in [
        "mcp__mcp-gsuite__list_calendars",
        "mcp__workspace-mcp__search_drive_files",
        "mcp__google_workspace__create_document",
        "mcp__gworkspace-mcp__create_document",
        "mcp__gworkspace-mcp__search_drive_files",
        "mcp__gworkspace-mcp__list_tasks",
    ] {
        assert_eq!(
            FamilyTable::builtin().classify(tool),
            Classification::Unrelated,
            "{tool}"
        );
    }
}

/// 🔴 #8454 S1 (§4): an `mcp__` name the guard cannot split into a non-empty
/// server and tool is malformed, never unrelated.
#[test]
fn egress_short_or_hollow_mcp_names_are_malformed() {
    for tool in [
        "mcp__slack",
        "mcp__",
        "MCP__x",
        "mcp____send",
        "mcp__github__",
    ] {
        assert!(
            matches!(
                FamilyTable::builtin().classify(tool),
                Classification::Malformed(_)
            ),
            "{tool}"
        );
    }
}

/// 🔴 #8454 S1 (Architect Q6): configured patterns only add to the built-in
/// table — every built-in family still matches, the added one now matches too,
/// and an empty pattern neither removes a family nor denies every tool.
#[test]
fn egress_config_patterns_add_and_never_remove() {
    let table = FamilyTable::builtin().with_added(["", "  ", "Zap-ier", "SLACK"]);
    for tool in [
        "mcp__slack__x",
        "mcp__gmail__x",
        "mcp__notion__x",
        "mcp__googlechat__x",
        "mcp__gchat__x",
        "mcp__gworkspace__compose_email",
    ] {
        assert!(
            matches!(table.classify(tool), Classification::Family(_)),
            "{tool}"
        );
    }
    assert_eq!(
        table.classify("mcp__zapier__run_action"),
        Classification::Family("zapier".to_owned())
    );
    assert_eq!(
        table.classify("mcp__github__create_issue"),
        Classification::Unrelated
    );
}

/// 🔴 #8454 S2 (drift guard): the registered timeout exceeds S1's worst case —
/// the gateway probe, the stdin budget, the daemon verdict query and the
/// inline policy load it falls back to (#8454 VC, R2), and the audit POST. A
/// hook killed at its timeout is non-blocking, an allow, so lowering the
/// timeout or raising any budget past it trips this test.
#[test]
fn egress_hook_timeout_exceeds_the_guard_budget() {
    use crate::commands::egress_verdict_client::VERDICT_QUERY_TIMEOUT;
    use crate::commands::hook_stdin::PM_GUARD_STDIN_TIMEOUT;
    use crate::commands::pm_guard_deny_log::AUDIT_POST_TIMEOUT;
    use trusty_mpm::core::channels_readiness::HOOK_LOAD_LIMIT;
    use trusty_mpm::core::discovery::GATEWAY_PROBE_TIMEOUT;
    use trusty_mpm::core::standalone::hooks::egress::EGRESS_GUARD_TIMEOUT_SECS;

    let budget = GATEWAY_PROBE_TIMEOUT
        + PM_GUARD_STDIN_TIMEOUT
        + VERDICT_QUERY_TIMEOUT
        + HOOK_LOAD_LIMIT
        + AUDIT_POST_TIMEOUT;
    let registered = std::time::Duration::from_secs(EGRESS_GUARD_TIMEOUT_SECS);
    assert!(
        registered > budget,
        "egress hook timeout {registered:?} must exceed the guard's worst case {budget:?}"
    );
}

/// 🔴 #8454 S2 (Architect D1): the trust-anchor floor answers before the
/// family check, so an anchor write is denied as one; an anchor allow leaves
/// the family verdict as it was.
#[test]
fn egress_trust_anchor_verdict_precedes_the_family_check() {
    let table = FamilyTable::builtin();
    let write = Ok(serde_json::json!({
        "tool_name": "Write",
        "tool_input": { "file_path": "/x/config.yaml", "content": "x" },
    }));
    match verdict(&write, &table, |_| Some("anchored".to_owned())) {
        Verdict::Deny { check, reason } => {
            assert_eq!(check, "trust-anchor-write");
            assert_eq!(reason, "anchored");
        }
        Verdict::Allow => panic!("an anchor deny must deny"),
    }
    assert_eq!(verdict(&write, &table, |_| None), Verdict::Allow);
    let slack = Ok(serde_json::json!({
        "tool_name": "mcp__slack__send_message",
        "tool_input": {},
    }));
    assert!(matches!(
        verdict(&slack, &table, |_| None),
        Verdict::Deny {
            check: "egress-family",
            ..
        }
    ));
}
