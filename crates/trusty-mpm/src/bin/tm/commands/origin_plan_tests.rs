//! Tests for the origin-remote decision (#6276).
//!
//! Why: this decision is what a first-ever `tm` run in a plain directory hits
//! immediately after #6274's auto-`git init`. The no-origin arm must PROCEED;
//! the two arms for a repository that has a remote must be byte-identical to
//! the pre-#6276 behavior.
//! What: pure tests for [`super::plan_for_origin`] across the three arms, an
//! SSH-alias remote, and the notice text.
//! Test: this file.

use std::path::Path;

use super::{OriginPlan, live_checkout_notice, plan_for_origin};

/// A repository with no origin remote proceeds to a session in its own
/// checkout.
///
/// Why: this is the whole of #6276. Before the fix both callers answered
/// `None` with a refusal, so there was no proceed variant to return — this
/// test does not compile against the pre-fix tree and fails against any
/// implementation that keeps refusing.
/// Test: this is the test.
#[test]
fn plan_sends_a_repo_with_no_origin_to_the_live_checkout() {
    assert_eq!(
        plan_for_origin(None),
        OriginPlan::LiveCheckout,
        "#6276: a local-only repository must proceed, not refuse"
    );
}

/// A GitHub origin still takes the managed-clone path.
///
/// Why: #6276 must not touch the auto-managed case — regression coverage for
/// "behavior for repos WITH an origin remote is unchanged".
/// Test: this is the test.
#[test]
fn plan_sends_a_github_origin_to_the_managed_clone() {
    let url = "https://github.com/bobmatnyc/trusty-tools.git";
    assert_eq!(plan_for_origin(Some(url)), OriginPlan::ManagedClone(url));
}

/// A GitHub SSH host alias is still GitHub.
///
/// Why: `git@github-duetto:owner/repo.git` reaches the managed clone through
/// `is_github_remote`'s alias rule; routing it to the live checkout would
/// silently downgrade every multi-account setup.
/// Test: this is the test.
#[test]
fn plan_sends_a_github_ssh_alias_origin_to_the_managed_clone() {
    let url = "git@github-duetto:duettoresearch/aria.git";
    assert_eq!(plan_for_origin(Some(url)), OriginPlan::ManagedClone(url));
}

/// A non-GitHub origin is still refused.
///
/// Why: the live-checkout guard for Gitea/GitLab/bare-SSH remotes (#1724,
/// #1777) is untouched by #6276 — only the no-remote case changed.
/// Test: this is the test.
#[test]
fn plan_refuses_a_non_github_origin() {
    for url in [
        "https://gitea.example.com/org/repo.git",
        "git@gitlab.com:org/repo.git",
        "https://bitbucket.org/org/repo.git",
    ] {
        assert_eq!(
            plan_for_origin(Some(url)),
            OriginPlan::RefuseNonGitHub(url),
            "a remote on {url} must keep the pre-#6276 refusal"
        );
    }
}

/// An empty remote string is a remote, not an absent one.
///
/// Why: `plan_for_origin` keys on `Option`, never on emptiness — a caller that
/// hands it `Some("")` has read something from git and must not be routed into
/// the local-only arm.
/// Test: this is the test.
#[test]
fn plan_treats_an_empty_remote_string_as_a_remote() {
    assert_eq!(plan_for_origin(Some("")), OriginPlan::RefuseNonGitHub(""));
}

/// The notice names the checkout and reads as a notice.
///
/// Why: the message that used to terminate the launch keeps its information —
/// where the session runs, and that no clone is made — without the "run `tm
/// connect` instead" instruction, which is now what already happens.
/// Test: this is the test.
#[test]
fn live_checkout_notice_names_the_checkout_and_does_not_read_as_a_refusal() {
    let msg = live_checkout_notice(Path::new("/tmp/local-only-6276"));
    assert!(
        msg.contains("/tmp/local-only-6276"),
        "the notice must name the checkout: {msg:?}"
    );
    assert!(
        msg.contains("no git origin remote"),
        "the notice must still say why there is no managed clone: {msg:?}"
    );
    for refusal_word in ["Run `tm connect`", "require", "not auto-managing"] {
        assert!(
            !msg.contains(refusal_word),
            "#6276: the notice must not read as a refusal ({refusal_word:?}): {msg:?}"
        );
    }
}
