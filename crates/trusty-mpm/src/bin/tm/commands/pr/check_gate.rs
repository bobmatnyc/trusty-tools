//! The checks gate `tm pr merge` applies before it calls `gh pr merge` (#8614).
//!
//! Why: a version-control agent merged a PR whose failing checks it had itself
//! found branch-caused, because the base branch required none of them. GitHub
//! holds a merge only on REQUIRED checks: a direct `gh pr merge` lands an
//! UNSTABLE PR at once, and `--auto` fires as soon as the required ones pass.
//! Either way a non-required check that is still running can fail after the
//! merge. The gate blocks the merge whatever branch protection declares.
//! What: [`refusal`] refuses, with or without `--auto`, while any check has
//! settled non-passing, while no check has registered and none is required,
//! and while a check GitHub would not wait for is still running.
//! `--allow-failing <check>` waives one named check the PM has judged, and
//! never a required one; `--allow-no-checks` lets a repo with no CI merge.
//! The required list ([`read_required`]) is the base branch's `.protection`
//! on `repos/{o}/{r}/branches/{b}` — which answers for an unprotected branch
//! too, where the protection endpoint returns 404 — plus every
//! `required_status_checks` rule on `repos/{o}/{r}/rules/branches/{b}`.
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

/// One required check, as branch protection and rulesets both shape it.
#[derive(Debug, Deserialize)]
struct RequiredCheck {
    /// The check name.
    #[serde(default)]
    context: String,
}

/// One active rule `GET repos/{o}/{r}/rules/branches/{b}` returns.
#[derive(Debug, Deserialize)]
struct Rule {
    /// The rule type; only `required_status_checks` names checks.
    #[serde(default, rename = "type")]
    kind: String,
    /// The rule's parameters, absent for parameterless rules.
    #[serde(default)]
    parameters: Option<RuleParameters>,
}

/// The parameters of a `required_status_checks` rule.
#[derive(Debug, Deserialize)]
struct RuleParameters {
    /// The checks the rule requires.
    #[serde(default)]
    required_status_checks: Vec<RequiredCheck>,
}

/// Append the trimmed `name` to `names` unless it is empty or already there.
fn push_unique(names: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if !name.is_empty() && !names.iter().any(|n| n == name) {
        names.push(name.to_string());
    }
}

/// The required check names a branch payload declares.
///
/// Why: an unprotected branch, and protection with enforcement `off`, require
/// nothing — so GitHub holds the merge on nothing and every running check
/// counts.
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
        .iter()
        .chain(rsc.checks.iter().map(|c| &c.context))
    {
        push_unique(&mut names, name);
    }
    Ok(Some(names))
}

/// The check names the active rulesets on a branch require.
///
/// Why: a repository ruleset can require a check that branch protection does
/// not list, and `--allow-failing` must never waive it.
/// What: every `required_status_checks` rule's
/// `parameters.required_status_checks[].context`, deduplicated. The payload
/// may be several JSON arrays back to back — `gh api --paginate` prints one
/// per page.
///
/// # Errors
///
/// The payload is empty or not a sequence of JSON arrays of rules.
///
/// Test: `ruleset_payload_lists_the_required_checks`.
pub(crate) fn parse_ruleset_required(json: &str) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    let mut pages = 0usize;
    for page in serde_json::Deserializer::from_str(json).into_iter::<Vec<Rule>>() {
        let page = page.map_err(|e| anyhow::anyhow!("cannot parse the rules payload: {e}"))?;
        pages += 1;
        for rule in page.iter().filter(|r| r.kind == "required_status_checks") {
            for check in rule
                .parameters
                .iter()
                .flat_map(|p| &p.required_status_checks)
            {
                push_unique(&mut names, &check.context);
            }
        }
    }
    anyhow::ensure!(pages > 0, "the rules payload is empty");
    Ok(names)
}

/// Read the required check names on `base`: branch protection plus rulesets.
///
/// What: an unknown protection list stays unknown (`None`) whatever the
/// rulesets add, since an unknown list waives nothing.
///
/// # Errors
///
/// Either `gh api` read fails (an unknown branch or repo, no access) or its
/// payload does not parse — the merge is then refused rather than judged on
/// half the requirements.
///
/// Test: `run_auto_arms_when_only_required_checks_run`,
/// `run_never_waives_a_ruleset_required_check`,
/// `run_refuses_when_the_ruleset_read_fails`,
/// `run_refuses_when_the_branch_read_fails`.
pub(crate) fn read_required<R: GhRunner>(
    gh: &R,
    slug: &str,
    base: &str,
) -> anyhow::Result<Option<Vec<String>>> {
    let a = argv(&["api", &format!("repos/{slug}/branches/{base}")]);
    let branch = parse_required(&gh.run(&a)?.stdout_ok(&a)?)?;
    let r = argv(&[
        "api",
        "--paginate",
        &format!("repos/{slug}/rules/branches/{base}"),
    ]);
    let ruleset = parse_ruleset_required(&gh.run(&r)?.stdout_ok(&r)?)?;
    Ok(branch.map(|mut names| {
        for name in &ruleset {
            push_unique(&mut names, name);
        }
        names
    }))
}

/// What the gate judges the checks against.
#[derive(Debug, Default)]
pub(crate) struct CheckGate<'a> {
    /// `--auto`: the merge arms now and fires later. Named in the re-run hint.
    pub(crate) auto: bool,
    /// `--allow-failing` check names, matched exactly.
    pub(crate) allow_failing: &'a [String],
    /// `--allow-no-checks`: a PR with no registered check may merge.
    pub(crate) allow_no_checks: bool,
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
/// Why: the reads are two more `gh` calls, so they run only when a check is
/// running, a failing check meets `--allow-failing`, or no check has
/// registered and `--allow-no-checks` was not passed.
/// What: exactly those three conditions. A wrong `false` fails closed: an
/// unknown list waives nothing, lets no running check through, and lets no
/// PR without checks through.
/// Test: `run_auto_arms_when_only_required_checks_run`,
/// `run_without_running_or_waived_checks_reads_no_protection`,
/// `run_allow_no_checks_merges_a_pr_without_checks`.
pub(crate) fn needs_required(
    rollup: &[RollupEntry],
    allow_failing: &[String],
    allow_no_checks: bool,
) -> bool {
    let checks = deciding_per_check(rollup);
    (checks.is_empty() && !allow_no_checks)
        || checks.iter().any(|e| e.is_unfinished())
        || (!allow_failing.is_empty() && checks.iter().any(|e| e.failed()))
}

/// The one-line refusal for these checks, or `None` when they let the merge
/// through.
///
/// What: with or without `--auto`, refuses while a deciding run has failed
/// and is not waived; then while no check has registered, unless
/// `--allow-no-checks` was passed or `--auto` arms with a required check
/// (auto-merge waits for it); then while a deciding run is unfinished and is
/// not waived. Only under `--auto` is a running required check exempt:
/// auto-merge waits for it. A direct merge never defers to GitHub, whose
/// client-side BLOCKED check misses UNKNOWN right after a push and is
/// bypassed by an admin merge (#8614: blocks whatever branch protection says).
/// Test: `a_failing_check_refuses_with_or_without_auto`,
/// `allow_failing_waives_only_a_named_non_required_check`,
/// `auto_refuses_while_a_non_required_check_runs`,
/// `no_registered_check_refuses_unless_one_is_required_or_allowed`,
/// `run_refuses_a_running_required_check_without_auto`,
/// `run_refuses_a_checkless_pr_with_required_checks_without_auto`.
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
    let rerun = format!("tm pr merge {pr}{}", if gate.auto { " --auto" } else { "" });
    if checks.is_empty() {
        let none_required = gate.required.is_none_or(<[String]>::is_empty);
        return (!gate.allow_no_checks && (!gate.auto || none_required)).then(|| {
            format!(
                "no checks have registered yet — a merge now would land unchecked, and only \
                 `--auto` can wait for a required check (#8614); re-run `{rerun}` once CI \
                 registers, or pass `--allow-no-checks` for a repo with no CI"
            )
        });
    }
    let running: Vec<String> = checks
        .iter()
        .filter(|e| e.is_unfinished() && !(gate.auto && gate.is_required(e)) && !gate.waives(e))
        .map(|e| e.display_label())
        .collect();
    (!running.is_empty()).then(|| {
        format!(
            "{} check(s) still running that the merge would not wait for: {} — a later \
             failure would still merge (#8614); re-run `{rerun}` once they settle{}",
            running.len(),
            running.join(", "),
            if gate.auto {
                ""
            } else {
                ", or pass `--auto` to wait on required checks"
            }
        )
    })
}

/// The checks `--allow-failing` waives on this merge, failing or running.
///
/// Why: a waived check is a merge decision a reviewer must be able to see
/// afterwards, so [`commit_body`] records it in the landing commit.
/// Test: `allow_failing_waives_only_a_named_non_required_check`,
/// `run_allow_failing_merges_over_a_waived_check`.
pub(crate) fn waived(rollup: &[RollupEntry], gate: &CheckGate<'_>) -> Vec<String> {
    deciding_per_check(rollup)
        .into_iter()
        .filter(|e| (e.failed() || e.is_unfinished()) && gate.waives(e))
        .map(|e| {
            let state = if e.failed() {
                "failed"
            } else {
                "still running"
            };
            format!("{} ({state})", e.display_label())
        })
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
