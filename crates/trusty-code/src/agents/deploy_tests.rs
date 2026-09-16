//! Unit tests for the project roster deploy (#2074, #7727, #7779).
//!
//! Why: split out of `deploy.rs` so the production file stays inside the
//! 500-SLOC cap while #7779 adds the pinned-handle deploy path.
//! What: the deploy's skip, corrupt-ledger, hand-edit and symlink-refusal
//! branches, plus the two post-validation swap regressions #7779 closes.
//! Test: this file IS the test module.

use super::*;
use trusty_agents_common::agents::manifest::Origin;

/// Every dispatchable roster name, for the count assertions below.
fn roster_names() -> Vec<&'static str> {
    DEFAULT_AGENTS.iter().map(EmbeddedAgent::name).collect()
}

/// `.md` filenames actually present in a deployed target directory.
fn deployed_md_files(target: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(target)
        .expect("read_dir on the deploy target")
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    names
}

#[test]
fn roster_target_is_under_the_native_config_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        roster_target(tmp.path()),
        tmp.path().join(".trusty-code").join(AGENTS_DIRNAME)
    );
}

#[test]
fn staged_sources_cover_every_roster_name_and_base_template() {
    let staged = stage_embedded_sources().expect("stage");
    for name in roster_names() {
        assert!(
            staged.path().join(format!("{name}.md")).is_file(),
            "roster agent '{name}' must have a staged source file"
        );
    }
    for base in crate::assets::BASE_AGENT_NAMES {
        let map = trusty_agents_common::agents::builder::build_source_map(staged.path());
        assert!(
            map.contains_key(*base),
            "base template '{base}' must be stageable for extends resolution"
        );
    }
}

/// Assert a symlinked skill-ref location refuses the deploy, writes no
/// agent, and leaves the out-of-project victim byte-identical (#7727).
#[cfg(unix)]
fn assert_skill_ref_symlink_refused(project: &Path, victim: &Path, original: &str) {
    let err = ensure_roster_deployed(project).expect_err("a symlink escape must be refused");
    // #7779: the refusal now comes from the `O_NOFOLLOW` descent for a symlink
    // BELOW the config root, and from the unchanged anchoring check for a
    // config root that escapes the project. Either is the same verdict.
    assert!(
        matches!(
            err,
            RosterDeployError::WriteTarget(
                WriteTargetError::SymlinkEscape { .. } | WriteTargetError::Unpinned { .. }
            )
        ),
        "expected a symlink refusal, got {err:?}"
    );
    assert_eq!(std::fs::read_to_string(victim).expect("victim"), original);
    // #7779: pinning the write targets creates the (empty) directories before
    // the refusal can fire, so the property is "no agent file landed", not "no
    // directory exists".
    assert!(
        deployed_md_files(&roster_target(project)).is_empty(),
        "no agent may be written once the refusal fires"
    );
}

/// #7727 review: `.trusty-code/skill-refs` committed as a symlink out of
/// the project.
#[cfg(unix)]
#[test]
fn symlinked_skill_refs_dir_is_refused_before_any_write() {
    let project = tempfile::tempdir().expect("project");
    let outside = tempfile::tempdir().expect("outside");
    let victim = outside.path().join("self-improvement-loop/SKILL.md");
    std::fs::create_dir_all(victim.parent().expect("parent")).expect("mkdir");
    std::fs::write(&victim, "USER DATA - hand written skill").expect("victim");
    let refs = super::super::skill_refs::project_skill_refs_dir(project.path());
    std::fs::create_dir_all(refs.parent().expect("parent")).expect("mkdir");
    std::os::unix::fs::symlink(outside.path(), &refs).expect("symlink");

    assert_skill_ref_symlink_refused(project.path(), &victim, "USER DATA - hand written skill");
}

/// #7727 review: a real `skill-refs` dir whose one skill folder is a
/// symlink to a user's own skill.
#[cfg(unix)]
#[test]
fn symlinked_skill_folder_is_refused_before_any_write() {
    let project = tempfile::tempdir().expect("project");
    let outside = tempfile::tempdir().expect("outside");
    let user_skill = outside.path().join("my-skill");
    std::fs::create_dir_all(&user_skill).expect("mkdir");
    let victim = user_skill.join("SKILL.md");
    std::fs::write(&victim, "USER SKILL - hand written").expect("victim");
    let refs = super::super::skill_refs::project_skill_refs_dir(project.path());
    std::fs::create_dir_all(&refs).expect("mkdir");
    std::os::unix::fs::symlink(&user_skill, refs.join("verification-before-completion"))
        .expect("symlink");

    assert_skill_ref_symlink_refused(project.path(), &victim, "USER SKILL - hand written");
}

/// #7727 review: one skill-ref file committed as a symlink to a user file.
#[cfg(unix)]
#[test]
fn symlinked_skill_ref_file_is_refused_before_any_write() {
    let project = tempfile::tempdir().expect("project");
    let outside = tempfile::tempdir().expect("outside");
    let victim = outside.path().join("notes.md");
    std::fs::write(&victim, "USER FILE").expect("victim");
    let refs = super::super::skill_refs::project_skill_refs_dir(project.path());
    let link = refs.join("condition-based-waiting/SKILL.md");
    std::fs::create_dir_all(link.parent().expect("parent")).expect("mkdir");
    std::os::unix::fs::symlink(&victim, &link).expect("symlink");

    assert_skill_ref_symlink_refused(project.path(), &victim, "USER FILE");
}

#[test]
fn fresh_project_materializes_the_whole_roster() {
    let project = tempfile::tempdir().expect("project tempdir");
    let outcome = ensure_roster_deployed(project.path()).expect("deploy must succeed");
    let RosterDeploy::Deployed { target, result } = outcome else {
        panic!("a clean project must deploy, got: {outcome:?}");
    };

    assert!(result.failed.is_empty(), "no agent may fail: {result:?}");
    // Asserted from the filesystem, not from the deployer's own report.
    let on_disk = deployed_md_files(&target);
    assert_eq!(
        on_disk.len(),
        roster_names().len(),
        "every roster agent must land on disk, got: {on_disk:?}"
    );
    for name in roster_names() {
        assert!(
            target.join(format!("{name}.md")).is_file(),
            "'{name}.md' must exist on disk after the first deploy"
        );
    }

    // The manifest exists beside them and records framework ownership.
    let (manifest_path, status) = roster_manifest_status(project.path());
    assert!(manifest_path.is_file(), "the ledger must exist beside them");
    assert_eq!(
        status,
        ManifestStatus::Present {
            managed: roster_names().len()
        }
    );
    assert_eq!(
        AgentManifest::load(&target).managed["engineer.md"].origin,
        Origin::Bundled
    );
}

#[test]
fn base_templates_are_never_deployed() {
    let project = tempfile::tempdir().expect("project tempdir");
    ensure_roster_deployed(project.path()).expect("deploy");
    for base in crate::assets::BASE_AGENT_NAMES {
        let deployed = deployed_md_files(&roster_target(project.path()));
        assert!(
            !deployed
                .iter()
                .any(|n| n.to_lowercase() == format!("{base}.md")),
            "composition base '{base}' must never be deployed as a dispatchable agent"
        );
    }
}

#[test]
fn second_deploy_of_an_untouched_roster_writes_nothing() {
    let project = tempfile::tempdir().expect("project tempdir");
    ensure_roster_deployed(project.path()).expect("first deploy");
    let outcome = ensure_roster_deployed(project.path()).expect("second deploy");
    let RosterDeploy::Deployed { result, .. } = outcome else {
        panic!("second deploy must still run, got: {outcome:?}");
    };
    assert!(
        result.deployed.is_empty(),
        "an untouched roster must need no rewrite: {result:?}"
    );
    assert_eq!(result.unchanged.len(), roster_names().len());
}

#[test]
fn hand_edited_agent_survives_a_second_deploy() {
    let project = tempfile::tempdir().expect("project tempdir");
    ensure_roster_deployed(project.path()).expect("first deploy");
    let target = roster_target(project.path());
    let edited = target.join("engineer.md");

    let hand_edit = "---\nname: engineer\nmodel: marker/hand-edited\n---\n\nMine now.\n";
    std::fs::write(&edited, hand_edit).expect("hand-edit the deployed agent");

    // Twice, to prove the preservation is stable rather than one-shot.
    for _ in 0..2 {
        let outcome = ensure_roster_deployed(project.path()).expect("re-deploy");
        let RosterDeploy::Deployed { result, .. } = outcome else {
            panic!("re-deploy must run, got: {outcome:?}");
        };
        assert!(
            !result.deployed.contains(&"engineer.md".to_string())
                && !result.repaired.contains(&"engineer.md".to_string()),
            "a hand-edited agent must never be rewritten: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&edited).expect("read back"),
            hand_edit,
            "the hand edit must survive byte-identical"
        );
    }

    // Its neighbours are untouched by the carve-out.
    assert!(target.join("rust-engineer.md").is_file());
}

#[test]
fn untracked_file_is_left_to_the_deployer() {
    let project = tempfile::tempdir().expect("project tempdir");
    let target = roster_target(project.path());
    std::fs::create_dir_all(&target).expect("mkdir target");
    let squatter = "---\nname: engineer\n---\n\nProject-owned, never deployed by us.\n";
    std::fs::write(target.join("engineer.md"), squatter).expect("write untracked file");

    let outcome = ensure_roster_deployed(project.path()).expect("deploy");
    let RosterDeploy::Deployed { result, .. } = outcome else {
        panic!("deploy must run, got: {outcome:?}");
    };
    assert!(
        result
            .untracked_modified
            .contains(&"engineer.md".to_string()),
        "an untracked, differing file is the deployer's own skip branch: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("engineer.md")).expect("read back"),
        squatter
    );
}

#[test]
fn corrupt_manifest_is_reported_and_nothing_is_written() {
    let project = tempfile::tempdir().expect("project tempdir");
    let target = roster_target(project.path());
    std::fs::create_dir_all(&target).expect("mkdir target");
    std::fs::write(target.join(MANIFEST_FILE), b"not valid json{{{").expect("corrupt ledger");

    let err = ensure_roster_deployed(project.path()).expect_err("must refuse");
    assert!(
        matches!(err, RosterDeployError::ManifestCorrupt { .. }),
        "got: {err:?}"
    );
    assert!(
        err.to_string().contains("refusing to deploy"),
        "the message must say nothing was written: {err}"
    );
    assert!(
        deployed_md_files(&target).is_empty(),
        "a corrupt ledger must leave the directory untouched"
    );
    // And the ledger itself was never reset.
    assert_eq!(
        std::fs::read_to_string(target.join(MANIFEST_FILE)).expect("read back"),
        "not valid json{{{"
    );
}

#[test]
fn manifest_status_reports_absent_then_present() {
    let project = tempfile::tempdir().expect("project tempdir");
    let (path, status) = roster_manifest_status(project.path());
    assert_eq!(status, ManifestStatus::Absent);
    assert_eq!(status.as_str(), "absent");
    assert!(!path.exists());

    ensure_roster_deployed(project.path()).expect("deploy");
    let (_, status) = roster_manifest_status(project.path());
    assert_eq!(status.as_str(), "present");
    assert!(matches!(status, ManifestStatus::Present { managed } if managed > 0));
}

#[test]
fn manifest_status_reports_corrupt() {
    let project = tempfile::tempdir().expect("project tempdir");
    let target = roster_target(project.path());
    std::fs::create_dir_all(&target).expect("mkdir target");
    std::fs::write(target.join(MANIFEST_FILE), b"{{{").expect("corrupt ledger");
    let (_, status) = roster_manifest_status(project.path());
    assert_eq!(status.as_str(), "corrupt");
}

#[test]
fn deploy_is_skipped_when_claude_agents_dir_wins() {
    let project = tempfile::tempdir().expect("project tempdir");
    let claude = project.path().join(".claude").join(AGENTS_DIRNAME);
    std::fs::create_dir_all(&claude).expect("mkdir .claude/agents");
    std::fs::write(
        claude.join("engineer.md"),
        "---\nname: engineer\n---\n\nProject catalog.\n",
    )
    .expect("write .claude agent");

    let outcome = ensure_roster_deployed(project.path()).expect("skip is not an error");
    assert!(
        matches!(outcome, RosterDeploy::Skipped(SkipReason::CompatRootWins)),
        "a winning .claude/agents/ must never be shadowed, got: {outcome:?}"
    );
    assert!(
        !roster_target(project.path()).exists(),
        "nothing may be written when the deploy is skipped"
    );
}

#[test]
fn deploy_and_log_is_a_no_op_without_a_project() {
    assert!(deploy_and_log(None).is_none());
}

#[test]
fn deploy_and_log_reports_a_corrupt_ledger_without_panicking() {
    crate::test_support::begin_capture();

    let project = tempfile::tempdir().expect("project tempdir");
    let target = roster_target(project.path());
    std::fs::create_dir_all(&target).expect("mkdir target");
    std::fs::write(target.join(MANIFEST_FILE), b"{{{").expect("corrupt ledger");
    assert!(
        deploy_and_log(Some(project.path())).is_none(),
        "a corrupt ledger must degrade to the in-memory roster, not abort"
    );

    // Degrading silently is the failure this branch exists to prevent, so
    // the assertion is on the ERROR event itself — downgrading it to
    // `debug!` must fail this test, not just change a log line.
    let errors = crate::test_support::captured_at_least(tracing::Level::ERROR);
    assert!(
        errors
            .iter()
            .any(|m| m.contains("could not materialize the agent roster")),
        "the corrupt ledger must be reported at ERROR level, got: {errors:?}"
    );
}

#[test]
fn deployed_agents_load_back_through_the_disk_loader() {
    let project = tempfile::tempdir().expect("project tempdir");
    ensure_roster_deployed(project.path()).expect("deploy");
    let loaded = crate::agents::load_all_agents(&roster_target(project.path()));
    assert_eq!(
        loaded.len(),
        roster_names().len(),
        "every deployed file must parse through the disk loader"
    );
    let restricted = loaded
        .iter()
        .find(|c| c.agent.name == "code-critic")
        .expect("code-critic must be deployed");
    let allowed = restricted
        .tools
        .as_ref()
        .and_then(|t| t.allowed.as_ref())
        .expect("code-critic carries a tcode-local tool allowlist");
    assert!(
        !allowed.contains(&"write_file".to_string()),
        "the tcode-local read-only restriction must survive materialization: {allowed:?}"
    );
}

/// Replace a validated directory with a symlink, the way a racing writer does.
#[cfg(unix)]
fn swap_for_symlink(dir: &Path, victim: &Path) {
    std::fs::remove_dir_all(dir).expect("remove the validated directory");
    std::os::unix::fs::symlink(victim, dir).expect("symlink the victim in its place");
}

/// Everything a directory holds, for a "the victim got nothing" assertion.
#[cfg(unix)]
fn entries(dir: &Path) -> Vec<std::ffi::OsString> {
    std::fs::read_dir(dir)
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name())
        .collect()
}

/// #7779: `skill-refs` swapped for a symlink AFTER validation writes nothing
/// into the victim and reports the swap.
///
/// Why: the reproduced race. A 4 ms `skill-refs -> victim` swap loop during real
/// `tcode` deploys put a `SKILL.md.<pid>.<nanos>.tmp` in the victim in 6 of 40
/// runs, because `check_native_write_target` and `materialize_skill_refs` each
/// resolved the path independently.
/// What: validates by opening the pinned handle, performs the swap, then runs
/// the write. Asserts the victim is empty and the error is
/// `WriteTargetError::Unpinned`.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn swapped_skill_refs_dir_never_reaches_the_victim() {
    let project = tempfile::tempdir().expect("project");
    let victim = tempfile::tempdir().expect("victim");
    let refs = NativeWriteDir::open(project.path(), Path::new(SKILL_REFS_DIRNAME))
        .expect("the skill-refs dir validates");

    swap_for_symlink(refs.path(), victim.path());

    let err = super::super::skill_refs::materialize_skill_refs(&refs)
        .expect_err("a swapped component must be reported");

    assert!(
        matches!(err, WriteTargetError::Unpinned { .. }),
        "expected Unpinned, got {err:?}"
    );
    assert!(
        entries(victim.path()).is_empty(),
        "the victim must receive nothing, found {:?}",
        entries(victim.path())
    );
}

/// #7779: the agents directory swapped AFTER validation publishes nothing into
/// the victim and reports the swap.
///
/// Why: the same race on the older guard at the roster-deploy site. The shared
/// deployer writes through plain paths, so the publish back into the project is
/// the step that has to resolve against the pinned descriptor.
/// What: validates by opening the pinned handle, performs the swap, then
/// publishes a deploy result. Asserts the victim is empty and the error is
/// `WriteTargetError::Unpinned`.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn swapped_agents_dir_never_reaches_the_victim() {
    let project = tempfile::tempdir().expect("project");
    let victim = tempfile::tempdir().expect("victim");
    let agents = NativeWriteDir::open(project.path(), Path::new(AGENTS_DIRNAME))
        .expect("the agents dir validates");

    let scratch = tempfile::tempdir().expect("scratch");
    std::fs::write(scratch.path().join("pm.md"), "composed body").expect("stage");
    std::fs::write(scratch.path().join(MANIFEST_FILE), "{}").expect("stage");
    let mut result = DeployResult::default();
    result.deployed.push("pm.md".to_string());

    swap_for_symlink(agents.path(), victim.path());

    let err = publish_from_scratch(&agents, scratch.path(), &result)
        .expect_err("a swapped component must be reported");

    assert!(
        matches!(
            err,
            RosterDeployError::WriteTarget(WriteTargetError::Unpinned { .. })
        ),
        "expected Unpinned, got {err:?}"
    );
    assert!(
        entries(victim.path()).is_empty(),
        "the victim must receive nothing, found {:?}",
        entries(victim.path())
    );
}
