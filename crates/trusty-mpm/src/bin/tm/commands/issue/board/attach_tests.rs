//! Tests for `tm issue board add` and D10's attach-failure arms (#10276, AC6,
//! AC8).
//!
//! Why: D10 lets exactly one failure degrade to a comment and exit 0; every
//! other arm must exit non-zero and post nothing. One test per arm, each fed
//! the error text `gh` prints for it.
//! What: `add` against the fake `gh` with the `item-add` call failing.
//! Test: this IS the test module (classified test file: 3000-SLOC cap).

use super::attach::{
    AttachFailure, AttachOutcome, NO_PROJECT_PREFIX, Target, add, classify, resolve_target,
};
use super::client::BoardClient;
use super::error::BoardError;
use super::fake_gh_tests::{FakeGh, Sim, is_mutation};
use super::project::ProjectBlock;
use super::spec::embedded_spec;
use super::verbs::check_verb;

/// gh's wording when the token lacks the project scope.
const SCOPE: &str = "your authentication token is missing required scopes [project]\nTo request it, run:  gh auth refresh -s project";
/// An SSO-protected organisation refusing the token.
const SSO: &str = "GraphQL: Resource protected by organization SAML enforcement. You must grant your Personal Access token access to this organization.";
/// #9750's subagent `gh` failure, verbatim from the issue.
const ISSUE_9750: &str = "command not found: env -u GH_TOKEN -u GITHUB_TOKEN gh";

fn fake() -> FakeGh {
    let mut sim = Sim {
        probe: (
            "HTTP/2.0 200 OK\r\nX-Oauth-Scopes: project\r\n\r\n{}".into(),
            true,
        ),
        ..Sim::default()
    };
    sim.new_project("o", 45, "board");
    sim.new_project("o", 3, "trusty-mpm");
    FakeGh::new(sim)
}

fn target(number: u64) -> Target {
    Target {
        owner: "o".into(),
        number,
        source: "test".into(),
    }
}

fn comments(gh: &FakeGh, issue: u64) -> Vec<String> {
    gh.sim
        .borrow()
        .comments
        .get(&issue)
        .cloned()
        .unwrap_or_default()
}

fn posted_a_comment(gh: &FakeGh) -> bool {
    gh.calls()
        .iter()
        .any(|c| is_mutation(c) && c.iter().any(|a| a == "comment"))
}

/// AC8: the classifier, including #9750's token failure as a non-scope error.
#[test]
fn attach_failures_are_classified_and_9750_is_not_a_scope_refusal_10276() {
    assert_eq!(classify(SCOPE), AttachFailure::MissingScope);
    assert_eq!(
        classify("your token has not been granted the required scopes to execute this query"),
        AttachFailure::MissingScope
    );
    assert_eq!(classify(SSO), AttachFailure::SsoRequired);
    assert_eq!(
        classify("Could not resolve to a ProjectV2 with the number 99."),
        AttachFailure::ProjectNotFound
    );
    assert_eq!(classify("HTTP 502: Bad gateway"), AttachFailure::Other);
    assert_eq!(classify(ISSUE_9750), AttachFailure::Other);
    // An SSO refusal that also mentions scopes still does not degrade.
    assert_eq!(
        classify(&format!("{SSO} (required scopes: read:org)")),
        AttachFailure::SsoRequired
    );
}

/// AC8: missing scope → one `no-project:` comment, exit 0; a rerun posts no
/// second comment.
#[test]
fn add_missing_scope_posts_one_no_project_comment_and_exits_zero_10276() {
    let gh = fake();
    let client = BoardClient::new(&gh);
    gh.fail_once("item-add", SCOPE);
    let first = add(&client, &target(45), "o/r", 7).expect("exit 0");
    assert!(
        matches!(first, AttachOutcome::Waived { posted: true, .. }),
        "{first:?}"
    );
    gh.fail_once("item-add", SCOPE);
    let second = add(&client, &target(45), "o/r", 7).expect("exit 0 again");
    assert!(
        matches!(second, AttachOutcome::Waived { posted: false, .. }),
        "{second:?}"
    );
    let posted = comments(&gh, 7);
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert!(posted[0].starts_with(NO_PROJECT_PREFIX), "{posted:?}");
}

/// Assert `add` fails with `kind` and posts no comment.
fn refused_without_comment(stderr: &str, number: u64, kind: &str) {
    let gh = fake();
    if !stderr.is_empty() {
        gh.fail_once("item-add", stderr);
    }
    let err = add(&BoardClient::new(&gh), &target(number), "o/r", 7).unwrap_err();
    assert!(
        matches!(&err, BoardError::AttachFailed { kind: k, issue: 7, .. } if *k == kind),
        "{err}"
    );
    assert!(
        err.to_string().contains("not the token's project scope"),
        "{err}"
    );
    assert!(comments(&gh, 7).is_empty());
    assert!(!posted_a_comment(&gh), "{:?}", gh.calls());
}

/// AC8: a 502 exits non-zero with no comment.
#[test]
fn add_502_exits_non_zero_without_a_comment_10276() {
    refused_without_comment(
        "HTTP 502: Bad gateway (https://api.github.com/graphql)",
        45,
        "other failure",
    );
}

/// AC8: an SSO 403 exits non-zero with no comment.
#[test]
fn add_sso_403_exits_non_zero_without_a_comment_10276() {
    refused_without_comment(SSO, 45, "SSO authorisation required");
}

/// AC8: a deleted project exits non-zero with no comment.
#[test]
fn add_deleted_project_exits_non_zero_without_a_comment_10276() {
    refused_without_comment("", 99, "project not found");
}

/// AC8: #9750's token failure exits non-zero with no comment.
#[test]
fn add_9750_token_failure_exits_non_zero_without_a_comment_10276() {
    refused_without_comment(ISSUE_9750, 45, "other failure");
}

/// AC6: the `project:` block wins over `default_project`, which is not even
/// read; `check` reports the mismatch; without a block the title resolves.
#[test]
fn add_uses_the_project_block_and_ignores_default_project_10276() {
    let gh = fake();
    let client = BoardClient::new(&gh);
    let block = ProjectBlock {
        owner: "o".into(),
        number: 45,
        drop_options: Vec::new(),
        extra_fields: Vec::new(),
    };
    let chosen = resolve_target(&client, Some(&block), Some("trusty-mpm"), "o/r").expect("target");
    assert_eq!(chosen.number, 45);
    assert!(
        !gh.calls()
            .iter()
            .any(|c| c.iter().any(|a| a.contains("OwnerProjects"))),
        "default_project is not looked up when a block exists"
    );
    let outcome = add(&client, &chosen, "o/r", 7).expect("attached");
    assert_eq!(outcome, AttachOutcome::Attached);
    let item_add = gh
        .calls()
        .into_iter()
        .find(|c| c.iter().any(|a| a == "item-add"))
        .expect("item-add ran");
    assert_eq!(
        item_add[1..],
        [
            "project",
            "item-add",
            "45",
            "--owner",
            "o",
            "--url",
            "https://github.com/o/r/issues/7"
        ]
    );

    let report = check_verb(
        &client,
        &embedded_spec().expect("spec"),
        &block,
        Some("trusty-mpm"),
        "o/r",
    )
    .expect("check");
    assert!(
        report
            .mismatch
            .as_deref()
            .is_some_and(|m| m.contains("`trusty-mpm`") && m.contains("#45")),
        "{report:?}"
    );

    let fallback = resolve_target(&client, None, Some("trusty-mpm"), "o/r").expect("fallback");
    assert_eq!(fallback.number, 3);
    let none = resolve_target(&client, None, None, "o/r").unwrap_err();
    assert!(matches!(none, BoardError::NoAttachTarget(_)), "{none}");
}
