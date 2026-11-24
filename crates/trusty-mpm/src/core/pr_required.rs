//! The required check names on a base branch: branch protection plus
//! rulesets (#8614).
//!
//! Why: #11673 — the daemon's PR watch judges only the required contexts, and
//! it must read them exactly as `tm pr merge`'s checks gate does. The parsers
//! moved here from the `tm` binary so both callers share one reading.
//! What: [`parse_required`] reads `GET repos/{o}/{r}/branches/{b}`,
//! [`parse_ruleset_required`] reads `GET repos/{o}/{r}/rules/branches/{b}`,
//! and [`combine`] merges the two. The `gh` calls stay with each caller.
//! Test: `check_gate_tests.rs` (the `tm` binary),
//! `required_combines_protection_and_rulesets`.

use serde::Deserialize;

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
pub fn parse_required(json: &str) -> anyhow::Result<Option<Vec<String>>> {
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
pub fn parse_ruleset_required(json: &str) -> anyhow::Result<Vec<String>> {
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

/// Merge the branch list with the ruleset list.
///
/// What: an unknown protection list stays unknown (`None`) whatever the
/// rulesets add, since an unknown list waives nothing; otherwise the ruleset
/// names are appended, deduplicated.
/// Test: `required_combines_protection_and_rulesets`.
pub fn combine(branch: Option<Vec<String>>, ruleset: &[String]) -> Option<Vec<String>> {
    branch.map(|mut names| {
        for name in ruleset {
            push_unique(&mut names, name);
        }
        names
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_combines_protection_and_rulesets() {
        let branch = parse_required(
            r#"{"protection":{"enabled":true,"required_status_checks":{"contexts":["a"],"checks":[{"context":"b"}]}}}"#,
        )
        .expect("branch payload parses");
        let rules = parse_ruleset_required(
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"b"},{"context":"c"}]}}]"#,
        )
        .expect("rules payload parses");
        assert_eq!(
            combine(branch, &rules),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
        assert_eq!(
            combine(None, &rules),
            None,
            "unknown protection stays unknown"
        );
    }
}
