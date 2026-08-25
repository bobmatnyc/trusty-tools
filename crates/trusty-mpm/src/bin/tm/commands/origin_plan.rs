//! What the launch path does about a repository's `origin` remote (#6276).
//!
//! Why: `tm` used to end at a refusal whenever a git repository had no origin
//! remote — bare `tm` bailed out of [`super::guided::fallback_protected`] and
//! `tm launch` bailed with "no git origin remote found". After #6274 auto-`git
//! init`, that refusal became the end-state of a first-ever `tm` run in a plain
//! directory: the repository was created and the very next step refused it.
//! Owner ruling 2026-08-25: a local-only repository proceeds to a working
//! session in its own checkout. A repository that HAS an origin remote is
//! unaffected — GitHub still gets the managed clone, and a non-GitHub host is
//! still refused.
//! What: [`plan_for_origin`] is the one decision, taken from the origin URL
//! alone, plus [`live_checkout_notice`] — the message that used to terminate
//! the launch and is now a notice printed before the session starts.
//! Test: `origin_plan_tests.rs`.

use std::path::Path;

/// What the launch path does with a repository, given its `origin` remote.
///
/// Why: the two callers ([`super::guided::fallback_protected`] and
/// [`super::launch::launch`]) each used to make this call inline, and each
/// answered "no origin remote" with a refusal. One enum makes the answer
/// reviewable and testable without a daemon, a tmux server, or a network.
/// What: three arms, keyed only on the origin URL. `ManagedClone` and
/// `RefuseNonGitHub` carry the remote so the caller does not re-derive it.
/// Test: `plan_sends_a_repo_with_no_origin_to_the_live_checkout`,
/// `plan_sends_a_github_origin_to_the_managed_clone`,
/// `plan_refuses_a_non_github_origin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OriginPlan<'a> {
    /// The origin names a GitHub repository — auto-managed clone, unchanged.
    ManagedClone(&'a str),
    /// There is no origin remote — run the session in this checkout (#6276).
    LiveCheckout,
    /// The origin names a host other than GitHub — refuse, unchanged.
    RefuseNonGitHub(&'a str),
}

/// Decide what to do about `origin_url`.
///
/// Why: see the module doc — the `None` case is the one #6276 changed, and it
/// is the only one that changed.
/// What: `None` (no `remote.origin.url` configured) is
/// [`OriginPlan::LiveCheckout`]. Any `Some` remote is classified by
/// [`super::guided::is_github_remote`], exactly as `fallback_protected` did
/// inline before this module existed.
/// Test: see [`OriginPlan`].
pub(crate) fn plan_for_origin(origin_url: Option<&str>) -> OriginPlan<'_> {
    match origin_url {
        // #6276: a local-only repository proceeds instead of being refused.
        None => OriginPlan::LiveCheckout,
        Some(url) if super::guided::is_github_remote(url) => OriginPlan::ManagedClone(url),
        Some(url) => OriginPlan::RefuseNonGitHub(url),
    }
}

/// The notice printed when a local-only repository starts its session.
///
/// Why: the no-origin case used to carry real information — that there is no
/// remote, and that no managed clone will be made. #6276 keeps that
/// information and drops the termination, so the operator still learns why
/// this session runs where it does.
/// What: a two-line notice naming `checkout`. The caller prints it with
/// `eprintln!` and then starts the session; it is never an error.
/// Test: `live_checkout_notice_names_the_checkout_and_does_not_read_as_a_refusal`.
pub(crate) fn live_checkout_notice(checkout: &Path) -> String {
    format!(
        "tm: no git origin remote in '{}' — this is a local-only repository.\n\
         tm: starting the session in this checkout; no managed clone is made.",
        checkout.display()
    )
}

#[cfg(test)]
#[path = "origin_plan_tests.rs"]
mod tests;
