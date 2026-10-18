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

// ---------------------------------------------------------------------------
// #4949 — directory-shaped skills reach the tier planner and the deploy
// ---------------------------------------------------------------------------

#[test]
fn list_source_stems_reads_directory_skills() {
    // The planner must see the same stems the deployer acts on. Before #4949
    // this function carried its own `is_file()` copy of the scan and returned
    // an empty set for a directory-shaped tier, so the user tier was planned
    // as absent and every one of its skills was dropped.
    let src = TempDir::new().unwrap();
    let skill = src.path().join("duetto-design-system");
    fs::create_dir_all(skill.join("references")).unwrap();
    fs::write(skill.join("SKILL.md"), "# Duetto\n").unwrap();
    fs::write(skill.join("references").join("tokens.md"), "# T\n").unwrap();
    write_skill(src.path(), "flat-skill", "F");

    let stems = list_source_stems(src.path()).unwrap();

    assert_eq!(stems, set(&["duetto-design-system", "flat-skill"]));
}

#[test]
fn deploy_all_deploys_a_directory_shaped_user_skill() {
    // End-to-end at the tier the defect was reported at: a user-custom skill
    // authored as a directory in `~/.trusty-mpm/skills/` must reach the
    // destination with every file it carries.
    let bundled = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    write_skill(bundled.path(), "shipped", "S");
    let skill = user.path().join("cto-kb-ingest");
    fs::create_dir_all(skill.join("scripts")).unwrap();
    fs::write(skill.join("SKILL.md"), "# Ingest\n").unwrap();
    fs::write(skill.join("scripts").join("ingest.sh"), "#!/bin/sh\n").unwrap();

    let deploy =
        deploy_all_skill_tiers(bundled.path(), user.path(), dest.path(), |_| true).unwrap();

    assert!(
        deploy.stats.deployed.contains(&"cto-kb-ingest".to_string()),
        "{:?}",
        deploy.stats
    );
    assert!(dest.path().join("cto-kb-ingest").join("SKILL.md").is_file());
    assert!(
        dest.path()
            .join("cto-kb-ingest")
            .join("scripts")
            .join("ingest.sh")
            .is_file(),
        "carried script missing"
    );
    // The bundled tier is untouched by the addition.
    assert!(dest.path().join("shipped").join("SKILL.md").is_file());
}

#[test]
fn deploy_all_lets_a_directory_user_skill_shadow_a_bundled_one() {
    // Precedence must hold across layouts: a directory-shaped user skill
    // outranks a same-named flat bundled skill.
    let bundled = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    write_skill(bundled.path(), "shared", "BUNDLED");
    let skill = user.path().join("shared");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "USER").unwrap();

    let deploy =
        deploy_all_skill_tiers(bundled.path(), user.path(), dest.path(), |_| true).unwrap();

    let content = fs::read_to_string(dest.path().join("shared").join("SKILL.md")).unwrap();
    assert_eq!(content, "USER", "user tier must win");
    assert_eq!(deploy.shadowed.len(), 1);
    assert_eq!(deploy.shadowed[0].winner, SkillTier::User);
}

// ---------------------------------------------------------------------------
// #9540 — an empty optional user tier is quiet; a required tier still warns
// ---------------------------------------------------------------------------

/// A `MakeWriter` over a shared buffer, so a test can read what was logged.
#[derive(Clone)]
struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `f` under a thread-local WARN-level subscriber; return its output.
fn capture_warnings<T>(f: impl FnOnce() -> T) -> (T, String) {
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(std::sync::Arc::clone(&buffer)))
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let logs =
        String::from_utf8_lossy(&buffer.lock().unwrap_or_else(|e| e.into_inner())).into_owned();
    (out, logs)
}

#[test]
fn deploy_all_empty_or_absent_user_tier_warns_for_no_destination() {
    // #9540: every launch deploys into two or more destinations, and an empty
    // `~/.trusty-mpm/skills` warned once per destination. The tier is optional.
    let bundled = TempDir::new().unwrap();
    write_skill(bundled.path(), "a", "A");
    let user_root = TempDir::new().unwrap();
    let empty_user = user_root.path().join("skills");
    fs::create_dir_all(&empty_user).unwrap();
    let absent_user = user_root.path().join("absent");
    let dests = [
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    ];

    let (results, logs) = capture_warnings(|| {
        [
            deploy_all_skill_tiers(bundled.path(), &empty_user, dests[0].path(), |_| true),
            deploy_all_skill_tiers(bundled.path(), &empty_user, dests[1].path(), |_| true),
            deploy_all_skill_tiers(bundled.path(), &absent_user, dests[2].path(), |_| true),
        ]
    });

    for (result, dest) in results.into_iter().zip(&dests) {
        let deploy = result.expect("deploy succeeds");
        assert_eq!(deploy.stats.deployed, vec!["a".to_string()]);
        assert!(dest.path().join("a").join("SKILL.md").is_file());
    }
    let user_path = user_root.path().display().to_string();
    assert!(
        !logs.contains(&user_path) && !logs.contains("skill source directory"),
        "an empty or absent optional user tier must log no warning:\n{logs}"
    );
}

#[test]
fn deploy_all_empty_or_missing_bundled_tier_still_warns() {
    // #9540: only the OPTIONAL user tier goes quiet. A required bundled source
    // that is empty or missing is a real misconfiguration (A2) and must still
    // warn, once per destination, naming the source.
    let user = TempDir::new().unwrap();
    write_skill(user.path(), "mine", "MINE");
    let bundled_root = TempDir::new().unwrap();
    let empty_bundled = bundled_root.path().join("empty");
    fs::create_dir_all(&empty_bundled).unwrap();
    let missing_bundled = bundled_root.path().join("missing");
    let dests = [TempDir::new().unwrap(), TempDir::new().unwrap()];

    let (results, logs) = capture_warnings(|| {
        [
            deploy_all_skill_tiers(&empty_bundled, user.path(), dests[0].path(), |_| true),
            deploy_all_skill_tiers(&missing_bundled, user.path(), dests[1].path(), |_| true),
        ]
    });

    for result in results {
        let deploy = result.expect("deploy succeeds");
        assert_eq!(deploy.stats.deployed, vec!["mine".to_string()]);
    }
    let empty_line = logs
        .lines()
        .find(|l| l.contains("skill source directory is empty"))
        .unwrap_or_else(|| panic!("an empty bundled tier must warn:\n{logs}"));
    assert!(empty_line.contains(&empty_bundled.display().to_string()));
    let missing_line = logs
        .lines()
        .find(|l| l.contains("skill source directory missing"))
        .unwrap_or_else(|| panic!("a missing bundled tier must warn:\n{logs}"));
    assert!(missing_line.contains(&missing_bundled.display().to_string()));
}

#[test]
fn deploy_all_user_tier_with_only_a_malformed_skill_still_reports_it() {
    // #9540: "empty" means the scan found nothing at all. A user tier whose
    // only entry is a directory with no SKILL.md is not empty — the deployer
    // must still name it in `skipped` and warn, as #4949 requires.
    let bundled = TempDir::new().unwrap();
    write_skill(bundled.path(), "a", "A");
    let user = TempDir::new().unwrap();
    fs::create_dir_all(user.path().join("broken")).unwrap();
    let dest = TempDir::new().unwrap();

    let (result, logs) = capture_warnings(|| {
        deploy_all_skill_tiers(bundled.path(), user.path(), dest.path(), |_| true)
    });

    let deploy = result.expect("deploy succeeds");
    assert_eq!(deploy.stats.skipped, vec!["broken".to_string()]);
    assert!(
        logs.contains("skill directory has no SKILL.md"),
        "a malformed user skill must still warn:\n{logs}"
    );
}
