//! Tests for `deploy_validate` — split out to keep `mod.rs` under the
//! 500-line production SLOC cap enforced by `scripts/check_line_cap.sh`.
//!
//! Why: `mod.rs`'s inline `#[cfg(test)] mod tests { ... }` block pushed the
//! file to 549 SLOC once the #3556 `AgentFrontmatterInvalid` probe and its
//! two regression tests landed; the cap counts by filename pattern, not by
//! `#[cfg(test)]`, so an inline test module still counts against the 500 prod
//! cap. Extracted verbatim (behavior-preserving, no logic changes) following
//! the same `builder.rs`/`builder_tests.rs` and `deployer.rs`/`deployer_tests.rs`
//! split precedent already established in `trusty-agents-common`.
//! What: every unit test for [`super::validate_workspace`] and
//! [`super::validate_and_repair`] — the agent/skill/settings gap probes, the
//! #2171 expected-set fallback tiers, the #3556 strict-frontmatter probe, and
//! the repair round-trip.
//! Test: this file IS the test module for `deploy_validate`; run with
//! `cargo test -p trusty-mpm -- core::deploy_validate`.

use super::*;
use tempfile::TempDir;

/// The production entry point, with a stable hook binary pinned for every case
/// in this file.
///
/// Why (#7244): the repair pipeline writes the project's hooks, which refuses a
/// build-artifact binary — and a test process is one. A CI runner has no
/// installed `tm` for the PATH fallback, so `HooksMissing` stayed a gap and a
/// repair could never report complete.
/// What: shadows [`super::validate_and_repair`] with the identical signature,
/// delegating to [`super::validate_and_repair_with_exe`] with
/// [`crate::test_support::STABLE_HOOK_EXE`].
/// Test: `repair_closes_gaps_on_incomplete_workspace`.
fn validate_and_repair(
    fw: &FrameworkPaths,
    workspace: &Path,
    repo_url: Option<&str>,
) -> RepairOutcome {
    super::validate_and_repair_with_exe(
        fw,
        workspace,
        repo_url,
        Some(Path::new(crate::test_support::STABLE_HOOK_EXE)),
    )
}

/// RAII guard restoring `$HOME` on drop (including panic) — mirrors the
/// identical pattern in `core::standalone::load::tests::HomeGuard` and
/// `session_launch::tests::EnvVarGuard`.
///
/// Why (#3965): [`validate_and_repair`] falls through to
/// `session_launch::prepare_session_with_repo_url`, which seeds
/// `$HOME/.claude.json` via the REAL process `$HOME` (`preseed_workspace_trust_home`),
/// not via the `fw`/`workspace` parameters this function takes. Pairs with
/// `#[serial_test::serial]` so this can never write into the operator's real
/// `~/.claude.json` or race a sibling test doing the same.
/// Test: used by `repair_closes_gaps_on_incomplete_workspace`.
struct HomeGuard(Option<String>);
impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: paired with `#[serial_test::serial]` — no other thread
        // reads/writes the environment concurrently.
        match self.0 {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// Build a hermetic `FrameworkPaths` whose SOURCE dirs are seeded and
/// whose `trusty_mpm_root` is forced to `None`, mirroring the pattern
/// `doctor_fs_checks.rs` uses so the resolution never escapes the temp dir
/// into the real checkout the test binary happens to run inside.
fn hermetic_paths(base: &Path) -> FrameworkPaths {
    let mut fw = FrameworkPaths::under(base);
    fw.trusty_mpm_root = None;
    fw
}

fn seed_agent_source(fw: &FrameworkPaths, names: &[&str]) {
    std::fs::create_dir_all(&fw.agents).unwrap();
    for name in names {
        std::fs::write(
            fw.agents.join(format!("{name}.md")),
            format!("---\nname: {name}\ndescription: d\n---\n\nBody.\n"),
        )
        .unwrap();
    }
}

/// The tier bundled skills actually deploy to since #6586 — the managed config
/// dir's `skills` sibling, which is what `validate_skills` probes.
///
/// Why: these fixtures used to build "a complete workspace" by writing the
/// bundled roster into `fw.claude_skills_dir()`. Bundled skills are user-tier
/// only now, so that directory is no longer where completeness is decided.
fn managed_skills_dir(fw: &FrameworkPaths) -> std::path::PathBuf {
    fw.skill_deploy_dir()
}

fn seed_skill_source(fw: &FrameworkPaths, names: &[&str]) {
    std::fs::create_dir_all(&fw.skills).unwrap();
    for name in names {
        std::fs::write(fw.skills.join(format!("{name}.md")), "skill body").unwrap();
    }
}

fn write_settings(fw: &FrameworkPaths, json: &str) {
    let claude_dir = fw.claude_home_dir().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("settings.json"), json).unwrap();
}

fn deploy_style_file(fw: &FrameworkPaths) {
    let style_dir = fw.claude_home_dir().join(".claude").join("output-styles");
    std::fs::create_dir_all(&style_dir).unwrap();
    let default = OUTPUT_STYLES[0];
    std::fs::write(style_dir.join(default.file_name), default.content).unwrap();
}

/// A minimal, deterministic agent-manifest entry for the entries the
/// #2171 fallback tests record directly (no real deploy pipeline run).
fn sample_agent_entry(source_name: &str) -> crate::core::agent_manifest::ManifestEntry {
    crate::core::agent_manifest::ManifestEntry {
        source_chain: vec![source_name.to_string()],
        checksum: agent_manifest::checksum("agent"),
        deployed_at: "2026-01-01T00:00:00Z".to_string(),
        origin: crate::core::agent_manifest::Origin::Bundled,
    }
}

/// A fully-provisioned workspace matching everything `prepare_session`
/// would have written, used as the positive-path baseline every negative
/// test starts from and mutates one field of.
fn fully_provisioned(base: &Path) -> FrameworkPaths {
    let fw = hermetic_paths(base);
    seed_agent_source(&fw, &["engineer", "BASE-AGENT"]);
    seed_skill_source(&fw, &["tm-doctor"]);

    let agents_dir = fw.agent_deploy_dir();
    std::fs::create_dir_all(&agents_dir).unwrap();
    // #3556: deployed fixtures must carry real (valid) frontmatter now
    // that `validate_agents` strict-YAML-checks every present file —
    // a bare content string with no frontmatter at all is itself a gap.
    std::fs::write(
        agents_dir.join("engineer.md"),
        "---\nname: engineer\n---\n\nagent\n",
    )
    .unwrap();
    std::fs::write(
        agents_dir.join("BASE-AGENT.md"),
        "---\nname: base-agent\n---\n\nbase\n",
    )
    .unwrap();
    AgentManifest::default().save(&agents_dir).unwrap();

    let skills_dir = managed_skills_dir(&fw);
    std::fs::create_dir_all(skills_dir.join("tm-doctor")).unwrap();
    std::fs::write(skills_dir.join("tm-doctor").join("SKILL.md"), "skill").unwrap();
    crate::core::skill_manifest::SkillManifest::default()
        .save(&skills_dir)
        .unwrap();

    deploy_style_file(&fw);
    write_settings(
        &fw,
        r#"{"outputStyle": "trusty-mpm", "hooks": {"SessionStart": []}}"#,
    );
    fw
}

#[test]
fn validate_complete_workspace_has_no_gaps() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    let report = validate_workspace(&fw);
    assert!(
        report.is_complete(),
        "expected no gaps, got: {:?}",
        report.gaps
    );
}

#[test]
fn validate_filtered_but_manifest_matching_workspace_has_no_gaps() {
    // #2171: a workspace provisioned from a FILTERED per-project roster
    // (a project manifest override excludes the generic `engineer`
    // catch-all; only `rust-engineer` is deployed, matching its own
    // ownership manifest exactly) must validate COMPLETE. The pre-fix
    // validator diffed against the unconditional full bundled roster and
    // falsely reported `engineer` missing even though it was never
    // supposed to be deployed here.
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
    fw.trusty_mpm_root = None;

    seed_agent_source(&fw, &["engineer", "rust-engineer"]);
    seed_skill_source(&fw, &["tm-doctor"]);

    // #4832: the project manifest layer lives in `.trusty-mpm/framework/`.
    let manifest_dir = workspace.join(".trusty-mpm").join("framework");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.toml"),
        "[agents]\nexclude = [\"engineer\"]\n",
    )
    .unwrap();

    let agents_dir = fw.agent_deploy_dir();
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("rust-engineer.md"),
        "---\nname: rust-engineer\n---\n\nagent\n",
    )
    .unwrap();
    let mut agent_manifest = AgentManifest::default();
    agent_manifest.managed.insert(
        "rust-engineer.md".to_string(),
        sample_agent_entry("rust-engineer"),
    );
    agent_manifest.save(&agents_dir).unwrap();

    let skills_dir = managed_skills_dir(&fw);
    std::fs::create_dir_all(skills_dir.join("tm-doctor")).unwrap();
    std::fs::write(skills_dir.join("tm-doctor").join("SKILL.md"), "skill").unwrap();
    crate::core::skill_manifest::SkillManifest::default()
        .save(&skills_dir)
        .unwrap();

    deploy_style_file(&fw);
    write_settings(
        &fw,
        r#"{"outputStyle": "trusty-mpm", "hooks": {"SessionStart": []}}"#,
    );

    let report = validate_workspace(&fw);
    assert!(
        report.is_complete(),
        "filtered-but-complete workspace must validate as complete, got: {:?}",
        report.gaps
    );
}

#[test]
fn validate_entry_missing_on_disk_but_in_manifest_is_still_a_gap() {
    // The plan's bundled source directory is unpopulated (a binary-only
    // install with no `framework/agents` yet), so expected-set resolution
    // falls through to tier (b): the workspace's own deployed manifest.
    // An entry the manifest claims to manage but which is NOT actually
    // present on disk must still be reported — the fallback must never
    // suppress a genuine gap, and an entry that IS on disk must not be
    // falsely flagged.
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
    fw.trusty_mpm_root = None;
    // No seed_agent_source call — the plan's bundled source stays empty.

    let agents_dir = fw.agent_deploy_dir();
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("rust-engineer.md"), "agent").unwrap();
    let mut agent_manifest = AgentManifest::default();
    agent_manifest.managed.insert(
        "rust-engineer.md".to_string(),
        sample_agent_entry("rust-engineer"),
    );
    agent_manifest.managed.insert(
        "python-engineer.md".to_string(),
        sample_agent_entry("python-engineer"),
    );
    agent_manifest.save(&agents_dir).unwrap();

    let report = validate_workspace(&fw);
    assert!(
        report
            .gaps
            .contains(&DeploymentGap::AgentMissing("python-engineer".to_string())),
        "manifest-recorded entry missing on disk must still be reported, got: {:?}",
        report.gaps
    );
    assert!(
        !report
            .gaps
            .contains(&DeploymentGap::AgentMissing("rust-engineer".to_string())),
        "the entry that IS on disk must not be falsely reported missing, got: {:?}",
        report.gaps
    );
}

#[test]
fn validate_stale_broken_frontmatter_is_a_gap() {
    // Issue #3556: a deployed agent that predates the quote-on-emit
    // composer fix — its `description:` is an unquoted plain scalar
    // containing a colon — must be surfaced as a gap even though the
    // FILE is present (the pre-#3556 validator only checked existence,
    // so this reproduced-in-production case slipped through silently).
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::write(
        fw.agent_deploy_dir().join("engineer.md"),
        "---\nname: engineer\ndescription: Rust 2024 edition specialist: memory-safe systems\n---\n\nBody.\n",
    )
    .unwrap();

    let report = validate_workspace(&fw);
    assert!(
        report.gaps.iter().any(|g| matches!(
            g,
            DeploymentGap::AgentFrontmatterInvalid(name, _) if name == "engineer"
        )),
        "expected an AgentFrontmatterInvalid gap for `engineer`, got: {:?}",
        report.gaps
    );
}

#[test]
fn validate_well_formed_agent_is_not_flagged() {
    // Negative case for the #3556 probe: a deployed agent whose
    // description contains a colon but is PROPERLY quoted must not be
    // flagged — the check is strict-YAML-validity, not "no colons
    // allowed".
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::write(
        fw.agent_deploy_dir().join("engineer.md"),
        "---\nname: engineer\ndescription: \"Rust 2024 edition specialist: memory-safe systems\"\n---\n\nBody.\n",
    )
    .unwrap();

    let report = validate_workspace(&fw);
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| matches!(g, DeploymentGap::AgentFrontmatterInvalid(..))),
        "properly quoted frontmatter must not be flagged, got: {:?}",
        report.gaps
    );
}

#[test]
fn validate_missing_agent_manifest_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::remove_file(fw.agent_deploy_dir().join(agent_manifest::MANIFEST_FILE)).unwrap();
    let report = validate_workspace(&fw);
    assert!(report.gaps.contains(&DeploymentGap::AgentManifestMissing));
}

#[test]
fn validate_missing_agent_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::remove_file(fw.agent_deploy_dir().join("engineer.md")).unwrap();
    let report = validate_workspace(&fw);
    assert!(
        report
            .gaps
            .contains(&DeploymentGap::AgentMissing("engineer".to_string()))
    );
}

#[test]
fn validate_missing_skill_manifest_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::remove_file(managed_skills_dir(&fw).join(skill_manifest::SKILL_MANIFEST_FILE))
        .unwrap();
    let report = validate_workspace(&fw);
    assert!(report.gaps.contains(&DeploymentGap::SkillManifestMissing));
}

#[test]
fn validate_missing_skill_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::remove_dir_all(managed_skills_dir(&fw).join("tm-doctor")).unwrap();
    let report = validate_workspace(&fw);
    assert!(
        report
            .gaps
            .contains(&DeploymentGap::SkillMissing("tm-doctor".to_string()))
    );
}

#[test]
fn validate_settings_missing_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::remove_file(fw.claude_home_dir().join(".claude").join("settings.json")).unwrap();
    let report = validate_workspace(&fw);
    assert!(report.gaps.contains(&DeploymentGap::SettingsMissing));
}

#[test]
fn validate_missing_output_style_key_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    write_settings(&fw, r#"{"hooks": {"SessionStart": []}}"#);
    let report = validate_workspace(&fw);
    assert!(report.gaps.contains(&DeploymentGap::OutputStyleKeyMissing));
}

#[test]
fn validate_unknown_output_style_id_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    write_settings(
        &fw,
        r#"{"outputStyle": "claude_mpm", "hooks": {"SessionStart": []}}"#,
    );
    let report = validate_workspace(&fw);
    assert!(report.gaps.contains(&DeploymentGap::OutputStyleUnknownId(
        "claude_mpm".to_string()
    )));
}

#[test]
fn validate_output_style_file_missing_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    let style_dir = fw.claude_home_dir().join(".claude").join("output-styles");
    std::fs::remove_dir_all(&style_dir).unwrap();
    let report = validate_workspace(&fw);
    assert!(report.gaps.contains(&DeploymentGap::OutputStyleFileMissing(
        "trusty-mpm".to_string()
    )));
}

#[test]
fn validate_missing_hooks_is_a_gap() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    write_settings(&fw, r#"{"outputStyle": "trusty-mpm"}"#);
    let report = validate_workspace(&fw);
    assert!(report.gaps.contains(&DeploymentGap::HooksMissing));
}

#[test]
fn describe_is_non_empty_for_every_variant() {
    let gaps = [
        DeploymentGap::AgentManifestMissing,
        DeploymentGap::AgentManifestCorrupt("bad".to_string()),
        DeploymentGap::AgentMissing("engineer".to_string()),
        DeploymentGap::AgentFrontmatterInvalid("engineer".to_string(), "bad".to_string()),
        DeploymentGap::SkillManifestMissing,
        DeploymentGap::SkillMissing("tm-doctor".to_string()),
        DeploymentGap::SettingsMissing,
        DeploymentGap::SettingsMalformed("bad json".to_string()),
        DeploymentGap::OutputStyleKeyMissing,
        DeploymentGap::OutputStyleUnknownId("x".to_string()),
        DeploymentGap::OutputStyleFileMissing("x".to_string()),
        DeploymentGap::HooksMissing,
        // #7849
        DeploymentGap::ProjectHookGroupMissing("Stop".to_string(), "tm hook --x".to_string()),
        DeploymentGap::ProjectHookGroupStale("Stop".to_string(), "tm hook --x".to_string()),
    ];
    for gap in gaps {
        assert!(!gap.describe().is_empty());
    }
}

#[test]
fn repair_is_a_noop_on_already_complete_workspace() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    let outcome = validate_and_repair(&fw, &fw.claude_home_dir(), None);
    assert!(!outcome.repaired);
    assert!(outcome.is_complete());
    assert_eq!(outcome.before, outcome.after);
}

#[test]
#[serial_test::serial]
fn repair_closes_gaps_on_incomplete_workspace() {
    // Seed only the framework SOURCE roster (what `prepare_session_inner`
    // would deploy from) but leave the workspace `.claude/` entirely
    // unprovisioned — the exact "spawned with an incomplete payload"
    // scenario #2158 describes. The repair path must deploy everything
    // and leave the workspace complete.
    // #3965: `#[serial]` + `$HOME` override — see `HomeGuard` above.
    let fake_home = TempDir::new().unwrap();
    let _home_guard = {
        let prior = std::env::var("HOME").ok();
        // SAFETY: serialized via `#[serial_test::serial]`.
        unsafe { std::env::set_var("HOME", fake_home.path()) };
        HomeGuard(prior)
    };
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
    let mut fw = fw;
    fw.trusty_mpm_root = None;
    seed_agent_source(&fw, &["engineer"]);
    seed_skill_source(&fw, &["tm-doctor"]);

    let before = validate_workspace(&fw);
    assert!(!before.is_complete(), "expected gaps before repair");

    let outcome = validate_and_repair(&fw, &workspace, None);
    assert!(outcome.repaired);
    assert!(
        outcome.is_complete(),
        "expected repair to close every gap, remaining: {:?}",
        outcome.after.gaps
    );
}

#[test]
#[serial_test::serial]
fn repair_is_not_reported_when_the_pipeline_fails_fatally() {
    // Why (#4781): `repaired` was set unconditionally on the repair path, so a
    // repair that was REFUSED still reported `repaired: true` next to a
    // `repair_error` — a caller reading the flag alone concluded the workspace
    // had been fixed while it was left exactly as broken as it was found.
    //
    // FIXTURE: the same one `prepare_session_refuses_when_the_instructions_cannot_be_built`
    // uses — a directory planted where `<workspace>/CLAUDE.md` goes, so the
    // instruction pipeline's load-or-create step fails and
    // `prepare_session_with_repo_url` returns the fatal `PrepError::Instructions`
    // (#4752) before settings/output-style/hooks are ever written.
    // #3965: `#[serial]` + `$HOME` override — see `HomeGuard` above.
    let fake_home = TempDir::new().unwrap();
    let _home_guard = {
        let prior = std::env::var("HOME").ok();
        // SAFETY: serialized via `#[serial_test::serial]`.
        unsafe { std::env::set_var("HOME", fake_home.path()) };
        HomeGuard(prior)
    };
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
    fw.trusty_mpm_root = None;
    seed_agent_source(&fw, &["engineer"]);
    seed_skill_source(&fw, &["tm-doctor"]);
    std::fs::create_dir_all(workspace.join("CLAUDE.md"))
        .expect("plant a directory where CLAUDE.md goes");

    assert!(
        !validate_workspace(&fw).is_complete(),
        "fixture must start incomplete so the repair path runs"
    );

    let outcome = validate_and_repair(&fw, &workspace, None);

    assert!(
        outcome.repair_error.is_some(),
        "the planted directory must make the repair pipeline fail"
    );
    assert!(
        !outcome.is_complete(),
        "a refused repair leaves gaps, remaining: {:?}",
        outcome.after.gaps
    );
    assert!(
        !outcome.repaired,
        "a repair that failed fatally must NOT report repaired: true (error was {:?})",
        outcome.repair_error
    );
}

#[test]
#[serial_test::serial]
fn repair_closes_a_managed_tier_bundled_skill_gap() {
    // Why (#6586): the probe and the repair had to move to the same tier in one
    // step, and this test is what proves they arrived. `validate_skills` reads
    // `fw.skill_deploy_dir()` — the managed user tier, where bundled skills live
    // since the 2026-09-01 owner ruling. `validate_and_repair` repairs by
    // calling `session_launch::prepare_session_with_repo_url`, which until #6586
    // wrote only the project tier. Probe one tier, repair another, and every gap
    // the probe reports is unrepairable: the daemon's spawn gate re-runs the
    // repair on every launch and never converges.
    //
    // FIXTURE: a workspace complete in every other respect with exactly the
    // managed-tier bundled skill removed, so the gap under test is the only one
    // and a green result cannot come from some other probe passing.
    // #3965: `#[serial]` + `$HOME` override — see `HomeGuard` above.
    let fake_home = TempDir::new().unwrap();
    let _home_guard = {
        let prior = std::env::var("HOME").ok();
        // SAFETY: serialized via `#[serial_test::serial]`.
        unsafe { std::env::set_var("HOME", fake_home.path()) };
        HomeGuard(prior)
    };
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    let workspace = fw.claude_home_dir();
    assert!(
        validate_workspace(&fw).is_complete(),
        "fixture precondition: the workspace starts complete"
    );

    std::fs::remove_dir_all(fw.skill_deploy_dir().join("tm-doctor")).unwrap();

    let before = validate_workspace(&fw);
    assert_eq!(
        before.gaps,
        vec![DeploymentGap::SkillMissing("tm-doctor".to_string())],
        "removing the managed-tier copy must be the ONLY gap reported"
    );

    let outcome = validate_and_repair(&fw, &workspace, None);

    assert!(
        outcome.repaired,
        "the repair must close a managed-tier skill gap (error: {:?}, remaining: {:?})",
        outcome.repair_error, outcome.after.gaps
    );
    assert!(outcome.is_complete(), "remaining: {:?}", outcome.after.gaps);
    assert!(
        fw.skill_deploy_dir()
            .join("tm-doctor")
            .join("SKILL.md")
            .is_file(),
        "the repair must rewrite the skill at the managed tier: {}",
        fw.skill_deploy_dir().display()
    );
    assert!(
        !fw.claude_skills_dir().join("tm-doctor").exists(),
        "the repair must not put a bundled skill back in the project tier"
    );
}

#[test]
fn a_stray_project_tier_bundled_skill_does_not_satisfy_completeness() {
    // Why (#6586): an older binary deployed every bundled skill to the project's
    // own `.claude/skills/` as well. Those copies are frozen — no deploy reaches
    // them any more — so counting one as "the skill is deployed" would report a
    // workspace complete while the tier that actually loads holds nothing. `tm
    // doctor`'s `skill_project_tier` check reports the stray; completeness must
    // keep reading only the managed tier.
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::remove_dir_all(fw.skill_deploy_dir().join("tm-doctor")).unwrap();

    // Plant the stray exactly where the pre-#6586 deploy would have left it.
    let stray = fw.claude_skills_dir().join("tm-doctor");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("SKILL.md"), "frozen copy from an older install").unwrap();

    let report = validate_workspace(&fw);
    assert!(
        report
            .gaps
            .contains(&DeploymentGap::SkillMissing("tm-doctor".to_string())),
        "a stray project-tier copy must not satisfy completeness, gaps: {:?}",
        report.gaps
    );
}

/// Repair an unprovisioned workspace with `memory_reachable` pinned, and report
/// the `autoMemoryEnabled` key the preparation left in the project settings.
///
/// Why (#7763): the threaded verdict has exactly one observable effect —
/// `prepare_session_inner` writes `autoMemoryEnabled: !memory_reachable`. Reading
/// that key back is what distinguishes a repair that reused the launch's answer
/// from one that re-probed the host and wrote the probe's answer instead.
/// What: the `repair_closes_gaps_on_incomplete_workspace` fixture (framework
/// SOURCE roster seeded, workspace `.claude/` empty) repaired through
/// [`super::validate_and_repair_reusing_memory`]. The caller owns the `$HOME`
/// override and the `#[serial_test::serial]` attribute.
/// Test: `repair_reuses_the_reachability_the_launch_resolved`.
fn repaired_auto_memory_key(base: &Path, memory_reachable: bool) -> serde_json::Value {
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut fw = FrameworkPaths::for_managed_project(base, &workspace);
    fw.trusty_mpm_root = None;
    seed_agent_source(&fw, &["engineer"]);
    seed_skill_source(&fw, &["tm-doctor"]);
    assert!(
        !validate_workspace(&fw).is_complete(),
        "fixture must start incomplete so the repair pipeline runs"
    );

    super::validate_and_repair_reusing_memory(&fw, &workspace, None, Some(memory_reachable));

    let settings = workspace.join(".claude").join("settings.json");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    value["autoMemoryEnabled"].clone()
}

/// THE #7763 REGRESSION TEST — fails on the pre-fix code.
///
/// Why: the daemon's spawn gate runs this repair right after the same launch
/// probed trusty-memory, and the repair re-ran the whole preparation pipeline
/// with nothing threaded — so it probed again and a slow or wedged daemon cost
/// the launch two `PROBE_TIMEOUT` budgets. Before the fix the repair ignored the
/// pinned verdict entirely, so BOTH runs below wrote whatever the host's own
/// probe said and the two results were equal on every host.
/// What: repairs two identical fixtures, one pinned reachable and one pinned
/// unreachable, and asserts each wrote the auto-memory key its own verdict
/// implies.
/// Test: itself.
#[test]
#[serial_test::serial]
fn repair_reuses_the_reachability_the_launch_resolved() {
    // #3965: `#[serial]` + `$HOME` override — see `HomeGuard` above.
    let fake_home = TempDir::new().unwrap();
    let _home_guard = {
        let prior = std::env::var("HOME").ok();
        // SAFETY: serialized via `#[serial_test::serial]`.
        unsafe { std::env::set_var("HOME", fake_home.path()) };
        HomeGuard(prior)
    };
    let reachable_base = TempDir::new().unwrap();
    let unreachable_base = TempDir::new().unwrap();

    let reachable = repaired_auto_memory_key(reachable_base.path(), true);
    let unreachable = repaired_auto_memory_key(unreachable_base.path(), false);

    assert_eq!(
        reachable,
        serde_json::json!(false),
        "a repair told trusty-memory is up must turn the auto-memory fallback OFF"
    );
    assert_eq!(
        unreachable,
        serde_json::json!(true),
        "a repair told trusty-memory is down must leave the auto-memory fallback ON"
    );
}

// ---------------------------------------------------------------------------
// #7849 — toggle-driven project hook groups.
// ---------------------------------------------------------------------------

/// The stable installed-looking binary every #7849 fixture pins.
///
/// Why: both the writer and the validator resolve the hook command through
/// `resolve_stable_hook_exe`, which refuses the test binary's own build-tree
/// path. Pinning one path makes the two agree without asking whether the host
/// running them has `tm` installed.
/// What: [`crate::test_support::STABLE_HOOK_EXE`] as a `&Path`.
/// Test: every `#7849` test below.
fn stable_exe() -> &'static Path {
    Path::new(crate::test_support::STABLE_HOOK_EXE)
}

/// A hermetic, fully-provisioned PROJECT — `claude_home_dir()` IS the workspace,
/// so the file `validate_settings` reads is the one the hook writer writes.
///
/// Why (#7849): `fully_provisioned` puts the settings file at the framework
/// base, which is not a project directory, so the toggle resolution
/// (`.trusty-mpm.toml`) has nowhere to live. This mirrors
/// `validate_filtered_but_manifest_matching_workspace_has_no_gaps`'s layout
/// instead: `for_managed_project(base, workspace)`.
/// What: the same roster/style/settings seeding `fully_provisioned` does, minus
/// the hooks — the caller writes those through the launch-path writer.
/// Test: `repair_adds_the_prompt_feedback_groups_after_the_flag_flips_on`.
fn provisioned_project(base: &Path) -> (FrameworkPaths, std::path::PathBuf) {
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut fw = FrameworkPaths::for_managed_project(base, &workspace);
    fw.trusty_mpm_root = None;

    seed_agent_source(&fw, &["engineer", "BASE-AGENT"]);
    seed_skill_source(&fw, &["tm-doctor"]);

    let agents_dir = fw.agent_deploy_dir();
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("engineer.md"),
        "---\nname: engineer\n---\n\nagent\n",
    )
    .unwrap();
    std::fs::write(
        agents_dir.join("BASE-AGENT.md"),
        "---\nname: base-agent\n---\n\nbase\n",
    )
    .unwrap();
    AgentManifest::default().save(&agents_dir).unwrap();

    let skills_dir = managed_skills_dir(&fw);
    std::fs::create_dir_all(skills_dir.join("tm-doctor")).unwrap();
    std::fs::write(skills_dir.join("tm-doctor").join("SKILL.md"), "skill").unwrap();
    crate::core::skill_manifest::SkillManifest::default()
        .save(&skills_dir)
        .unwrap();

    deploy_style_file(&fw);
    write_settings(&fw, r#"{"outputStyle": "trusty-mpm"}"#);
    (fw, workspace)
}

/// Write the committed project flag, which outranks the host default.
///
/// Why: `prompt_self_improvement::enabled_for` falls back to
/// `~/.trusty-mpm/config.toml` when the project declines, so a fixture that
/// simply omitted the key would read the operator's machine. Stating it here
/// removes the `$HOME` dependency in both directions.
/// What: `.trusty-mpm.toml` carrying `prompt_self_improvement = <on>`.
/// Test: `repair_adds_the_prompt_feedback_groups_after_the_flag_flips_on`.
fn write_project_flag(workspace: &Path, on: bool) {
    std::fs::write(
        workspace.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        format!("prompt_self_improvement = {on}\n"),
    )
    .unwrap();
}

/// Read `<workspace>/.claude/settings.json`.
fn read_project_settings(workspace: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(workspace.join(".claude").join("settings.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The hook events carrying a `--prompt-feedback` capture group, sorted.
fn prompt_feedback_events(workspace: &Path) -> Vec<String> {
    let val = read_project_settings(workspace);
    let Some(hooks) = val.get("hooks").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut events: Vec<String> = hooks
        .iter()
        .filter(|(_, groups)| {
            groups.as_array().is_some_and(|groups| {
                groups.iter().any(|group| {
                    group
                        .get("hooks")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|inner| {
                            inner.iter().any(|entry| {
                                entry
                                    .get("command")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|c| c.ends_with(" hook --prompt-feedback"))
                            })
                        })
                })
            })
        })
        .map(|(event, _)| event.clone())
        .collect();
    events.sort();
    events
}

/// Seed a project whose settings file the launch-path writer produced with the
/// flag OFF, then flip the flag ON — the #7849 incident shape.
fn project_with_the_flag_flipped_on(base: &Path) -> (FrameworkPaths, std::path::PathBuf) {
    let (fw, workspace) = provisioned_project(base);
    write_project_flag(&workspace, false);
    crate::core::session_launch::ensure_project_hooks_with(&fw, &workspace, Some(stable_exe()))
        .expect("the launch-path writer must succeed against a pinned installed binary");
    assert!(
        prompt_feedback_events(&workspace).is_empty(),
        "the flag was off, so no capture group may have been written"
    );
    write_project_flag(&workspace, true);
    (fw, workspace)
}

/// THE #7849 REGRESSION TEST — fails on the pre-fix code.
///
/// Why: `validate_settings` only asked whether `hooks` was present and
/// non-empty, so a project whose `prompt_self_improvement` flipped on after its
/// settings file was written reported "no gaps found" and `--repair` rewrote
/// nothing. The capture never registered until the next fresh session.
/// What: seeds the incident shape, runs `--repair`, and asserts the two capture
/// groups landed and the workspace validates complete.
/// Test: itself.
#[test]
fn repair_adds_the_prompt_feedback_groups_after_the_flag_flips_on() {
    let tmp = TempDir::new().unwrap();
    let (fw, workspace) = project_with_the_flag_flipped_on(tmp.path());

    let outcome = super::validate_and_repair_with_exe(&fw, &workspace, None, Some(stable_exe()));

    assert!(
        !outcome.before.is_complete(),
        "the flag flip must be reported as a gap, got: {:?}",
        outcome.before.gaps
    );
    assert_eq!(
        prompt_feedback_events(&workspace),
        vec!["Stop".to_string(), "SubagentStop".to_string()],
        "--repair must add exactly the two capture groups"
    );
    assert!(
        outcome.is_complete(),
        "the repair must close the gap it found, left: {:?}",
        outcome.after.gaps
    );
}

/// A second `--repair` changes nothing.
///
/// Why: the spawn/resume gate calls this on every launch; a repair that
/// rewrote the file each time would snapshot on every session and push the one
/// prior state worth keeping out of the archive (#7244's lesson).
/// What: repairs twice and compares the file bytes across the second call.
/// Test: itself.
#[test]
fn a_second_repair_leaves_the_settings_file_byte_identical() {
    let tmp = TempDir::new().unwrap();
    let (fw, workspace) = project_with_the_flag_flipped_on(tmp.path());
    let path = workspace.join(".claude").join("settings.json");

    super::validate_and_repair_with_exe(&fw, &workspace, None, Some(stable_exe()));
    let after_first = std::fs::read(&path).unwrap();

    let outcome = super::validate_and_repair_with_exe(&fw, &workspace, None, Some(stable_exe()));
    assert!(
        outcome.before.is_complete(),
        "the second run must find nothing to do, got: {:?}",
        outcome.before.gaps
    );
    assert_eq!(
        after_first,
        std::fs::read(&path).unwrap(),
        "a second --repair must not rewrite the file"
    );
}

/// A hook-group repair preserves every foreign entry and every foreign key.
///
/// Why: the file is the project's own, not tm's. A resync that dropped a
/// hand-added hook or an unrelated settings key would trade one defect for a
/// worse one.
/// What: plants a foreign `Stop` group and a foreign top-level key, repairs,
/// and asserts both survive alongside the added capture groups.
/// Test: itself.
#[test]
fn a_hook_group_repair_leaves_foreign_entries_untouched() {
    let tmp = TempDir::new().unwrap();
    let (fw, workspace) = project_with_the_flag_flipped_on(tmp.path());
    let path = workspace.join(".claude").join("settings.json");

    let mut val = read_project_settings(&workspace);
    val["permissions"] = serde_json::json!({ "allow": ["Bash(ls:*)"] });
    val["hooks"]["Stop"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": "/opt/other-harness/run --stop" }]
        }));
    std::fs::write(&path, serde_json::to_string_pretty(&val).unwrap()).unwrap();

    super::validate_and_repair_with_exe(&fw, &workspace, None, Some(stable_exe()));

    let after = read_project_settings(&workspace);
    assert_eq!(
        after["permissions"],
        serde_json::json!({ "allow": ["Bash(ls:*)"] }),
        "an unrelated settings key must survive the repair"
    );
    let stop_commands: Vec<String> = after["hooks"]["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|g| g.get("hooks").and_then(serde_json::Value::as_array))
        .flatten()
        .filter_map(|e| e.get("command").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect();
    assert!(
        stop_commands
            .iter()
            .any(|c| c == "/opt/other-harness/run --stop"),
        "the foreign Stop entry must survive: {stop_commands:?}"
    );
}

/// A capture group the flag no longer asks for is reported and removed.
///
/// Why (#7849, the other direction): a stale group keeps firing forever — it
/// spawns a `tm` process at the end of every turn in a project that turned the
/// feature off. The strip domain in the writer already covers it; what was
/// missing was anything that noticed.
/// What: writes the file with the flag ON, flips it OFF, repairs, and asserts
/// the two groups are gone and every other group survives.
/// Test: itself.
#[test]
fn repair_removes_a_stale_prompt_feedback_group_when_the_flag_is_off() {
    let tmp = TempDir::new().unwrap();
    let (fw, workspace) = provisioned_project(tmp.path());
    write_project_flag(&workspace, true);
    crate::core::session_launch::ensure_project_hooks_with(&fw, &workspace, Some(stable_exe()))
        .expect("write with the flag on");
    assert_eq!(prompt_feedback_events(&workspace).len(), 2);
    let before_guard = read_project_settings(&workspace)["hooks"]["PreToolUse"].clone();

    write_project_flag(&workspace, false);
    let outcome = super::validate_and_repair_with_exe(&fw, &workspace, None, Some(stable_exe()));

    assert!(
        outcome.before.gaps.iter().any(|g| matches!(
            g,
            DeploymentGap::ProjectHookGroupStale(event, _) if event == "Stop"
        )),
        "the stale group must be reported, got: {:?}",
        outcome.before.gaps
    );
    assert!(
        prompt_feedback_events(&workspace).is_empty(),
        "--repair must remove the group the flag no longer asks for"
    );
    assert_eq!(
        read_project_settings(&workspace)["hooks"]["PreToolUse"],
        before_guard,
        "only the stale group may be removed"
    );
}

/// A settings file that will not parse is a FAIL, never "no gaps found".
///
/// Why (#7849 fail-open check): the toggle probe reads the parsed value, so a
/// file that never parsed must reach the existing `SettingsMalformed` gap and
/// stop there — silently treating an unreadable file as a complete one is the
/// same defect this issue is about, moved one probe along.
/// What: overwrites the settings file with invalid JSON and asserts the report
/// names it and reports no hook-group gap derived from a value nobody read.
/// Test: itself.
#[test]
fn a_malformed_settings_file_is_a_fail_not_a_silent_pass() {
    let tmp = TempDir::new().unwrap();
    let (fw, workspace) = project_with_the_flag_flipped_on(tmp.path());
    std::fs::write(
        workspace.join(".claude").join("settings.json"),
        "{ not json",
    )
    .unwrap();

    let report = super::validate_workspace_with_exe(&fw, Some(stable_exe()));

    assert!(!report.is_complete(), "a malformed file is never complete");
    assert!(
        report
            .gaps
            .iter()
            .any(|g| matches!(g, DeploymentGap::SettingsMalformed(_))),
        "the parse failure must be named, got: {:?}",
        report.gaps
    );
    assert!(
        !report.gaps.iter().any(DeploymentGap::is_project_hook_group),
        "no hook-group verdict may be derived from a value nobody parsed: {:?}",
        report.gaps
    );
}

/// An unresolvable hook binary leaves the report INCOMPLETE, never clean.
///
/// Why (#7849, fail-open check): the first cut swallowed the resolution error
/// into an empty diff, so a project with real drift validated complete and
/// `tm doctor` stayed silent whenever the running binary could not be resolved
/// — the same symptom this issue is about, gated on the exe instead of on the
/// toggle.
/// What: builds the real drift fixture, then hands the validator a probe that
/// refuses. The probe seam is used rather than a foreign `exe_override`
/// because `resolve_stable_hook_exe` rescues a refused override from `$PATH`
/// and the well-known daemon directories, so a path pin cannot reach this arm
/// on a host that has `tm` installed. Asserts the report is not complete, names
/// the diagnostic, and that every gap routes to the in-place resync — which is
/// what makes `--repair` surface the same refusal as `repair_error`.
/// Test: itself.
#[test]
fn an_unresolvable_hook_binary_is_an_incomplete_diagnostic_never_a_clean_report() {
    let tmp = TempDir::new().unwrap();
    let (fw, _workspace) = project_with_the_flag_flipped_on(tmp.path());

    let report = super::validate_workspace_with_probe(&fw, &|_, _, _| {
        Err(crate::core::standalone::hooks::StableHookExeError::Unresolved)
    });

    assert!(
        !report.is_complete(),
        "an unverifiable hook set must never read as complete"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| matches!(g, DeploymentGap::ProjectHookDiagnosticIncomplete(_))),
        "the resolution failure must be named, got: {:?}",
        report.gaps
    );
    assert!(
        report.gaps.iter().all(DeploymentGap::is_project_hook_group),
        "every gap must route to the in-place resync, got: {:?}",
        report.gaps
    );
}

/// A writer failure surfaces as a repair error, never as "no gaps found".
///
/// Why (#7849 fail-open check): the whole defect was a silent success. A repair
/// that cannot write must say so — reporting the workspace complete because
/// nothing could be changed is the same failure in a new place.
/// What: seeds the incident shape, makes `.claude/` unwritable so the
/// snapshot-before-write refuses, and asserts the outcome carries an error and
/// stays incomplete.
/// Test: itself.
#[test]
fn a_hook_writer_failure_surfaces_rather_than_reporting_no_gaps() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let (fw, workspace) = project_with_the_flag_flipped_on(tmp.path());
    let claude_dir = workspace.join(".claude");

    let original = std::fs::metadata(&claude_dir).unwrap().permissions();
    std::fs::set_permissions(&claude_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let outcome = super::validate_and_repair_with_exe(&fw, &workspace, None, Some(stable_exe()));
    std::fs::set_permissions(&claude_dir, original).unwrap();

    assert!(
        !outcome.before.is_complete(),
        "the gap must still be reported, got: {:?}",
        outcome.before.gaps
    );
    assert!(
        outcome.repair_error.is_some(),
        "a refused write must surface as a repair error"
    );
    assert!(
        !outcome.is_complete() && !outcome.repaired,
        "a refused write must never report the workspace repaired"
    );
}
