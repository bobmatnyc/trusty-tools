//! `tm doctor` legacy-instruction-source probe (issue #2876).
//!
//! Why: leftover GLOBAL instruction sources (bundled skill copies in
//! `~/.claude/skills` from the pre-project-local deploy model, and the legacy
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
/// What: `Warn` (advisory — never `Fail`) when EITHER `~/.claude/skills` holds
/// bundled skill copies OR the legacy `~/.trusty-mpm/claude-config` directory
/// exists, naming what was found and the one-line remediation; `Ok` when
/// neither is present. `home` is the base to resolve both paths under (the real
/// home in production, a temp dir in tests).
///
/// #7610: the `claude-config` finding names only that child as safe to
/// delete and calls out its live siblings under the same `~/.trusty-mpm`
/// parent (`usage/`, `session-manager/`, `statusline/`) — the parent itself
/// is never superseded, only the one child directory is.
///
/// #7102: the remediation it prints must be one that actually clears it. It
/// used to offer `tm install`, whose skill step wrote this same directory, so
/// following the advice reproduced the warning; the installer now deploys
/// bundled skills to the managed tier only
/// ([`crate::core::skill_install_tiers::deploy_install_skill_tiers`]) and the
/// text says to delete the copies. #7783 closed the last writer that refilled
/// it ([`crate::core::reinstall`]) and widened the count past the `tm-` prefix
/// — see [`count_legacy_bundled_skills`].
/// Test: `legacy_sources_ok_when_absent`, `legacy_sources_warns_on_tm_skills`,
/// `legacy_sources_counts_an_unprefixed_bundled_skill`,
/// `legacy_sources_warns_on_claude_config`,
/// `legacy_sources_ok_after_the_install_deploy`.
pub(super) fn check_legacy_instruction_sources(home: &Path) -> DoctorCheck {
    let mut findings: Vec<String> = Vec::new();

    let bundled_copies = count_legacy_bundled_skills(&home.join(".claude").join("skills"));
    if bundled_copies > 0 {
        // #7102: the old text said "remove them or run `tm install` to refresh",
        // and `tm install` wrote this very directory — so the remediation
        // reproduced the finding. Removal is now the whole fix.
        // #7783: the count covers every bundled stem, not just the `tm-*` ones,
        // so the wording may no longer promise a prefix.
        findings.push(format!(
            "{bundled_copies} bundled skill copy(ies) in ~/.claude/skills (legacy global \
             deploy — delete them; bundled skills deploy only to the tm-managed \
             CLAUDE_CONFIG_DIR since #6586, and neither `tm install` nor `tm reinstall` \
             writes them here)"
        ));
    }

    let legacy_config = home.join(".trusty-mpm").join("claude-config");
    if legacy_config.is_dir() {
        // #7610: name only the `claude-config` child as safe to delete — its
        // parent `~/.trusty-mpm` also holds `usage/` (savings ledger),
        // `session-manager/` (session store), and `statusline/`, all live and
        // read by `tm repair savings-ledger` / `tm statusline` today. A reader
        // acting on "safe to delete" against the parent directory would take
        // those out with it.
        findings.push(
            "legacy ~/.trusty-mpm/claude-config directory only (superseded by the tm-owned \
             ~/.trusty-tools config home — safe to delete; sibling directories \
             ~/.trusty-mpm/usage, ~/.trusty-mpm/session-manager, and ~/.trusty-mpm/statusline \
             are live and not covered by this advice — do not delete the ~/.trusty-mpm parent)"
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

/// Count leftover BUNDLED skill copies directly under a `~/.claude/skills`.
///
/// Why (#7783): this counted entries whose name starts with `tm-`, which is
/// only the prefixed third of what tm ships — a live `tm doctor` reported 23
/// copies beside ~33 unprefixed ones (`documentation-style`,
/// `writing-plans`, …) it could not see. The roster this binary ships is the
/// only test that stays correct as skills are added and renamed, and it is
/// also the exact set the deployers write, so the check and the deploy agree
/// by construction.
/// What: the number of entries whose name resolves to a stem in
/// [`crate::core::manifest::framework::bundled_skill_stems`] — the catalog the
/// framework manifest already validates the bundle against, so no second
/// derivation of "what tm ships" enters the codebase. Both on-disk forms
/// count: the `<stem>/` directory and the flat `<stem>.md`. A missing or
/// unreadable directory is `0`. An operator's OWN skill that happens to share
/// a bundled stem is counted too; the finding is advisory and the remediation
/// (delete the copy — the managed tier already serves it) is the same either
/// way.
/// Test: `legacy_sources_warns_on_tm_skills`,
/// `legacy_sources_counts_an_unprefixed_bundled_skill`,
/// `legacy_sources_ok_when_absent`.
fn count_legacy_bundled_skills(skills_dir: &Path) -> usize {
    let bundled = crate::core::manifest::framework::bundled_skill_stems();
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| bundled.contains(name.strip_suffix(".md").unwrap_or(name)))
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
    fn legacy_sources_counts_an_unprefixed_bundled_skill() {
        // #7783: the count filtered on a `tm-` prefix, so the bundled skills
        // that carry none — `documentation-style` here — were invisible to it
        // and a live `tm doctor` under-reported the leftovers by ~33 copies.
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join(".claude").join("skills");
        let bundled = skills.join("documentation-style");
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(bundled.join("SKILL.md"), "stale copy").unwrap();
        // An operator skill matching no bundled stem is still never counted.
        std::fs::create_dir_all(skills.join("my-own-skill")).unwrap();

        let check = check_legacy_instruction_sources(tmp.path());
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains("1 bundled skill copy(ies)"),
            "{}",
            check.message
        );
    }

    #[test]
    fn legacy_sources_warns_on_claude_config() {
        // The legacy managed-config dir must be flagged, scoped to the
        // `claude-config` child — never worded as though the `~/.trusty-mpm`
        // parent itself is safe to delete (#7610).
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".trusty-mpm").join("claude-config")).unwrap();

        let check = check_legacy_instruction_sources(tmp.path());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("claude-config"));
        // #7610: the message must name the live siblings under the same
        // parent so a reader does not delete the ledger or session store.
        assert!(check.message.contains("usage"), "{}", check.message);
        assert!(
            check.message.contains("session-manager"),
            "{}",
            check.message
        );
        assert!(check.message.contains("statusline"), "{}", check.message);
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
