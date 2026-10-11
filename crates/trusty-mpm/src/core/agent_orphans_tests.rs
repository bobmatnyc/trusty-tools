//! Tests for the canonical-dir orphan agent sweep (#8992).
//!
//! Why: the sweep DELETES from the directory every PM shares, so each refusal
//! and each fail-closed arm needs a test that goes red if it is dropped.
//! What: temp-dir deploy dirs driven through [`super::sweep_orphan_agents_in`]
//! and [`super::scan_orphan_agents`]. No test touches the host's agent dir.
//! Test: this file.

use super::*;
use tempfile::TempDir;
use trusty_agents_common::agents::manifest::{ManifestEntry, Origin, checksum};

/// A minimal deployed agent document; `stamped` adds the #4698 stamp.
fn doc(name: &str, stamped: bool) -> String {
    let stamp = if stamped {
        "provenance: framework-owned\n"
    } else {
        ""
    };
    format!("---\nname: {name}\nrole: ops\n{stamp}---\n\nBody.\n")
}

/// Write `<dir>/<file_name>`.
fn place(dir: &Path, file_name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(file_name), body).unwrap();
}

/// Record `file_name` in `dir`'s ledger with `body`'s checksum.
fn track(dir: &Path, file_name: &str, body: &str, origin: Origin) {
    let mut manifest = AgentManifest::load(dir);
    manifest.managed.insert(
        file_name.to_owned(),
        ManifestEntry {
            source_chain: vec![],
            checksum: checksum(body),
            deployed_at: "2026-08-01T14:36:49Z".to_owned(),
            origin,
        },
    );
    manifest.save(dir).unwrap();
}

/// Place AND track one file.
fn deploy(dir: &Path, file_name: &str, body: &str, origin: Origin) {
    place(dir, file_name, body);
    track(dir, file_name, body, origin);
}

/// The live set: the agents the "current bundle" ships.
fn live() -> BTreeSet<String> {
    ["engineer", "base-agent", "qa"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// A deploy dir holding one current agent (`engineer.md`) and `BASE-AGENT.md`.
fn deploy_dir(root: &TempDir) -> PathBuf {
    let dir = root.path().join("agents");
    deploy(&dir, "engineer.md", &doc("engineer", true), Origin::Bundled);
    deploy(
        &dir,
        "BASE-AGENT.md",
        &doc("base-agent", true),
        Origin::Bundled,
    );
    dir
}

/// The one step for `file_name`, or a panic naming what was produced.
fn step_for<'a>(steps: &'a [RepairStep], file_name: &str) -> &'a RepairStep {
    steps
        .iter()
        .find(|s| s.path.file_name().is_some_and(|n| n == file_name))
        .unwrap_or_else(|| panic!("no step for {file_name}: {steps:?}"))
}

/// Assert the sweep refused the whole dir and produced nothing else.
fn assert_dir_refused(steps: &[RepairStep], dir: &Path) {
    assert_eq!(steps.len(), 1, "one dir-wide refusal expected: {steps:?}");
    assert_eq!(steps[0].path, dir);
    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{steps:?}"
    );
}

#[test]
fn a_pristine_orphan_is_removed() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);
    let body = doc("ops", true);
    deploy(&dir, "ops.md", &body, Origin::Bundled);

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply);

    assert_eq!(steps.len(), 1, "only the orphan is a step: {steps:?}");
    let StepStatus::Applied {
        backup: Some(backup),
    } = &step_for(&steps, "ops.md").status
    else {
        panic!("a pristine bundled orphan must be removed: {steps:?}");
    };
    assert!(!dir.join("ops.md").exists(), "the orphan must be gone");
    assert_eq!(
        std::fs::read_to_string(backup).unwrap(),
        body,
        "backed up first"
    );
    assert!(backup.starts_with(backups.path().join("canonical-agents")));
    let ledger = AgentManifest::load(&dir);
    assert!(
        !ledger.managed.contains_key("ops.md"),
        "ledger entry dropped"
    );
    assert!(
        ledger.managed.contains_key("engineer.md"),
        "others untouched"
    );
    // A second run finds nothing left to do.
    assert!(sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply).is_empty());
}

#[test]
fn an_edited_orphan_is_kept() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);
    track(&dir, "ops.md", &doc("ops", true), Origin::Bundled);
    let edited = format!("{}\nMy own note.\n", doc("ops", true));
    place(&dir, "ops.md", &edited);

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply);

    let StepStatus::Refused(why) = &step_for(&steps, "ops.md").status else {
        panic!("an edited orphan must be refused: {steps:?}");
    };
    assert!(why.contains("edited"), "the reason names the edit: {why}");
    assert_eq!(std::fs::read_to_string(dir.join("ops.md")).unwrap(), edited);
    assert!(AgentManifest::load(&dir).managed.contains_key("ops.md"));
}

#[test]
fn an_untracked_file_is_kept() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);
    place(&dir, "my-agent.md", &doc("my-agent", false));
    place(&dir, "copied.md", &doc("copied", true));

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply);

    let StepStatus::Refused(plain) = &step_for(&steps, "my-agent.md").status else {
        panic!("a file with no tm provenance must be refused: {steps:?}");
    };
    assert!(plain.contains("no tm provenance"), "{plain}");
    // The stamp alone is never a licence: there is no checksum to verify.
    let StepStatus::Refused(stamped) = &step_for(&steps, "copied.md").status else {
        panic!("a stamped file absent from the ledger must be refused: {steps:?}");
    };
    assert!(stamped.contains("stamped"), "{stamped}");
    assert!(dir.join("my-agent.md").is_file());
    assert!(dir.join("copied.md").is_file());
}

#[test]
fn a_user_origin_orphan_is_kept() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);
    deploy(&dir, "mine.md", &doc("mine", false), Origin::User);

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply);

    assert!(
        matches!(step_for(&steps, "mine.md").status, StepStatus::Refused(_)),
        "{steps:?}"
    );
    assert!(dir.join("mine.md").is_file());
}

#[test]
fn a_current_bundled_agent_is_kept() {
    // `BASE-AGENT.md` declares `name: base-agent`: matched by stem and case,
    // it is live, not an orphan.
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply);

    assert!(steps.is_empty(), "no current agent is an orphan: {steps:?}");
    assert!(dir.join("engineer.md").is_file());
    assert!(dir.join("BASE-AGENT.md").is_file());
}

#[test]
fn a_missing_ledger_refuses_the_sweep() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = root.path().join("agents");
    place(&dir, "ops.md", &doc("ops", true));

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply);

    assert_dir_refused(&steps, &dir);
    assert!(
        dir.join("ops.md").is_file(),
        "nothing deleted without a ledger"
    );
    assert!(
        scan_orphan_agents(&dir, &live()).is_err(),
        "doctor fails closed too"
    );
}

#[test]
fn a_corrupt_ledger_refuses_the_sweep() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);
    deploy(&dir, "ops.md", &doc("ops", true), Origin::Bundled);
    std::fs::write(dir.join(MANIFEST_FILE), "{ not json").unwrap();

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply);

    assert_dir_refused(&steps, &dir);
    assert!(
        dir.join("ops.md").is_file(),
        "nothing deleted on a corrupt ledger"
    );
    assert!(
        scan_orphan_agents(&dir, &live()).is_err(),
        "doctor fails closed too"
    );
}

#[test]
fn an_empty_live_set_refuses_the_sweep() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);

    let steps = sweep_orphan_agents_in(&dir, &BTreeSet::new(), backups.path(), RepairMode::Apply);

    assert_dir_refused(&steps, &dir);
    assert!(dir.join("engineer.md").is_file());
    assert!(scan_orphan_agents(&dir, &BTreeSet::new()).is_err());
}

#[test]
fn a_dry_run_writes_nothing() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);
    deploy(&dir, "ops.md", &doc("ops", true), Origin::Bundled);
    let ledger_before = std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap();

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::DryRun);

    assert_eq!(step_for(&steps, "ops.md").status, StepStatus::Planned);
    assert!(dir.join("ops.md").is_file());
    assert_eq!(
        std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap(),
        ledger_before
    );
    assert!(!backups.path().join("canonical-agents").exists());
}

#[test]
fn a_stale_ledger_entry_is_dropped_without_a_backup() {
    let root = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let dir = deploy_dir(&root);
    track(&dir, "gone.md", &doc("gone", true), Origin::Bundled);

    let steps = sweep_orphan_agents_in(&dir, &live(), backups.path(), RepairMode::Apply);

    assert_eq!(
        step_for(&steps, "gone.md").status,
        StepStatus::Applied { backup: None }
    );
    assert!(!AgentManifest::load(&dir).managed.contains_key("gone.md"));
}

#[test]
fn scan_reports_what_the_sweep_would_do() {
    let root = TempDir::new().unwrap();
    let dir = deploy_dir(&root);
    deploy(&dir, "ops.md", &doc("ops", true), Origin::Bundled);
    place(&dir, "my-agent.md", &doc("my-agent", false));

    let orphans = scan_orphan_agents(&dir, &live()).unwrap();

    let names: Vec<&str> = orphans.iter().map(|o| o.file_name.as_str()).collect();
    assert_eq!(names, ["my-agent.md", "ops.md"]);
    assert_eq!(orphans[1].verdict, AgentRemoval::Removable);
    assert!(matches!(orphans[0].verdict, AgentRemoval::Kept(_)));
    assert!(dir.join("ops.md").is_file(), "the scan writes nothing");
    assert!(
        scan_orphan_agents(&root.path().join("absent"), &live())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn live_names_cover_every_repo_roster_file() {
    // Every file the real bundle deploys must be live, or the sweep would
    // delete it. Hermetic paths keep host state out of the source-dir half.
    let base = TempDir::new().unwrap();
    let mut paths = FrameworkPaths::under(base.path());
    paths.trusty_mpm_root = None;
    let roster = crate::core::content_source::test_support::repo_roster();

    let live = live_agent_names(&paths, &roster);

    for (file_name, _) in roster.iter() {
        assert!(
            live.contains(&stem_key(file_name)),
            "{file_name} must be live: {live:?}"
        );
    }
    assert!(live.contains("base-agent") && live.contains("rust-engineer"));
    assert!(!live.contains("ops"), "ops left the bundle in #4765");
}
