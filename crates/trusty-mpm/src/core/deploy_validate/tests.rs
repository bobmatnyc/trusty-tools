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

/// The tier bundled skills actually deploy to since #6586 — see
/// [`FrameworkPaths::managed_skills_dir`], which is what `validate_skills`
/// probes.
///
/// Why: these fixtures used to build "a complete workspace" by writing the
/// bundled roster into `fw.claude_skills_dir()`. Bundled skills are user-tier
/// only now, so that directory is no longer where completeness is decided.
fn managed_skills_dir(fw: &FrameworkPaths) -> std::path::PathBuf {
    fw.managed_skills_dir()
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

/// Why (#6586): the probe moved to the managed tier, and the failure mode worth
/// pinning is the mirror image of the one that moved it — a project-tier copy
/// standing in for the managed one. If `validate_skills` accepted either, a
/// workspace whose managed tier was never provisioned would read complete
/// because a leftover copy from an older binary happened to sit in the project.
/// What: removes the managed-tier skill, plants the same stem in the project
/// tier, and asserts the gap is still reported.
/// Test: this function IS the test.
#[test]
fn validate_ignores_a_bundled_skill_in_the_project_tier() {
    let tmp = TempDir::new().unwrap();
    let fw = fully_provisioned(tmp.path());
    std::fs::remove_dir_all(managed_skills_dir(&fw).join("tm-doctor")).unwrap();

    let project_copy = fw.claude_skills_dir().join("tm-doctor");
    std::fs::create_dir_all(&project_copy).unwrap();
    std::fs::write(project_copy.join("SKILL.md"), "skill").unwrap();

    let report = validate_workspace(&fw);
    assert!(
        report
            .gaps
            .contains(&DeploymentGap::SkillMissing("tm-doctor".to_string())),
        "a project-tier copy must not satisfy the managed-tier roster: {:?}",
        report.gaps
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
