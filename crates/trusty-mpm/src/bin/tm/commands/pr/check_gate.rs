//! The checks gate `tm pr merge` applies before it calls `gh pr merge` (#8614).
//!
//! Why: a version-control agent merged a PR whose failing checks it had itself
//! found branch-caused, because the base branch required none of them. Two
//! holes let that through: `tm pr merge` never read the checks, and GitHub's
//! auto-merge waits only on REQUIRED checks, so a non-required check still
//! running when `--auto` arms can fail afterwards and the PR merges anyway.
//! What: [`refusal`] refuses while any check has settled non-passing, and,
//! under `--auto`, while any check auto-merge would not wait for is still
//! running. `--allow-failing <check>` waives one named check the PM has
//! judged, and never a required one. The required list comes from the base
//! branch's `.protection` on `repos/{o}/{r}/branches/{b}` ([`read_required`]),
//! which answers for an unprotected branch too — the protection endpoint
//! returns 404 there.
//! Test: `check_gate_tests.rs`.

use serde::Deserialize;

use super::rollup::{RollupEntry, deciding_per_check};
use super::{GhRunner, argv};

/// `GET repos/{o}/{r}/branches/{b}`, reduced to the protection it reports.
#[derive(Debug, Deserialize)]
struct BranchView {
    /// Absent when the payload does not carry it — the required list is then
    /// unknown, which waives nothing.
    #[serde(default)]
    protection: Option<Protection>,
}

/// The `protection` object of a branch.
#[derive(Debug, Deserialize)]
struct Protection {
    /// `false` on an unprotected branch.
    #[serde(default)]
    enabled: bool,
    /// The required status checks, when protection lists any.
    #[serde(default)]
    required_status_checks: Option<RequiredChecks>,
}

/// `protection.required_status_checks`.
#[derive(Debug, Deserialize)]
struct RequiredChecks {
    /// `off`, `non_admins` or `everyone`.
    #[serde(default)]
    enforcement_level: Option<String>,
    /// Required check names, legacy spelling.
    #[serde(default)]
    contexts: Vec<String>,
    /// Required checks, each with its name in `context`.
    #[serde(default)]
    checks: Vec<RequiredCheck>,
}

/// One entry of `required_status_checks.checks`.
#[derive(Debug, Deserialize)]
struct RequiredCheck {
    /// The check name.
    #[serde(default)]
    context: String,
}

/// The required check names a branch payload declares.
///
/// Why: an unprotected branch, and protection with enforcement `off`, require
/// nothing — so auto-merge waits on nothing and every running check counts.
/// What: `Some(names)` — `contexts` and `checks[].context`, deduplicated —
/// when protection is enabled and enforced; `Some(empty)` when it is not;
/// `None` when the payload carries no `protection` at all.
///
/// # Errors
///
/// The payload is not JSON.
///
/// Test: `branch_payload_lists_the_enforced_checks`,
/// `an_unprotected_branch_requires_nothing`,
/// `a_payload_without_protection_is_unknown`.
pub(crate) fn parse_required(json: &str) -> anyhow::Result<Option<Vec<String>>> {
    let view: BranchView = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("cannot parse the branch payload: {e}"))?;
    let Some(protection) = view.protection else {
        return Ok(None);
    };
    let Some(rsc) = protection.required_status_checks.filter(|r| {
        protection.enabled
            && !r
                .enforcement_level
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case("off"))
    }) else {
        return Ok(Some(Vec::new()));
    };
    let mut names: Vec<String> = Vec::new();
    for name in rsc
        .contexts
        .into_iter()
        .chain(rsc.checks.into_iter().map(|c| c.context))
    {
        let name = name.trim().to_string();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(Some(names))
}

/// Read the required check names on `base` from the branch endpoint.
///
/// # Errors
///
/// `gh api` fails (an unknown branch or repo) or its payload is not JSON.
///
/// Test: `run_auto_arms_when_only_required_checks_run`.
pub(crate) fn read_required<R: GhRunner>(
    gh: &R,
    slug: &str,
    base: &str,
) -> anyhow::Result<Option<Vec<String>>> {
    let a = argv(&["api", &format!("repos/{slug}/branches/{base}")]);
    parse_required(&gh.run(&a)?.stdout_ok(&a)?)
}

/// What the gate judges the checks against.
#[derive(Debug, Default)]
pub(crate) struct CheckGate<'a> {
    /// `--auto`: the merge arms now and fires later.
    pub(crate) auto: bool,
    /// `--allow-failing` check names, matched exactly.
    pub(crate) allow_failing: &'a [String],
    /// The base branch's required check names; `None` when unknown.
    pub(crate) required: Option<&'a [String]>,
}

impl CheckGate<'_> {
    /// Does the required list name `e`? `false` when the list is unknown.
    fn is_required(&self, e: &RollupEntry) -> bool {
        match (e.label(), self.required) {
            (Some(name), Some(req)) => req.iter().any(|r| r == name),
            _ => false,
        }
    }

    /// Does `--allow-failing` waive `e`? Named exactly, and provably not
    /// required — an unknown required list waives nothing.
    fn waives(&self, e: &RollupEntry) -> bool {
        let named = e
            .label()
            .is_some_and(|n| self.allow_failing.iter().any(|a| a.trim() == n));
        named && self.required.is_some() && !self.is_required(e)
    }
}

/// Does the gate's answer depend on the required list?
///
/// Why: the read is one more `gh` call, so it runs only when a running check
/// meets `--auto` or a failing check meets `--allow-failing`.
/// What: exactly those two conditions. A wrong `false` fails closed: an
/// unknown list waives nothing and lets no running check through.
/// Test: `run_auto_arms_when_only_required_checks_run`,
/// `run_without_running_or_waived_checks_reads_no_protection`.
pub(crate) fn needs_required(rollup: &[RollupEntry], auto: bool, allow_failing: &[String]) -> bool {
    let checks = deciding_per_check(rollup);
    (auto && checks.iter().any(|e| e.is_unfinished()))
        || (!allow_failing.is_empty() && checks.iter().any(|e| e.failed()))
}

/// The one-line refusal for these checks, or `None` when they let the merge
/// through.
///
/// What: refuses while a deciding run has failed and is not waived; then,
/// under `--auto`, while a deciding run is unfinished and is neither required
/// (auto-merge waits for those) nor waived.
/// Test: `a_failing_check_refuses_with_or_without_auto`,
/// `allow_failing_waives_only_a_named_non_required_check`,
/// `auto_refuses_while_a_non_required_check_runs`.
pub(crate) fn refusal(rollup: &[RollupEntry], gate: &CheckGate<'_>, pr: u64) -> Option<String> {
    let checks = deciding_per_check(rollup);
    let failing: Vec<String> = checks
        .iter()
        .filter(|e| e.failed() && !gate.waives(e))
        .map(|e| e.display_label())
        .collect();
    if !failing.is_empty() {
        return Some(format!(
            "{} check(s) failing: {} — a failing check blocks the merge whatever branch \
             protection requires (#8614); fix the branch, or pass `--allow-failing <check>` \
             for a failure the PM has waived (never a required check)",
            failing.len(),
            failing.join(", ")
        ));
    }
    if !gate.auto {
        return None;
    }
    let running: Vec<String> = checks
        .iter()
        .filter(|e| e.is_unfinished() && !gate.is_required(e) && !gate.waives(e))
        .map(|e| e.display_label())
        .collect();
    (!running.is_empty()).then(|| {
        format!(
            "{} check(s) still running that auto-merge would not wait for: {} — it waits only \
             on required checks, so a later failure would still merge (#8614); re-run \
             `tm pr merge {pr} --auto` once they settle",
            running.len(),
            running.join(", ")
        )
    })
}

/// The checks `--allow-failing` waives on this merge, failing or running.
///
/// Why: a waived check is a merge decision a reviewer must be able to see
/// afterwards, so [`commit_body`] records it in the landing commit.
/// Test: `allow_failing_waives_only_a_named_non_required_check`.
pub(crate) fn waived(rollup: &[RollupEntry], gate: &CheckGate<'_>) -> Vec<String> {
    deciding_per_check(rollup)
        .into_iter()
        .filter(|e| (e.failed() || (gate.auto && e.is_unfinished())) && gate.waives(e))
        .map(RollupEntry::display_label)
        .collect()
}

/// The squash commit body: `body`, headed by one line naming every waived
/// check when there is one.
/// Test: `a_waiver_heads_the_commit_body`.
pub(crate) fn commit_body(body: &str, waived: &[String]) -> String {
    if waived.is_empty() {
        return body.to_string();
    }
    format!(
        "Merged over waived check(s) via `tm pr merge --allow-failing` (#8614): {}\n\n{body}",
        waived.join(", ")
    )
}

#[cfg(test)]
#[path = "check_gate_tests.rs"]
mod tests;
