//! `tm pr queue-check`'s test-ladder rung and code-critic verdict gate
//! (owner ruling Dr4), plus `tm pr open --rung` writing the `rung:N` label.
//!
//! Every test drives the real `queue_check::verdicts` or `open::plan` through
//! the parent module's scripted `gh`; nothing touches the network.

use serde_json::{Value, json};

use super::super::open::{self, ChangelogVerdict};
use super::super::queue_check;
use super::{FakeGh, REQUIRED, full_body, open_args};
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

/// The PR's current head commit.
const HEAD: &str = "0a400b509db40cb102929fa28341367b1ddc8840";
/// An earlier head the critic reviewed before a later push.
const OLD_HEAD: &str = "608d739081ed23fbc8703847ff08d17adeaeaa74";

/// A view past every non-Dr4 gate: green checks, clean, approved.
fn view(labels: &[&str], comments: Value) -> String {
    let labels: Vec<Value> = labels.iter().map(|l| json!({ "name": l })).collect();
    json!({
        "isDraft": false, "labels": labels, "reviewDecision": "APPROVED",
        "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
        "statusCheckRollup": [
            {"name": "Clippy", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"name": "Rust tests", "status": "COMPLETED", "conclusion": "SUCCESS"}],
        "headRefOid": HEAD, "comments": comments, "reviews": []
    })
    .to_string()
}

/// One PR comment by an author with `role`.
fn comment(role: &str, body: &str, at: &str) -> Value {
    json!({"authorAssociation": role, "body": body, "createdAt": at, "url": "u"})
}

/// The first stop reason queue-check reports for `view_json`, if any.
fn reason(view_json: &str) -> Option<String> {
    let gh = FakeGh::new()
        .on("branches/main/protection", REQUIRED)
        .on("pr view", view_json);
    let required = queue_check::required_contexts(&gh, "o/r", "main").expect("contexts");
    let verdicts = queue_check::verdicts(&gh, "o/r", &required, &[1]).expect("verdicts");
    verdicts.into_iter().next().and_then(|v| v.reason)
}

#[test]
fn dr4_queue_check_reads_head_and_reviews() {
    let gh = FakeGh::new()
        .on("branches/main/protection", REQUIRED)
        .on("pr view", &view(&["rung:3"], json!([])));
    let required = queue_check::required_contexts(&gh, "o/r", "main").expect("contexts");
    queue_check::verdicts(&gh, "o/r", &required, &[1]).expect("verdicts");
    let read = gh
        .calls()
        .into_iter()
        .find(|c| c.get(1).map(String::as_str) == Some("view"))
        .expect("a pr view")
        .join(" ");
    for field in ["headRefOid", "reviews", "labels", "comments"] {
        assert!(read.contains(field), "{read}");
    }
}

#[test]
fn dr4_no_rung_label_is_refused() {
    let got = reason(&view(&[], json!([]))).expect("refused");
    assert_eq!(
        got,
        "no test-ladder rung recorded (label rung:N); open with tm pr open --rung"
    );
}

#[test]
fn dr4_two_rung_labels_are_refused() {
    let got = reason(&view(&["rung:3", "rung:5"], json!([]))).expect("refused");
    assert!(got.contains("more than one test-ladder rung"), "{got}");
}

#[test]
fn dr4_a_malformed_rung_label_is_refused() {
    for bad in ["rung:7", "rung:0", "rung:high", "Rung: "] {
        let got = reason(&view(&[bad], json!([]))).expect("refused");
        assert!(got.contains("malformed test-ladder label"), "{bad}: {got}");
    }
}

#[test]
fn dr4_rung_5_with_no_verdict_is_refused() {
    let got = reason(&view(&["rung:5"], json!([]))).expect("refused");
    assert_eq!(
        got,
        format!("rung 5 PR has no code-critic verdict on head {HEAD}")
    );
}

#[test]
fn dr4_rung_5_with_approve_pinned_to_an_old_sha_is_refused() {
    let body = format!("## Verdict: APPROVE pinned to {OLD_HEAD}");
    let comments = json!([comment("OWNER", &body, "2026-11-16T10:00:00Z")]);
    let got = reason(&view(&["rung:5"], comments)).expect("refused");
    assert!(
        got.contains("rung 5 PR has no code-critic verdict on head"),
        "{got}"
    );
}

#[test]
fn dr4_rung_6_with_approve_pinned_to_head_passes() {
    let body = format!("## Verdict: APPROVE pinned to {HEAD}");
    let comments = json!([comment("MEMBER", &body, "2026-11-16T10:00:00Z")]);
    assert_eq!(reason(&view(&["rung:6"], comments)), None);
}

#[test]
fn dr4_rung_5_with_warn_pinned_to_short_head_passes() {
    let body = format!("## Verdict: WARN pinned to {}", &HEAD[..10]);
    let comments = json!([comment("COLLABORATOR", &body, "2026-11-16T10:00:00Z")]);
    assert_eq!(reason(&view(&["rung:5"], comments)), None);
}

#[test]
fn dr4_rung_3_with_no_verdict_passes() {
    assert_eq!(reason(&view(&["rung:3"], json!([]))), None);
}

#[test]
fn dr4_an_untrusted_authors_verdict_is_ignored() {
    let body = format!("## Verdict: APPROVE pinned to {HEAD}");
    let comments = json!([comment("NONE", &body, "2026-11-16T10:00:00Z")]);
    let got = reason(&view(&["rung:5"], comments)).expect("refused");
    assert!(got.contains("no code-critic verdict on head"), "{got}");
}

// Migrated from `tests.rs` `queue_stop_order_prefers_critic_block`: the BLOCK
// stop still precedes the rung stops.
#[test]
fn dr4_stop_order_prefers_critic_block() {
    let comments = json!([comment(
        "OWNER",
        "## Verdict: BLOCK",
        "2026-11-16T10:00:00Z"
    )]);
    let got = reason(&view(&[], comments)).expect("refused");
    assert!(got.contains("code-critic BLOCK"), "{got}");
}

// Migrated from `queue_critic_block_then_approve_is_clear`.
#[test]
fn queue_critic_block_then_approve_is_clear() {
    let block = format!("## Verdict: BLOCK pinned to {HEAD}");
    let approve = format!("## Verdict: APPROVE pinned to {HEAD}");
    let comments = json!([
        comment("OWNER", &block, "2026-11-16T10:00:00Z"),
        comment("OWNER", &approve, "2026-11-16T11:00:00Z"),
    ]);
    assert_eq!(reason(&view(&["rung:5"], comments)), None);
}

// Migrated from `queue_critic_ignores_unrelated_comments`.
#[test]
fn queue_critic_ignores_unrelated_comments() {
    let comments = json!([comment(
        "OWNER",
        "we should BLOCK bad merges in general, says code-critic",
        "2026-11-16T10:00:00Z"
    )]);
    assert_eq!(reason(&view(&["rung:3"], comments)), None);
}

#[test]
fn dr4_open_rung_5_applies_the_rung_5_label() {
    let mut args = open_args("/dev/null");
    args.rung = Some(5);
    let plan = open::plan(
        &args,
        &full_body(),
        Some("tm-test-01"),
        ChangelogVerdict::Pass,
        &ResolvedTicketing::default(),
    )
    .expect("a complete body plans");
    let joined = plan.argv.join(" ");
    assert!(joined.contains("--label rung:5"), "{joined}");
    assert!(plan.create_labels().contains(&"rung:5".to_string()));
    let (_, seed) = plan.rung.as_ref().expect("a rung seed");
    assert_eq!(&seed[..3], ["label", "create", "rung:5"]);
    assert!(seed.contains(&"--force".to_string()), "{seed:?}");
}
