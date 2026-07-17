//! Renders `references/doctor.md` from a maintained literal list of `tm
//! doctor` checks (source #5 of the issue #2913 design-research brief).
//!
//! Why: unlike the CLI tree, MCP catalog, and agent/skill rosters, doctor
//! checks have no single data table — each is a `DoctorCheck::new("literal",
//! ...)` call scattered across `run_doctor` and five sibling files
//! (`doctor_fs_checks.rs`, `doctor_deploy_validate.rs`,
//! `doctor_staleness.rs`, `doctor_output_style.rs`,
//! `doctor_agent_skills.rs`). The design-research brief's recommendation for
//! this "semi-extractable" surface is a maintained literal list cross-checked
//! by a count/name assertion test against `run_doctor`'s actual output,
//! rather than fragile literal-string grepping across six files.
//! What: [`DOCTOR_CHECKS`] — name + one-line description, in `run_doctor`'s
//! execution order — plus [`render`]. The drift guard lives in this
//! module's test suite: `doctor_checks_match_run_doctor_names` calls the real
//! `run_doctor` and asserts the name list is identical, so an added/removed/
//! renamed check fails the test suite (not just a docs staleness bug).
//! Test: `doctor_checks_match_run_doctor_names`,
//! `doctor_render_contains_known_check`.

use std::fmt::Write as _;

/// Every `tm doctor` check, in `run_doctor`'s execution order.
///
/// Why: `run_doctor`'s own doc comment (`crates/trusty-mpm/src/daemon/
/// doctor.rs`) is the closest thing to a spec for this list; this constant
/// mirrors it so the generator has a single, reviewable source instead of
/// re-deriving prose per release.
/// What: `(name, what it probes)` pairs. Kept in sync by
/// `doctor_checks_match_run_doctor_names`, which fails the test suite the
/// moment `run_doctor`'s actual check set diverges from this list.
/// Test: `doctor_checks_match_run_doctor_names`.
pub(crate) const DOCTOR_CHECKS: [(&str, &str); 17] = [
    (
        "instructions",
        "Framework instructions deployed and non-empty for the target project.",
    ),
    (
        "agents",
        "Bundled agent roster deployed under the operator/workspace `.claude/agents/` tier.",
    ),
    (
        "skills",
        "Bundled skill catalog deployed under the operator/workspace `.claude/skills/` tier.",
    ),
    (
        "skill_source",
        "The framework's own skill source directory is present and readable.",
    ),
    (
        "output_style",
        "The `trusty-mpm` Claude Code output style is configured and its file exists (DOC-28 F4).",
    ),
    (
        "deployment",
        "Full manifest-completeness diff of the deployed payload against the canonical bundled roster (issue #2158).",
    ),
    (
        "skill_staleness",
        "Deployed skill content matches the bundled/embedded source (issue #2876).",
    ),
    (
        "legacy_sources",
        "No legacy global instruction sources linger from a pre-migration install (issue #2876).",
    ),
    (
        "agent_skills",
        "Every agent's declared `skills:` frontmatter resolves to a real skill — dangling references fail (DOC-42, issue #2889).",
    ),
    (
        "agent_skills_prose_hints",
        "Informational: skill names mentioned in agent prose but not declared in `skills:` frontmatter (always `Ok`, issue #2906).",
    ),
    (
        "memory",
        "trusty-memory sidecar reachability probe (bounded by `PROBE_TIMEOUT`).",
    ),
    (
        "search",
        "trusty-search sidecar reachability + expected-index-present probe (bounded by `PROBE_TIMEOUT`).",
    ),
    (
        "worktrees",
        "No orphaned git worktrees under the managed workspace root (Fix 1b, #1840).",
    ),
    (
        "gh_account",
        "Active `gh` CLI identity is unambiguous — warns on multi-account ambiguity.",
    ),
    (
        "oauth_token",
        "Warns when a managed session risks the `CLAUDE_CONFIG_DIR`-keyed Keychain login loop (issue #2246).",
    ),
    (
        "hooks_contamination",
        "Warns when a project's `.claude/settings*.json` still carries tm hook entries from a pre-fix `tm install` — suggests `tm hooks clean` (issue #2940).",
    ),
    (
        "hooks_foreign_conflict",
        "Informational: warns when a project's `.claude/settings*.json` carries foreign (claude-mpm) hook entries that would fire inside a tm session — never auto-removed (issue #2940).",
    ),
];

/// Render the full doctor-check reference.
///
/// Why: an operator triaging a `tm doctor` `Warn`/`Fail` needs to know what
/// each check name actually probes without reading five source files.
/// What: one table row per [`DOCTOR_CHECKS`] entry, in `run_doctor`'s
/// execution order (not re-sorted — order communicates the probe sequence).
/// Test: `doctor_render_contains_known_check`.
pub(crate) fn render() -> String {
    let mut out = String::new();
    out.push_str("# Doctor Check Reference\n\n");
    out.push_str(
        "Generated from a maintained literal list cross-checked against \
         `run_doctor`'s actual check names (see this module's \
         `doctor_checks_match_run_doctor_names` test — an added, removed, or \
         renamed check fails the test suite). Source: \
         `crates/trusty-mpm/src/daemon/doctor.rs` and its five sibling \
         `doctor_*.rs` files. Regenerate with `tm generate capabilities`.\n\n",
    );
    let _ = writeln!(out, "{} checks, in execution order.\n", DOCTOR_CHECKS.len());

    out.push_str("| # | Check | What it probes |\n|---|---|---|\n");
    for (i, (name, description)) in DOCTOR_CHECKS.iter().enumerate() {
        let _ = writeln!(out, "| {} | `{name}` | {description} |", i + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::daemon::doctor::run_doctor;

    #[test]
    fn doctor_render_contains_known_check() {
        let rendered = render();
        assert!(rendered.contains("`instructions`"), "{rendered}");
        assert!(rendered.contains("`oauth_token`"), "{rendered}");
    }

    #[test]
    fn doctor_render_is_deterministic() {
        assert_eq!(render(), render());
    }

    /// The drift guard: `DOCTOR_CHECKS`'s name list must exactly match what
    /// `run_doctor` actually produces, in the same order. This is the
    /// "unit test asserting the generator's doctor list length == the
    /// doctor's actual check count" the issue #2913 brief requires — and
    /// goes further by also asserting name equality, not just length.
    #[tokio::test]
    async fn doctor_checks_match_run_doctor_names() {
        let report = run_doctor(None, None, &[]).await;
        let actual: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
        let expected: Vec<&str> = DOCTOR_CHECKS.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            actual, expected,
            "generate::doctor::DOCTOR_CHECKS has drifted from run_doctor's actual checks"
        );
    }
}
