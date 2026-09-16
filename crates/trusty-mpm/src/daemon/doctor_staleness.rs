//! `tm doctor` legacy-instruction-source probe (issue #2876).
//!
//! Why: leftover GLOBAL instruction sources — bundled skill copies in
//! `~/.claude/skills` from the pre-project-local deploy model — shadow the
//! current framework and keep serving stale guidance, one of the two silent
//! drift modes behind the PR #2825 attribution bypass. The narrower
//! agents/skills/deployment probes do not catch them.
//!
//! **`~/.trusty-mpm/claude-config` is NOT one of them (#7797).** This probe
//! also warned whenever that directory merely existed, calling it superseded
//! and safe to delete. It is the standalone driver's LIVE `CLAUDE_CONFIG_DIR`:
//! `ensure_global_config_dir` creates and populates it on every `tm run`,
//! `tm load` and `tm update`, and `tm reinstall`'s `standalone` target
//! (`core::reinstall::reinstall_targets`) redeploys into it. So the sub-check
//! warned on every environment that had ever launched a standalone session,
//! and its remediation was undone by the next launch. Nothing distinguishes a
//! pre-migration leftover there from the live tier — the driver deploys the
//! same agents and skills into it today — so the sub-check is gone rather than
//! narrowed. The tm-owned `~/.trusty-tools/trusty-mpm/claude-config` that
//! DAEMON-managed sessions use is a different directory and is unaffected.
//!
//! The OTHER drift mode — deployed skill content diverging from the installed
//! binary's assets — used to live here too. Issue #4604 moved it to
//! [`super::doctor_skill_drift`] when its reference point changed from the
//! `~/.trusty-mpm/framework/skills` extraction cache (which could itself lag
//! the binary, making every skill it covered report clean) to the binary's own
//! embedded assets.
//!
//! What: [`check_legacy_instruction_sources`] `Warn`s when bundled skill
//! copies are left in `~/.claude/skills` — advisory-only, since a leftover
//! copy shadows rather than silently drops a convention. It never blocks
//! session start and never writes.
//! Test: the `tests` module below covers the present/absent branches.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Probe for legacy GLOBAL skill copies that can serve stale guidance.
///
/// Why: before managed sessions went project-local, skills deployed into the
/// global `~/.claude/skills/`. That tier is superseded (project-local
/// `.claude/skills/` and the tm-managed `CLAUDE_CONFIG_DIR`), but leftover
/// copies keep being read by Claude Code and can serve outdated conventions —
/// e.g. an old attribution footer — to any session (issue #2876). This probe
/// makes those leftovers visible.
/// What: `Warn` (advisory — never `Fail`) when `~/.claude/skills` holds bundled
/// skill copies, naming the count and the one-line remediation; `Ok` when it
/// holds none. `home` is the base to resolve that path under (the real home in
/// production, a temp dir in tests).
///
/// #7102: the remediation it prints must be one that actually clears it. It
/// used to offer `tm install`, whose skill step wrote this same directory, so
/// following the advice reproduced the warning; the installer now deploys
/// bundled skills to the managed tier only
/// ([`crate::core::skill_install_tiers::deploy_install_skill_tiers`]) and the
/// text says to delete the copies. #7783 closed the last writer that refilled
/// `~/.claude/skills` ([`crate::core::reinstall`]) and widened the count past
/// the `tm-` prefix — see [`count_legacy_bundled_skills`].
///
/// #7797: `~/.trusty-mpm/claude-config` is no longer a finding here. See the
/// module doc — it is the standalone driver's live config home, not a
/// leftover, so its mere existence warned on every environment.
/// Test: `legacy_sources_ok_when_absent`, `legacy_sources_warns_on_tm_skills`,
/// `legacy_sources_counts_an_unprefixed_bundled_skill`,
/// `legacy_sources_ok_when_the_standalone_config_dir_exists`,
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

    // #7797: no `~/.trusty-mpm/claude-config` sub-check — that directory is the
    // standalone driver's live CLAUDE_CONFIG_DIR, recreated by the next
    // `tm run`/`tm load`/`tm update` and redeployed into by `tm reinstall`.

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
    fn legacy_sources_ok_when_the_standalone_config_dir_exists() {
        // #7797: `~/.trusty-mpm/claude-config` used to `Warn` on mere
        // existence. It is the standalone driver's live CLAUDE_CONFIG_DIR —
        // `ensure_global_config_dir` creates it on every `tm run`/`tm load`/
        // `tm update` and `tm reinstall`'s `standalone` target deploys agents
        // and skills into it — so the warning fired on every environment that
        // had ever launched one, and advised deleting a directory tm recreates.
        // Populate it the way the driver does, not as a bare directory: the
        // verdict must be Ok either way.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".trusty-mpm").join("claude-config");
        std::fs::create_dir_all(cfg.join("agents")).unwrap();
        std::fs::create_dir_all(cfg.join("skills").join("documentation-style")).unwrap();
        std::fs::write(
            cfg.join("skills")
                .join("documentation-style")
                .join("SKILL.md"),
            "deployed by the standalone driver",
        )
        .unwrap();

        let check = check_legacy_instruction_sources(tmp.path());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(
            !check.message.contains("claude-config"),
            "{}",
            check.message
        );
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
