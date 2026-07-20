//! `tm doctor` staleness and legacy-instruction-source probes (issue #2876).
//!
//! Why: trusty-mpm's per-project conventions (notably the commit/PR attribution
//! footer) ship in bundled skills and framework instructions. Two silent drift
//! modes let stale text keep being served — the exact failure behind the PR
//! #2825 attribution bypass: (1) a workspace whose deployed skills differ from
//! the installed binary's bundled assets, and (2) leftover GLOBAL instruction
//! sources (`~/.claude/skills/tm-*` copies from the pre-project-local deploy
//! model, and the legacy `~/.trusty-mpm/claude-config` managed-config dir) that
//! shadow the current framework. Neither is caught by the narrower
//! agents/skills/deployment probes. Split into its own file (mirroring the
//! existing `doctor_output_style.rs` / `doctor_fs_checks.rs` split) to keep
//! `doctor.rs` under the 500-SLOC production cap.
//! What: [`check_skill_staleness`] diffs the deployed skill manifest against the
//! current bundled source via [`crate::core::skill_staleness::stale_skills`].
//! Ordinary drift is advisory (`Warn`); drift in a
//! [`CONVENTIONS_BEARING_SKILLS`] entry is escalated to `Fail` because that
//! specific skill is the sole carrier of a framework-guaranteed rule and a
//! quiet warning is exactly what let the PR #2825 bypass through unnoticed.
//! [`check_legacy_instruction_sources`] `Warn`s when legacy global instruction
//! sources exist — that probe stays advisory-only, since a leftover directory
//! shadows rather than silently drops a convention. Neither probe blocks
//! session start; both are read-only signals surfaced through `tm doctor`.
//! Test: the `tests` module below covers the fresh/stale, ordinary/
//! conventions-bearing, and present/absent branches of both probes.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_staleness::stale_skills;

/// Bundled skills whose drift can silently drop a framework-guaranteed rule.
///
/// Why: [`check_skill_staleness`] treats all drift as advisory (`Warn`) by
/// design — a user-modified skill is intentionally never overwritten (see
/// `trusty_agents_common::skills::deployer`'s "checksum differs → user-modified
/// → skip" rule), and most bundled skills are just good-practice guidance
/// where staleness is low-stakes. But a small subset of skills are the sole
/// place a *framework convention* — a rule other code or process depends on
/// being followed exactly, like the commit/PR attribution footer — is spelled
/// out. A user edit to one of THESE that silently drops or alters the
/// convention text reproduces the exact PR #2825 attribution bypass this
/// probe exists to catch. A skill belongs on this list only if: (1) it states
/// a framework-guaranteed rule as executable instruction, not merely a
/// cross-reference to another skill, and (2) an edited/stale copy could cause
/// that rule to lapse without any other signal catching it. Kept intentionally
/// short — sweeping in ordinary best-practice skills would make `Fail` noise
/// that trains operators to ignore it.
///
/// What: the list, with each entry's rule:
/// - `documentation-style` — the mandatory Why/What/Test doc-comment triple
///   that root `CLAUDE.md` requires verbatim on every public item.
/// - `tm-pr-workflow` — the canonical source of the attribution-footer text,
///   the `--assignee @me --label trusty-mpm` defaults, and the
///   squash-merge-only policy; this is the skill PR #2825 itself involved.
/// - `tm-git-file-tracking` — CB#4's mandatory blocking file-tracking gate,
///   plus the only other skill that reproduces the attribution footer inline
///   in worked commit-message examples the PM copies literally.
///
/// Test: `staleness_escalates_when_conventions_skill_stale`,
/// `staleness_warns_when_ordinary_skill_stale`.
const CONVENTIONS_BEARING_SKILLS: &[&str] = &[
    "documentation-style",
    "tm-pr-workflow",
    "tm-git-file-tracking",
];

/// Probe whether the deployed skills are stale relative to bundled assets.
///
/// Why: a long-lived worktree (or any workspace not re-provisioned since the
/// bundled assets changed) keeps serving outdated skill text until the next
/// deploy self-heals it — the drift that let the PR #2825 attribution bypass
/// persist. Surfacing it tells the operator a relaunch / `tm install` is due
/// BEFORE a stale convention is acted on (issue #2876). Ordinary drift is
/// advisory (`Warn`); drift in a [`CONVENTIONS_BEARING_SKILLS`] entry is
/// escalated to `Fail` because that specific text is the sole carrier of a
/// framework-guaranteed rule — a quiet warning is exactly what let PR #2825
/// slip through.
/// What: compares `paths.skill_source_dir()` (the bundled source) against the
/// deploy manifest at `paths.claude_skills_dir()`. `Ok` when nothing is stale
/// (including the common case of nothing yet deployed, which is not
/// staleness). Otherwise `Fail` when any stale skill is conventions-bearing
/// (naming it explicitly and pointing at `tm install`), else `Warn` naming the
/// stale skills (capped for readability) as before. This only escalates the
/// doctor signal — it never blocks session start (issue #2876 scope: the
/// launch path is untouched).
/// Test: `staleness_ok_when_fresh`, `staleness_warns_when_stale`,
/// `staleness_escalates_when_conventions_skill_stale`,
/// `staleness_warns_when_ordinary_skill_stale`.
pub(super) fn check_skill_staleness(paths: &FrameworkPaths) -> DoctorCheck {
    let stale = stale_skills(&paths.skill_source_dir(), &paths.claude_skills_dir());
    if stale.is_empty() {
        return DoctorCheck::new(
            "skill_staleness",
            CheckStatus::Ok,
            "deployed skills match the installed binary's bundled assets",
        );
    }

    let drifted_conventions: Vec<&str> = stale
        .iter()
        .map(String::as_str)
        .filter(|stem| CONVENTIONS_BEARING_SKILLS.contains(stem))
        .collect();

    // Keep the message bounded regardless of how many skills drifted.
    let shown: Vec<&str> = stale.iter().take(5).map(String::as_str).collect();
    let suffix = if stale.len() > shown.len() {
        format!(", … (+{} more)", stale.len() - shown.len())
    } else {
        String::new()
    };

    if drifted_conventions.is_empty() {
        return DoctorCheck::new(
            "skill_staleness",
            CheckStatus::Warn,
            format!(
                "{} deployed skill(s) are stale relative to bundled assets ({}{}) — \
                 relaunch the session or run `tm install` to refresh",
                stale.len(),
                shown.join(", "),
                suffix
            ),
        );
    }

    DoctorCheck::new(
        "skill_staleness",
        CheckStatus::Fail,
        format!(
            "conventions-bearing skill(s) have drifted from bundled assets \
             ({}) — a framework-guaranteed convention (e.g. the commit/PR \
             attribution footer or Why/What/Test doc pattern) may have \
             silently lapsed in the deployed copy; run `tm install` now to \
             restore the bundled version. Other stale skill(s) ({}{}) are \
             lower-severity but should also be refreshed by the same command.",
            drifted_conventions.join(", "),
            shown.join(", "),
            suffix
        ),
    )
}

/// Probe for legacy GLOBAL instruction sources that can serve stale guidance.
///
/// Why: before managed sessions went project-local, skills deployed into the
/// global `~/.claude/skills/` and managed config lived at
/// `~/.trusty-mpm/claude-config`. Both are superseded (project-local
/// `.claude/skills/` and the tm-owned `~/.trusty-tools/…/claude-config`
/// respectively), but leftover copies keep being read by Claude Code and can
/// serve outdated conventions — e.g. an old attribution footer — to any session
/// (issue #2876). This probe makes those leftovers visible.
/// What: `Warn` (advisory — never `Fail`) when EITHER `~/.claude/skills/tm-*`
/// skill copies exist OR the legacy `~/.trusty-mpm/claude-config` directory
/// exists, naming what was found and the one-line remediation; `Ok` when
/// neither is present. `home` is the base to resolve both paths under (the real
/// home in production, a temp dir in tests).
/// Test: `legacy_sources_ok_when_absent`, `legacy_sources_warns_on_tm_skills`,
/// `legacy_sources_warns_on_claude_config`.
pub(super) fn check_legacy_instruction_sources(home: &Path) -> DoctorCheck {
    let mut findings: Vec<String> = Vec::new();

    let tm_skill_count = count_legacy_tm_skills(&home.join(".claude").join("skills"));
    if tm_skill_count > 0 {
        findings.push(format!(
            "{tm_skill_count} `tm-*` skill copy(ies) in ~/.claude/skills (legacy global \
             deploy — remove them or run `tm install` to refresh; project-local \
             .claude/skills is now authoritative)"
        ));
    }

    let legacy_config = home.join(".trusty-mpm").join("claude-config");
    if legacy_config.is_dir() {
        findings.push(
            "legacy ~/.trusty-mpm/claude-config directory (superseded by the tm-owned \
             ~/.trusty-tools config home — safe to delete once no session references it)"
                .to_string(),
        );
    }

    if findings.is_empty() {
        DoctorCheck::new(
            "legacy_sources",
            CheckStatus::Ok,
            "no legacy global instruction sources found",
        )
    } else {
        DoctorCheck::new(
            "legacy_sources",
            CheckStatus::Warn,
            format!(
                "legacy instruction source(s) may serve stale guidance: {}",
                findings.join("; ")
            ),
        )
    }
}

/// Count `tm-*` skill entries directly under a `~/.claude/skills` directory.
///
/// Why: [`check_legacy_instruction_sources`] flags leftover global trusty-mpm
/// skill copies; only entries named `tm-*` are trusty-mpm's, so an unrelated
/// user skill in the same directory is never miscounted.
/// What: returns the number of entries (directory `tm-*/SKILL.md` or flat
/// `tm-*.md`) whose name starts with `tm-`. A missing/unreadable directory is
/// `0`.
/// Test: covered by `legacy_sources_warns_on_tm_skills`,
/// `legacy_sources_ok_when_absent`.
fn count_legacy_tm_skills(skills_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("tm-"))
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::skill_deployer::deploy_skills;

    #[test]
    fn staleness_ok_when_fresh() {
        // Freshly deployed skills (source unchanged since deploy) are not stale.
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = FrameworkPaths::under(tmp.path());
        paths.trusty_mpm_root = None;
        let source = paths.skill_source_dir();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("tm-doctor.md"), "v1").unwrap();
        deploy_skills(&source, &paths.claude_skills_dir()).unwrap();

        let check = check_skill_staleness(&paths);
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
    }

    #[test]
    fn staleness_warns_when_stale() {
        // Mutating the bundled source after deploy must Warn and point at the fix.
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = FrameworkPaths::under(tmp.path());
        paths.trusty_mpm_root = None;
        let source = paths.skill_source_dir();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("tm-doctor.md"), "v1").unwrap();
        deploy_skills(&source, &paths.claude_skills_dir()).unwrap();
        // Framework upgrade changes the bundled asset.
        std::fs::write(source.join("tm-doctor.md"), "v2").unwrap();

        let check = check_skill_staleness(&paths);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("tm-doctor"));
        assert!(check.message.contains("tm install"));
    }

    #[test]
    fn staleness_escalates_when_conventions_skill_stale() {
        // Drift in a conventions-bearing skill (tm-pr-workflow carries the
        // attribution-footer convention — the exact skill PR #2825 involved)
        // must escalate to Fail, name the skill, and point at the fix.
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = FrameworkPaths::under(tmp.path());
        paths.trusty_mpm_root = None;
        let source = paths.skill_source_dir();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("tm-pr-workflow.md"), "v1").unwrap();
        deploy_skills(&source, &paths.claude_skills_dir()).unwrap();
        // Framework upgrade changes the bundled asset; the deployed copy (user
        // never touched it) now silently drifts from the convention text.
        std::fs::write(source.join("tm-pr-workflow.md"), "v2").unwrap();

        let check = check_skill_staleness(&paths);
        assert_eq!(
            check.status,
            CheckStatus::Fail,
            "message: {}",
            check.message
        );
        assert!(check.message.contains("tm-pr-workflow"));
        assert!(check.message.contains("tm install"));
        assert!(check.message.to_lowercase().contains("convention"));
    }

    #[test]
    fn staleness_warns_when_ordinary_skill_stale() {
        // A conventions-bearing skill is ALSO deployed and unchanged in this
        // scenario — only the ordinary (non-allowlisted) skill drifted. The
        // escalation must not fire: status stays Warn, proving the Fail path
        // is scoped to CONVENTIONS_BEARING_SKILLS and does not leak onto every
        // check that happens to run alongside a conventions-bearing skill.
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = FrameworkPaths::under(tmp.path());
        paths.trusty_mpm_root = None;
        let source = paths.skill_source_dir();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("tm-doctor.md"), "v1").unwrap();
        std::fs::write(source.join("tm-pr-workflow.md"), "v1").unwrap();
        deploy_skills(&source, &paths.claude_skills_dir()).unwrap();
        // Only the ordinary skill's bundled source changes.
        std::fs::write(source.join("tm-doctor.md"), "v2").unwrap();

        let check = check_skill_staleness(&paths);
        assert_eq!(
            check.status,
            CheckStatus::Warn,
            "message: {}",
            check.message
        );
        assert!(check.message.contains("tm-doctor"));
    }

    #[test]
    fn legacy_sources_ok_when_absent() {
        // A clean home with no legacy global sources is Ok.
        let tmp = tempfile::tempdir().unwrap();
        let check = check_legacy_instruction_sources(tmp.path());
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn legacy_sources_warns_on_tm_skills() {
        // A leftover `~/.claude/skills/tm-*` copy must be flagged.
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join(".claude").join("skills");
        let skill_dir = skills.join("tm-pr-workflow");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "old attribution").unwrap();
        // A non-tm user skill must NOT trip the probe.
        std::fs::create_dir_all(skills.join("my-skill")).unwrap();

        let check = check_legacy_instruction_sources(tmp.path());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains(".claude/skills"));
        assert!(check.message.contains('1'));
    }

    #[test]
    fn legacy_sources_warns_on_claude_config() {
        // The legacy managed-config dir must be flagged.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".trusty-mpm").join("claude-config")).unwrap();

        let check = check_legacy_instruction_sources(tmp.path());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("claude-config"));
    }
}
