//! `tm doctor` legacy-instruction-source probe (issue #2876).
//!
//! Why: leftover GLOBAL instruction sources (`~/.claude/skills/tm-*` copies
//! from the pre-project-local deploy model, and the legacy
//! `~/.trusty-mpm/claude-config` managed-config dir) shadow the current
//! framework and keep serving stale guidance — one of the two silent drift
//! modes behind the PR #2825 attribution bypass. Neither is caught by the
//! narrower agents/skills/deployment probes.
//!
//! The OTHER drift mode — deployed skill content diverging from the installed
//! binary's assets — used to live here too. Issue #4604 moved it to
//! [`super::doctor_skill_drift`] when its reference point changed from the
//! `~/.trusty-mpm/framework/skills` extraction cache (which could itself lag
//! the binary, making every skill it covered report clean) to the binary's own
//! embedded assets.
//!
//! What: [`check_legacy_instruction_sources`] `Warn`s when legacy global
//! instruction sources exist — advisory-only, since a leftover directory
//! shadows rather than silently drops a convention. It never blocks session
//! start and never writes.
//! Test: the `tests` module below covers the present/absent branches.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};

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
///
/// #7102: the remediation it prints must be one that actually clears it. It
/// used to offer `tm install`, whose skill step wrote this same directory, so
/// following the advice reproduced the warning; the installer now deploys
/// bundled skills to the managed tier only
/// ([`crate::core::skill_install_tiers::deploy_install_skill_tiers`]) and the
/// text says to delete the copies.
/// Test: `legacy_sources_ok_when_absent`, `legacy_sources_warns_on_tm_skills`,
/// `legacy_sources_warns_on_claude_config`,
/// `legacy_sources_ok_after_the_install_deploy`.
pub(super) fn check_legacy_instruction_sources(home: &Path) -> DoctorCheck {
    let mut findings: Vec<String> = Vec::new();

    let tm_skill_count = count_legacy_tm_skills(&home.join(".claude").join("skills"));
    if tm_skill_count > 0 {
        // #7102: the old text said "remove them or run `tm install` to refresh",
        // and `tm install` wrote this very directory — so the remediation
        // reproduced the finding. Removal is now the whole fix.
        findings.push(format!(
            "{tm_skill_count} `tm-*` skill copy(ies) in ~/.claude/skills (legacy global \
             deploy — delete them; bundled skills deploy only to the tm-managed \
             CLAUDE_CONFIG_DIR since #6586, and `tm install` no longer writes them here)"
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
        let skill_dir = skills.join("tm-workflow");
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

    #[test]
    fn legacy_sources_ok_after_the_install_deploy() {
        // #7102: `tm install`'s skill step wrote every bundled `tm-*` skill into
        // `~/.claude/skills`, the directory this check flags — so the check's
        // own remediation ("run `tm install`") put the flagged copies back and
        // the warning never cleared. Drive the installer's real deploy against a
        // temp home, then read this check's verdict on that same home: the two
        // must agree.
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::core::paths::FrameworkPaths::under(tmp.path());
        let source = paths.skill_source_dir();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("tm-workflow.md"), "bundled body").unwrap();

        crate::core::skill_install_tiers::deploy_install_skill_tiers(&paths).unwrap();

        let check = check_legacy_instruction_sources(tmp.path());
        assert_eq!(
            check.status,
            CheckStatus::Ok,
            "the installer's own deploy target must not trip legacy_sources: {}",
            check.message
        );
        // …and the deploy must actually have happened, or the assertion above
        // passes vacuously on a no-op.
        assert!(
            paths
                .skill_deploy_dir()
                .join("tm-workflow")
                .join("SKILL.md")
                .is_file(),
            "the bundled skill must be on disk in the managed tier"
        );
    }
}
