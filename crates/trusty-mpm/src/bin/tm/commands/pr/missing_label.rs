//! `gh pr create` against a repository that lacks one of its labels (#8431).
//!
//! Why: `tm pr open` always passes `--label trusty-mpm`, and `gh pr create`
//! refuses the whole create with `could not add label: 'trusty-mpm' not found`
//! in a repository that never had that label — `duettoresearch/jev-matching`
//! was one. The `ws/<session>` label is seeded before the create (#7513); the
//! convention label is not, because seeding it with `--force` would restyle a
//! label a project already owns.
//! What: [`create`] runs the create and, ONLY on a missing-label refusal
//! naming the convention label, creates it without `--force` (a label that
//! does not exist has no styling to overwrite) and retries; when it cannot be
//! created, that one `--label` is dropped and the create retried with a
//! warning. A missing-label refusal naming any other label — including
//! `ws/<session>`, seeded before create per #7513 — is returned untouched,
//! same as every other failure, so `tm pr open` still fails loudly on it.
//! Test: `pr_8431_a_missing_convention_label_is_created_and_the_create_retried`,
//! `pr_8431_a_label_that_cannot_be_created_is_dropped_with_a_warning`,
//! `pr_8431_other_create_failures_still_fail`,
//! `pr_8431_a_missing_workstream_label_still_fails_loudly`.

use trusty_mpm::core::policy_labels;

use super::GhRun;
use super::GhRunner;
use super::open::{OpenPlan, created_pr_url};

/// The label `gh pr create` named as missing, when that is why it failed.
///
/// What: the name inside `could not add label: '<name>' not found`.
/// Test: `pr_8431_a_missing_convention_label_is_created_and_the_create_retried`.
pub(crate) fn missing_label(stderr: &str) -> Option<&str> {
    let rest = stderr.split("could not add label: '").nth(1)?;
    let (name, tail) = rest.split_once('\'')?;
    tail.trim_start()
        .starts_with("not found")
        .then_some(name)
        .filter(|n| !n.is_empty())
}

/// Run the plan's `gh pr create`, recovering from a missing label only.
///
/// Why: see the module doc — a missing label must not cost the PR, and no
/// other failure may be reported as success.
/// What: returns the final create run and the labels dropped from it. A label
/// is recovered only when the refusal names one the plan itself applies, and
/// each is recovered at most once, so the loop is bounded by the label count.
/// Test: see the module doc.
pub(crate) fn create<R: GhRunner>(
    gh: &R,
    plan: &OpenPlan,
    repo: Option<&str>,
) -> anyhow::Result<(GhRun, Vec<String>)> {
    let mut argv = plan.argv.clone();
    let mut pending = plan.create_labels();
    let mut dropped = Vec::new();
    loop {
        let out = gh.run(&argv)?;
        if out.success || created_pr_url(&out.stdout).is_some() {
            return Ok((out, dropped));
        }
        // #8431: only "label not found" is recoverable; anything else returns.
        let Some(name) = missing_label(&out.stderr).map(str::to_owned) else {
            return Ok((out, dropped));
        };
        // #8431 review follow-up: recovery (create-then-retry, or
        // drop-and-retry) covers ONLY the convention label. Any other
        // missing label — including `ws/<session>`, seeded before create per
        // #7513 — still fails the create as before; #7513's "fail loudly"
        // guarantee depends on that.
        if name != policy_labels::CONVENTION_LABEL {
            return Ok((out, dropped));
        }
        let Some(at) = pending.iter().position(|l| *l == name) else {
            return Ok((out, dropped));
        };
        pending.remove(at);
        if seed_convention_label(gh, repo) {
            continue;
        }
        eprintln!(
            "  warning: this repository has no `{name}` label and it could not be created; \
             opening the PR without it"
        );
        remove_label(&mut argv, &name);
        dropped.push(name);
    }
}

/// Create the convention label, without `--force`; true when it now exists.
fn seed_convention_label<R: GhRunner>(gh: &R, repo: Option<&str>) -> bool {
    let argv = policy_labels::create_label_argv(&policy_labels::convention_label(), repo, false);
    match gh.run(&argv) {
        Ok(run) if run.success => {
            eprintln!(
                "  note: created the missing `{}` label",
                policy_labels::CONVENTION_LABEL
            );
            true
        }
        Ok(run) => {
            eprintln!(
                "  warning: could not create the `{}` label: {}",
                policy_labels::CONVENTION_LABEL,
                run.stderr.trim()
            );
            false
        }
        Err(e) => {
            eprintln!(
                "  warning: could not create the `{}` label: {e:#}",
                policy_labels::CONVENTION_LABEL
            );
            false
        }
    }
}

/// Remove the `--label <name>` pair from a `gh pr create` argv.
fn remove_label(argv: &mut Vec<String>, name: &str) {
    if let Some(i) = argv
        .windows(2)
        .position(|w| w[0] == "--label" && w[1] == name)
    {
        argv.drain(i..i + 2);
    }
}
