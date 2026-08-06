//! Behaviour tests for the `tm reinstall` asset engine (see `reinstall.rs`).
//!
//! Why: the whole point of the command is that it reaches EVERY destination and
//! says what it did at each. Both halves need pinning — a target list that
//! silently drops the standalone tier, or a report that shows zeros where a
//! deploy was declined, is the failure the command exists to end.
//! What: target enumeration, the hop-1 refresh, the ownership matrix
//! (preserved without `--force`, replaced with it, corruption repaired either
//! way), a missing destination, a failed destination, and the ledger-lock
//! serialisation that keeps a concurrent daemon deploy from racing it.
//! Test: this file IS the test module.

use super::*;
use tempfile::TempDir;

/// A minimal source agent, and a base it extends.
const BASE_AGENT: &str = "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBase content.\n";
const ENGINEER: &str = "---\nname: engineer\nrole: engineer\nextends: base-agent\n---\n\n# Engineer\n\nEngineer content.\n";
/// A minimal source skill.
const DEMO_SKILL: &str = "---\nname: demo-skill\n---\n\n# Demo\n\nBundled skill text.\n";

/// A framework layout under `base` holding two agents and one skill, with both
/// bundle stamps pinned as already-current.
///
/// Why: pinning the stamps is what stops [`reinstall_assets`]'s hop-1 self-heal
/// materializing the REAL compiled-in bundle over this fixture — the same trick
/// #4873 used for its skill fixture. Without it every ownership assertion below
/// would be made against ~110 real assets instead of three known ones.
/// Test: used by every `reinstall_*` test except
/// `reinstall_refreshes_the_source_first`, which deliberately leaves the stamps
/// absent.
fn seeded(base: &std::path::Path) -> FrameworkPaths {
    let paths = FrameworkPaths::under(base);
    std::fs::create_dir_all(&paths.agents).unwrap();
    std::fs::write(paths.agents.join("base-agent.md"), BASE_AGENT).unwrap();
    std::fs::write(paths.agents.join("engineer.md"), ENGINEER).unwrap();
    std::fs::write(
        paths
            .agents
            .join(crate::core::agent_source::STAMP_FILE_NAME),
        crate::core::agent_source::agent_bundle_stamp(),
    )
    .unwrap();

    std::fs::create_dir_all(&paths.skills).unwrap();
    std::fs::write(paths.skills.join("demo-skill.md"), DEMO_SKILL).unwrap();
    std::fs::write(
        paths
            .skills
            .join(crate::core::skill_source::STAMP_FILE_NAME),
        crate::core::skill_source::skill_bundle_stamp(),
    )
    .unwrap();
    paths
}

/// The standalone driver's config dir, under the same hermetic base.
fn standalone_dir(base: &std::path::Path) -> std::path::PathBuf {
    base.join(".trusty-mpm").join("claude-config")
}

/// Find one tier report by label.
fn tier<'a>(report: &'a ReinstallReport, label: &str) -> &'a TierReport {
    report
        .tiers
        .iter()
        .find(|t| t.label == label)
        .unwrap_or_else(|| panic!("no `{label}` tier in {:?}", report.tiers))
}

#[test]
fn targets_cover_the_standalone_tier() {
    // #4849: `tm run`/`load`/`login` deploy into `~/.trusty-mpm/claude-config`,
    // which `skill_deploy_tiers` — and therefore every command built on it —
    // structurally cannot reach. It is the reason this target list exists.
    let base = TempDir::new().unwrap();
    let paths = FrameworkPaths::under(base.path());
    let sa = standalone_dir(base.path());

    let targets = reinstall_targets(&paths, &sa, None);

    let standalone = targets
        .iter()
        .find(|t| t.label == "standalone")
        .expect("the standalone tier must be covered");
    assert_eq!(standalone.skills_dir, sa.join("skills"));
    assert_eq!(standalone.agents_dir, Some(sa.join("agents")));
}

#[test]
fn targets_attach_agents_to_two_destinations_only() {
    // #4409 pins framework-owned agents to the tm-managed config dir. The only
    // other place they legitimately land is the standalone driver's own config
    // dir; the operator's `~/.claude/agents` and any project must stay clear.
    let base = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let paths = FrameworkPaths::under(base.path());

    let targets = reinstall_targets(&paths, &standalone_dir(base.path()), Some(project.path()));

    let with_agents: Vec<&str> = targets
        .iter()
        .filter(|t| t.agents_dir.is_some())
        .map(|t| t.label.as_str())
        .collect();
    assert_eq!(with_agents, vec!["managed config", "standalone"]);
    assert_eq!(
        tier_dir(&targets, "managed config"),
        Some(paths.agent_deploy_dir())
    );
}

/// The agents directory of a labelled target, for a one-line assertion.
fn tier_dir(targets: &[ReinstallTarget], label: &str) -> Option<std::path::PathBuf> {
    targets
        .iter()
        .find(|t| t.label == label)
        .and_then(|t| t.agents_dir.clone())
}

#[test]
fn targets_deduplicate() {
    // A machine whose standalone root IS the managed config dir must be served
    // once, not twice — a second pass would deploy over its own output and
    // double every count.
    let base = TempDir::new().unwrap();
    let paths = FrameworkPaths::under(base.path());
    let managed_config = paths.agent_deploy_dir().parent().unwrap().to_path_buf();

    let targets = reinstall_targets(&paths, &managed_config, None);

    assert!(
        !targets.iter().any(|t| t.label == "standalone"),
        "the standalone tier duplicates the managed one here: {targets:?}"
    );
}

#[test]
fn reinstall_reports_every_target() {
    // The command's contract: one report per destination, each naming what it
    // actually did there. A tier reporting all zeros because it was never
    // visited is exactly the silent no-op this replaces.
    let base = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let backups = base.path().join("backups");

    let report = reinstall_assets(
        &paths,
        &standalone_dir(base.path()),
        Some(project.path()),
        false,
        &backups,
    );

    assert!(!report.has_failures(), "{report:?}");
    assert_eq!(report.tiers.len(), 4, "{:?}", report.tiers);
    for t in &report.tiers {
        assert_eq!(t.skills.deployed, 1, "{} deployed no skill: {t:?}", t.label);
        assert!(t.skills_dir.join("demo-skill").join("SKILL.md").is_file());
    }
    for label in ["managed config", "standalone"] {
        let t = tier(&report, label);
        assert_eq!(t.agents.deployed, 2, "{label}: {t:?}");
        let dir = t.agents_dir.as_ref().unwrap();
        assert!(dir.join("engineer.md").is_file());
    }
    // #4409: agents never land in the operator home or the project.
    assert!(!paths.claude_agents_dir().join("engineer.md").exists());
    assert!(
        !project
            .path()
            .join(".claude")
            .join("agents")
            .join("engineer.md")
            .exists()
    );
}

#[test]
fn reinstall_creates_a_missing_destination() {
    // A machine that has never run the standalone driver has no
    // `claude-config/` at all. Hop 2 must provision it rather than skip it.
    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let sa = standalone_dir(base.path());
    assert!(!sa.exists());

    let report = reinstall_assets(&paths, &sa, None, false, &base.path().join("backups"));

    assert!(!report.has_failures(), "{report:?}");
    assert!(sa.join("agents").join("engineer.md").is_file());
    assert!(
        sa.join("skills")
            .join("demo-skill")
            .join("SKILL.md")
            .is_file()
    );
}

#[test]
fn reinstall_refreshes_the_source_first() {
    // Hop 1 is the half that makes a compiled-in asset change reach disk at all
    // (#4840/#1917). Without the pinned stamps the source is stale by
    // definition, so a run against a bare base must report a refresh — and the
    // real bundled roster must then be what lands.
    let base = TempDir::new().unwrap();
    let paths = FrameworkPaths::under(base.path());

    let report = reinstall_assets(
        &paths,
        &standalone_dir(base.path()),
        None,
        false,
        &base.path().join("backups"),
    );

    assert!(report.agent_source_refreshed, "{report:?}");
    assert!(report.skill_source_refreshed, "{report:?}");
    assert!(paths.agents.join("engineer.md").is_file());
    assert!(paths.skills.join("tm-doctor.md").is_file());
    assert!(tier(&report, "managed config").agents.deployed > 0);
}

#[test]
fn reinstall_preserves_a_customized_agent_without_force() {
    // Ownership: a file the manifest tracks as USER-owned and that has been
    // edited is the operator's. It survives a reinstall byte-for-byte, and the
    // report says so rather than counting it as deployed.
    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let dest = paths.agent_deploy_dir();
    let custom = "---\nname: engineer\n---\n\nMINE — KEEP ME\n";
    seed_user_owned_agent(&dest, custom);

    let report = reinstall_assets(
        &paths,
        &standalone_dir(base.path()),
        None,
        false,
        &base.path().join("backups"),
    );

    let t = tier(&report, "managed config");
    assert_eq!(t.agents.preserved, 1, "{t:?}");
    assert_eq!(
        std::fs::read_to_string(dest.join("engineer.md")).unwrap(),
        custom
    );
    assert!(
        t.notes.iter().any(|n| n.contains("tm reinstall --force")),
        "the preserved note must name the escape hatch: {:?}",
        t.notes
    );
}

#[test]
fn reinstall_replaces_a_customized_agent_with_force() {
    // `--force` is the explicit clobber. The bundled composition wins, and the
    // operator's copy is backed up in place rather than lost.
    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let dest = paths.agent_deploy_dir();
    seed_user_owned_agent(&dest, "---\nname: engineer\n---\n\nMINE\n");

    let report = reinstall_assets(
        &paths,
        &standalone_dir(base.path()),
        None,
        true,
        &base.path().join("backups"),
    );

    let t = tier(&report, "managed config");
    assert_eq!(t.agents.preserved, 0, "{t:?}");
    let deployed = std::fs::read_to_string(dest.join("engineer.md")).unwrap();
    assert!(deployed.contains("Engineer content."), "{deployed}");
    let backups: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
        .collect();
    assert_eq!(backups.len(), 1, "the overwrite must be backed up");
}

#[test]
fn reinstall_repairs_a_corrupt_agent_without_force() {
    // #4408's carve-out, which must not regress: a FRAMEWORK-owned file whose
    // frontmatter has been mangled is corruption, not customization. It is
    // repaired with no `--force`, and reported as repaired rather than as an
    // ordinary refresh.
    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let sa = standalone_dir(base.path());
    let dest = paths.agent_deploy_dir();

    // First run establishes framework ownership in the manifest.
    reinstall_assets(&paths, &sa, None, false, &base.path().join("backups"));
    // The literal #4408 stub: `name:`-only frontmatter, no `description:`.
    std::fs::write(dest.join("engineer.md"), "---\nname: engineer\n---\n\nv1\n").unwrap();

    let report = reinstall_assets(&paths, &sa, None, false, &base.path().join("backups"));

    let t = tier(&report, "managed config");
    assert_eq!(t.agents.repaired, 1, "{t:?}");
    assert_eq!(t.agents.preserved, 0, "{t:?}");
    assert!(
        std::fs::read_to_string(dest.join("engineer.md"))
            .unwrap()
            .contains("Engineer content.")
    );
}

/// Deploy `content` as `engineer.md` at `dest` under a USER-owned manifest
/// entry whose checksum deliberately disagrees with it.
///
/// Why: this is the exact shape the deployer's preserve branch keys off —
/// tracked, user-owned origin, edited. Building it by hand keeps the fixture
/// independent of whichever command last wrote a real manifest.
/// Test: used by the two agent-ownership tests above.
fn seed_user_owned_agent(dest: &std::path::Path, content: &str) {
    use trusty_agents_common::agents::manifest::{AgentManifest, ManifestEntry, Origin, checksum};
    std::fs::create_dir_all(dest).unwrap();
    std::fs::write(dest.join("engineer.md"), content).unwrap();
    let mut manifest = AgentManifest::default();
    manifest.managed.insert(
        "engineer.md".to_string(),
        ManifestEntry {
            source_chain: vec!["engineer".to_string()],
            checksum: checksum("something else entirely"),
            deployed_at: "2026-01-01T00:00:00Z".to_string(),
            origin: Origin::User,
        },
    );
    manifest.save(dest).unwrap();
}

#[test]
fn reinstall_preserves_a_customized_skill_without_force() {
    // The skill half of the ownership rule. A hand-edited deployed skill is
    // frozen by the deployer on purpose; the reinstall must respect that and
    // say it did.
    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let sa = standalone_dir(base.path());
    let dest = paths.agent_deploy_dir().parent().unwrap().join("skills");

    reinstall_assets(&paths, &sa, None, false, &base.path().join("backups"));
    let deployed = dest.join("demo-skill").join("SKILL.md");
    std::fs::write(&deployed, "MY OWN TEXT\n").unwrap();

    let report = reinstall_assets(&paths, &sa, None, false, &base.path().join("backups"));

    let t = tier(&report, "managed config");
    assert_eq!(t.skills.preserved, 1, "{t:?}");
    assert_eq!(std::fs::read_to_string(&deployed).unwrap(), "MY OWN TEXT\n");
}

#[test]
fn reinstall_replaces_a_customized_skill_with_force() {
    // `--force` re-stamps the frozen skill and lets the very next deploy write
    // the bundled text over it — with the original copied under `backup_root`
    // first, which is this module's non-negotiable condition for clobbering.
    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let sa = standalone_dir(base.path());
    let backups = base.path().join("backups");
    let dest = paths.agent_deploy_dir().parent().unwrap().join("skills");

    reinstall_assets(&paths, &sa, None, false, &backups);
    let deployed = dest.join("demo-skill").join("SKILL.md");
    std::fs::write(&deployed, "MY OWN TEXT\n").unwrap();

    let report = reinstall_assets(&paths, &sa, None, true, &backups);

    let t = tier(&report, "managed config");
    assert_eq!(t.skills.preserved, 0, "{t:?}");
    assert!(
        std::fs::read_to_string(&deployed)
            .unwrap()
            .contains("Bundled skill text.")
    );
    assert!(
        backups.join("moved-paths.log").is_file(),
        "the clobber must leave a backup ledger under {}",
        backups.display()
    );
}

#[test]
fn force_leaves_an_operator_skill_alone() {
    // The roster is the only admission test. A skill whose name matches nothing
    // bundled is the operator's, and `--force` is not licence to touch it.
    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let sa = standalone_dir(base.path());
    let dest = paths.agent_deploy_dir().parent().unwrap().join("skills");
    let mine = dest.join("my-own-skill");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("SKILL.md"), "OPERATOR CONTENT\n").unwrap();

    reinstall_assets(&paths, &sa, None, true, &base.path().join("backups"));

    assert_eq!(
        std::fs::read_to_string(mine.join("SKILL.md")).unwrap(),
        "OPERATOR CONTENT\n"
    );
}

#[test]
fn reinstall_reports_a_failed_destination_and_serves_the_others() {
    // A destination that cannot be written must not abandon the rest of the
    // run — abandoning two tiers because the third is read-only is the silent
    // partial deploy this command exists to end.
    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let sa = standalone_dir(base.path());
    // A regular FILE where the managed skills directory belongs: every write
    // into it fails, on every platform, without depending on permission bits.
    let managed_skills = paths.agent_deploy_dir().parent().unwrap().join("skills");
    std::fs::create_dir_all(managed_skills.parent().unwrap()).unwrap();
    std::fs::write(&managed_skills, "not a directory").unwrap();

    let report = reinstall_assets(&paths, &sa, None, false, &base.path().join("backups"));

    assert!(report.has_failures(), "{report:?}");
    assert_eq!(tier(&report, "standalone").skills.deployed, 1);
    assert!(
        sa.join("skills")
            .join("demo-skill")
            .join("SKILL.md")
            .is_file()
    );
}

#[test]
fn reinstall_serialises_against_a_held_skill_ledger_lock() {
    // The concurrency answer (#4881): `tm reinstall` can plausibly run while
    // the daemon deploys for a session spawn. It takes no lock of its own — it
    // routes every write through the deployers, which hold the target
    // directory's exclusive ledger lock — so a reinstall must BLOCK on a lock
    // another writer holds rather than racing its manifest.
    use std::sync::mpsc;
    use trusty_agents_common::agents::manifest::ManifestError;

    let base = TempDir::new().unwrap();
    let paths = seeded(base.path());
    let sa = standalone_dir(base.path());
    let dest = sa.join("skills");
    std::fs::create_dir_all(&dest).unwrap();

    let (tx, rx) = mpsc::channel();
    let paths_moved = paths.clone();
    let sa_moved = sa.clone();
    let backups = base.path().join("backups");
    let marker = dest.join("demo-skill").join("SKILL.md");

    // The handle leaves the critical section rather than being joined inside
    // it: the reinstall cannot finish until we release.
    let handle = trusty_agents_common::skills::manifest::with_skill_manifest_lock::<
        _,
        ManifestError,
        _,
    >(&dest, || {
        let handle = std::thread::spawn(move || {
            tx.send(reinstall_assets(
                &paths_moved,
                &sa_moved,
                None,
                false,
                &backups,
            ))
            .ok();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).is_err(),
            "the reinstall finished while another writer held the skill ledger lock"
        );
        assert!(
            !marker.exists(),
            "the reinstall wrote a skill file while the ledger lock was held"
        );
        Ok(handle)
    })
    .unwrap();

    handle.join().unwrap();
    let report = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the reinstall must complete once the lock is released");
    assert!(marker.is_file(), "{report:?}");
}

#[test]
fn preserved_note_is_empty_when_nothing_was_preserved() {
    assert_eq!(preserved_note("agent", &[], ""), None);
}

#[test]
fn preserved_note_names_the_force_flag() {
    // The note has to point at the flag on the command the operator just ran,
    // not at `tm install --reset-agents`, which is a different command.
    let note = preserved_note("skill", &["a.md".to_string()], "a.md").unwrap();
    assert!(note.contains("1 skill file(s)"), "{note}");
    assert!(note.contains("tm reinstall --force"), "{note}");
}
