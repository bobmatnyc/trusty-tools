//! Tests for `tiers` — split out to mirror the `builder`/`builder_tests`
//! pattern and keep `tiers.rs` under the 500-line SLOC cap.
//!
//! Why: moved verbatim from `trusty-mpm::core::skill_tiers`'s inline
//! `#[cfg(test)] mod tests` (#2892, #2818) — behavior-preserving extraction,
//! not a rewrite.
//! What: covers the pure `plan_skill_tiers` precedence table, `resolve_skill_tier`
//! (DOC-42), `list_source_stems` / `list_project_custom_stems`, and the
//! end-to-end `deploy_all_skill_tiers` tier merge over real directories.
//! Test: this file IS the test module for `tiers`; run with
//! `cargo test -p trusty-agents-common -- skills::tiers`.

use super::*;
use std::fs;
use tempfile::TempDir;

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn plan_disjoint_all_deploy() {
    // No collisions: every user and bundled stem deploys, no shadows.
    let plan = plan_skill_tiers(&set(&["p"]), &set(&["u"]), &set(&["b"]));
    assert_eq!(plan.user_deploy, set(&["u"]));
    assert_eq!(plan.bundled_deploy, set(&["b"]));
    assert!(plan.shadowed.is_empty());
}

#[test]
fn plan_user_shadows_bundled() {
    // A user skill of the same name as a bundled one wins; bundled is
    // suppressed and the collision recorded.
    let plan = plan_skill_tiers(&BTreeSet::new(), &set(&["dup"]), &set(&["dup", "only"]));
    assert_eq!(plan.user_deploy, set(&["dup"]));
    assert_eq!(plan.bundled_deploy, set(&["only"]));
    assert_eq!(
        plan.shadowed,
        vec![Shadow {
            stem: "dup".into(),
            winner: SkillTier::User,
            loser: SkillTier::Bundled,
        }]
    );
}

#[test]
fn plan_project_shadows_user_and_bundled() {
    // A project-custom skill outranks BOTH lower tiers of the same name.
    let plan = plan_skill_tiers(&set(&["dup"]), &set(&["dup"]), &set(&["dup"]));
    assert!(plan.user_deploy.is_empty());
    assert!(plan.bundled_deploy.is_empty());
    // Two shadows for "dup": project>user and project>bundled, in that
    // (loser-tier) order.
    assert_eq!(
        plan.shadowed,
        vec![
            Shadow {
                stem: "dup".into(),
                winner: SkillTier::Project,
                loser: SkillTier::User,
            },
            Shadow {
                stem: "dup".into(),
                winner: SkillTier::Project,
                loser: SkillTier::Bundled,
            },
        ]
    );
}

#[test]
fn plan_project_shadows_user_only() {
    // Project shadows user; a disjoint bundled skill still deploys.
    let plan = plan_skill_tiers(&set(&["dup"]), &set(&["dup"]), &set(&["b"]));
    assert!(plan.user_deploy.is_empty());
    assert_eq!(plan.bundled_deploy, set(&["b"]));
    assert_eq!(
        plan.shadowed,
        vec![Shadow {
            stem: "dup".into(),
            winner: SkillTier::Project,
            loser: SkillTier::User,
        }]
    );
}

// ── resolve_skill_tier (DOC-42, issue #2889) ────────────────────────────

#[test]
fn resolve_tier_project_wins() {
    assert_eq!(
        resolve_skill_tier("dup", &set(&["dup"]), &set(&["dup"]), &set(&["dup"])),
        Some(SkillTier::Project)
    );
}

#[test]
fn resolve_tier_user_wins_over_bundled() {
    assert_eq!(
        resolve_skill_tier("dup", &BTreeSet::new(), &set(&["dup"]), &set(&["dup"])),
        Some(SkillTier::User)
    );
}

#[test]
fn resolve_tier_bundled_only() {
    assert_eq!(
        resolve_skill_tier("b", &BTreeSet::new(), &BTreeSet::new(), &set(&["b"])),
        Some(SkillTier::Bundled)
    );
}

#[test]
fn resolve_tier_absent_is_none() {
    // A dangling declared skill — present in no tier — resolves to None.
    assert_eq!(
        resolve_skill_tier(
            "missing",
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new()
        ),
        None
    );
}

fn write_skill(dir: &Path, stem: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(format!("{stem}.md")),
        format!("---\nname: {stem}\n---\n\n{body}\n"),
    )
    .unwrap();
}

#[test]
fn list_source_stems_reads_md_files() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "alpha", "A");
    write_skill(dir.path(), "beta", "B");
    // Hidden and non-md files are ignored.
    fs::write(dir.path().join(".hidden.md"), "x").unwrap();
    fs::write(dir.path().join("notes.txt"), "x").unwrap();
    assert_eq!(
        list_source_stems(dir.path()).unwrap(),
        set(&["alpha", "beta"])
    );
}

#[test]
fn list_source_stems_missing_empty() {
    assert!(
        list_source_stems(Path::new("/nonexistent/skills"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn list_source_stems_matches_deployer_stem_for_double_extension() {
    // Regression (PR #2818 review, MEDIUM): a pathological `foo.md.md`
    // source file must resolve to the SAME stem the deployer's own
    // `select` predicate uses (single-strip: `foo.md`), not a
    // repeated-strip `foo`. Otherwise the planner greenlights a stem the
    // deployer then silently rejects and the skill never deploys.
    let bundled = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    fs::create_dir_all(bundled.path()).unwrap();
    fs::write(
        bundled.path().join("foo.md.md"),
        "---\nname: foo.md\n---\n\nDOUBLE EXTENSION\n",
    )
    .unwrap();

    let stems = list_source_stems(bundled.path()).unwrap();
    assert_eq!(
        stems,
        set(&["foo.md"]),
        "planner stem must match the deployer's single-strip semantics"
    );

    // Prove it round-trips through the real orchestrator: the planned
    // stem must actually be what gets deployed, not silently dropped.
    let out = deploy_all_skill_tiers(
        bundled.path(),
        Path::new("/nonexistent"),
        dest.path(),
        |_| true,
    )
    .unwrap();
    assert!(
        out.stats.deployed.contains(&"foo.md".to_string()),
        "the double-extension skill must actually deploy: {out:?}"
    );
    assert!(dest.path().join("foo.md").join("SKILL.md").is_file());
}

#[test]
fn project_custom_stems_finds_unmanaged() {
    // A hand-placed skill dir with SKILL.md and no manifest entry is
    // project-custom; a bundled deploy of another skill is not.
    let dest = TempDir::new().unwrap();
    let custom = dest.path().join("my-skill");
    fs::create_dir_all(&custom).unwrap();
    fs::write(custom.join("SKILL.md"), "custom").unwrap();

    // Deploy a bundled skill so the manifest manages it.
    let src = TempDir::new().unwrap();
    write_skill(src.path(), "shipped", "S");
    deploy_skills_filtered(src.path(), dest.path(), |_| true).unwrap();

    let project = list_project_custom_stems(dest.path()).unwrap();
    assert!(project.contains("my-skill"));
    assert!(
        !project.contains("shipped"),
        "managed skills are not custom"
    );
}

#[test]
fn deploy_all_user_overrides_bundled() {
    // A user-custom skill of the same name as a bundled one wins on disk.
    let bundled = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    write_skill(bundled.path(), "shared", "FROM BUNDLED");
    write_skill(bundled.path(), "bundled-only", "B");
    write_skill(user.path(), "shared", "FROM USER");

    let out = deploy_all_skill_tiers(bundled.path(), user.path(), dest.path(), |_| true).unwrap();

    let shared = fs::read_to_string(dest.path().join("shared").join("SKILL.md")).unwrap();
    assert!(shared.contains("FROM USER"), "user tier must win: {shared}");
    assert!(dest.path().join("bundled-only").join("SKILL.md").is_file());
    assert_eq!(out.shadowed.len(), 1);
    assert_eq!(out.shadowed[0].winner, SkillTier::User);
}

#[test]
fn deploy_all_preserves_project() {
    // A hand-placed project skill is never overwritten by either tier,
    // even across a redeploy, and its content is untouched.
    let bundled = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    write_skill(bundled.path(), "keeper", "FROM BUNDLED");
    write_skill(user.path(), "keeper", "FROM USER");

    let custom = dest.path().join("keeper");
    fs::create_dir_all(&custom).unwrap();
    fs::write(custom.join("SKILL.md"), "HAND WRITTEN").unwrap();

    // Two deploys to prove the preservation is stable across redeploys.
    deploy_all_skill_tiers(bundled.path(), user.path(), dest.path(), |_| true).unwrap();
    let out = deploy_all_skill_tiers(bundled.path(), user.path(), dest.path(), |_| true).unwrap();

    let kept = fs::read_to_string(custom.join("SKILL.md")).unwrap();
    assert_eq!(
        kept, "HAND WRITTEN",
        "project-custom skill must be preserved"
    );
    // Project shadows both user and bundled for "keeper".
    assert!(
        out.shadowed
            .iter()
            .any(|s| s.stem == "keeper" && s.winner == SkillTier::Project)
    );
}

#[test]
fn deploy_all_no_user_tier_matches_bundled_only() {
    // With no user tier directory, the result is exactly the bundled deploy.
    let bundled = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    write_skill(bundled.path(), "a", "A");
    write_skill(bundled.path(), "b", "B");

    let out = deploy_all_skill_tiers(
        bundled.path(),
        Path::new("/nonexistent/user/skills"),
        dest.path(),
        |_| true,
    )
    .unwrap();

    assert_eq!(out.stats.deployed.len(), 2);
    assert!(out.shadowed.is_empty());
    assert!(dest.path().join("a").join("SKILL.md").is_file());
    assert!(dest.path().join("b").join("SKILL.md").is_file());
}
