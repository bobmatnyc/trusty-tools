//! Unit tests for [`super::ensure_managed_config_dir_with_root`] — the single
//! choke point every managed run path shares.
//!
//! Why a sibling file (#4873): `managed_config.rs` is a PRODUCTION file under
//! this repo's 500-SLOC cap and was already at 416 lines before this issue's
//! coverage was added. A `*_tests.rs` sibling is classified as a test file
//! (3000-SLOC cap) by `scripts/check_line_cap.sh`, matching the pattern
//! `agent_source_tests.rs` and `lifecycle_tests.rs` already use.
//!
//! What: the pre-existing agent/scaffolding cases, moved verbatim, plus the
//! #4873 skill cases — refresh, no-op idempotence, project-custom
//! preservation, frozen-skill preservation, and the warning that finally makes
//! that last one visible.

use super::*;
use tempfile::TempDir;

/// The session workspace these tests provision against.
///
/// Why (#4880): `ensure_managed_config_dir_with_root` now also refreshes the
/// PROJECT skill tier, which deploys into `<project_dir>/.claude/skills`.
/// Keeping it inside the same temp base is what stops these tests writing into
/// the operator's real checkout.
/// What: `<base>/workspace`, created on first use.
/// Test: used by every case in this file.
fn project_dir(base: &Path) -> std::path::PathBuf {
    let dir = base.join("workspace");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a `FrameworkPaths` whose framework SOURCE dirs are populated from a
/// small but representative slice of the real bundled roster, so the deploy
/// assertions do not depend on a prior `tm install` or the git submodule.
fn seed_framework(base: &Path) -> FrameworkPaths {
    let fw = FrameworkPaths::under(base);
    std::fs::create_dir_all(&fw.agents).unwrap();
    std::fs::create_dir_all(&fw.skills).unwrap();
    // A minimal set spanning the roster's shape: a base to inherit from, a
    // core specialist, and a couple of the agents #1996 called out.
    let agents: &[(&str, &str)] = &[
        (
            "BASE-AGENT.md",
            "---\nname: BASE-AGENT\ndescription: base\n---\n\nBase.\n",
        ),
        (
            "engineer.md",
            "---\nname: engineer\ndescription: implementation specialist\n---\n\nEngineer.\n",
        ),
        (
            "rust-engineer.md",
            "---\nname: rust-engineer\ndescription: rust specialist\n---\n\nRust.\n",
        ),
        (
            "research.md",
            "---\nname: research\ndescription: research specialist\n---\n\nResearch.\n",
        ),
        (
            "qa.md",
            "---\nname: qa\ndescription: quality specialist\n---\n\nQA.\n",
        ),
        (
            "version-control.md",
            "---\nname: version-control\ndescription: git specialist\n---\n\nVCS.\n",
        ),
        (
            "ticketing.md",
            "---\nname: ticketing\ndescription: ticketing specialist\n---\n\nTickets.\n",
        ),
    ];
    for (name, body) in agents {
        std::fs::write(fw.agents.join(name), body).unwrap();
    }
    std::fs::write(
        fw.skills.join("tm-doctor.md"),
        "---\nname: tm-doctor\ndescription: doctor\n---\n\nDoctor.\n",
    )
    .unwrap();
    fw
}

#[test]
fn ensure_managed_config_dir_deploys_full_roster() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();

    // Scaffolding landed.
    assert!(config_dir.join("settings.json").exists());
    assert!(config_dir.join(".mcp.json").exists());

    // Every seeded specialist must be present AND spawnable (has a
    // `description` in its deployed frontmatter).
    let agents_dir = config_dir.join("agents");
    for name in [
        "engineer",
        "rust-engineer",
        "research",
        "qa",
        "version-control",
        "ticketing",
    ] {
        let path = agents_dir.join(format!("{name}.md"));
        assert!(path.exists(), "agent {name} must be deployed to config dir");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("description:"),
            "deployed agent {name} must carry a description (spawnable)"
        );
    }

    // Skills landed too.
    assert!(
        config_dir.join("skills/tm-doctor/SKILL.md").exists(),
        "tm-* skills must be deployed to config dir"
    );
}

#[test]
fn ensure_managed_config_dir_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();
    let first = std::fs::read_to_string(config_dir.join("agents/engineer.md")).unwrap();

    // A second call must not fail and must leave the deployed roster intact.
    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();
    let second = std::fs::read_to_string(config_dir.join("agents/engineer.md")).unwrap();

    assert_eq!(first, second, "re-provisioning must be idempotent");
}

#[test]
fn ensure_managed_config_dir_refreshes_stale_bundled_agents() {
    // #4840: the exact measured shape — a framework agent source written by
    // an older `tm install` sits on disk while the running binary embeds a
    // newer one. Provisioning must close that gap with no manual step.
    let tmp = TempDir::new().unwrap();
    let fw = FrameworkPaths::under(tmp.path());
    std::fs::create_dir_all(&fw.agents).unwrap();
    std::fs::write(
        fw.agents.join("BASE-AGENT.md"),
        "---\nname: BASE-AGENT\ndescription: base\n---\n\nSTALE-SENTINEL-4840\n",
    )
    .unwrap();
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();

    assert_eq!(
        std::fs::read_to_string(fw.agents.join("BASE-AGENT.md")).unwrap(),
        crate::core::bundle::BASE_AGENT,
        "the bundled agent SOURCE must be re-materialized from the running binary"
    );
    let deployed = std::fs::read_to_string(config_dir.join("agents/BASE-AGENT.md")).unwrap();
    assert!(
        !deployed.contains("STALE-SENTINEL-4840"),
        "the stale composition must not survive into the deploy target"
    );
}

#[test]
fn ensure_managed_config_dir_survives_an_unwritable_agent_target() {
    // Fail open (#4840): a broken agent deploy must never block a session.
    // A regular file where `<config_dir>/agents` belongs makes every write
    // under it impossible and cannot be repaired by `create_dir_all`.
    let tmp = TempDir::new().unwrap();
    let fw = FrameworkPaths::under(tmp.path());
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("agents"), "blocking file\n").unwrap();

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path()))
        .expect("an undeployable agent roster must not fail provisioning");

    assert!(
        config_dir.join("settings.json").exists(),
        "the rest of the config dir must still be provisioned"
    );
}

#[test]
fn ensure_managed_config_dir_deploys_user_tier_skill() {
    // PR #2818 review (round 3, MEDIUM decision): a user-custom skill
    // (`fw.user_skill_source_dir()`, i.e. `<root>/skills`) must reach the
    // tm-global roster, not just per-project deploys.
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    std::fs::create_dir_all(&fw.user_skills).unwrap();
    std::fs::write(
        fw.user_skills.join("my-custom-skill.md"),
        "---\nname: my-custom-skill\n---\n\nUSER CUSTOM.\n",
    )
    .unwrap();

    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();

    assert!(
        config_dir.join("skills/my-custom-skill/SKILL.md").exists(),
        "user-custom skill must be deployed to the tm-global config dir"
    );
}

/// Static verification against the REAL bundled roster (`src/assets/agents`,
/// `src/assets/skills`). Ignored by default because it composes and writes the
/// full ~40-agent set; run explicitly to prove the complete specialist roster
/// deploys and that every deployed agent is spawnable (carries a `description`):
///
/// `cargo test -p trusty-mpm --lib manual_static_verify_full_roster -- --ignored --nocapture`
#[test]
#[ignore = "static verification — composes the full real roster; run explicitly"]
fn manual_static_verify_full_roster() {
    let tmp = TempDir::new().unwrap();
    let fw = FrameworkPaths::under(tmp.path());
    std::fs::create_dir_all(&fw.agents).unwrap();
    std::fs::create_dir_all(&fw.skills).unwrap();

    // Copy the REAL bundled raw sources into the framework source dirs.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for (src, dst) in [
        (manifest.join("src/assets/agents"), &fw.agents),
        (manifest.join("src/assets/skills"), &fw.skills),
    ] {
        for entry in std::fs::read_dir(&src).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().and_then(|e| e.to_str()) == Some("md") {
                std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
            }
        }
    }

    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();

    // List every deployed agent and confirm each is spawnable.
    let agents_dir = config_dir.join("agents");
    let mut deployed: Vec<String> = std::fs::read_dir(&agents_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    deployed.sort();

    println!("\n=== deployed agents ({}) ===", deployed.len());
    let mut missing_desc = Vec::new();
    for name in &deployed {
        let body = std::fs::read_to_string(agents_dir.join(name)).unwrap();
        let has_desc = body.contains("description:");
        println!("  {name}  spawnable={has_desc}");
        // BASE-*.md are inheritance templates (parents of `extends:` agents),
        // intentionally without a `description` and never independently
        // spawnable — exclude them from the spawnable requirement.
        if !has_desc && !name.starts_with("BASE-") {
            missing_desc.push(name.clone());
        }
    }

    // The specialists #1996 and the task call out must all be present.
    for required in [
        "engineer.md",
        "rust-engineer.md",
        "research.md",
        "qa.md",
        "local-ops.md",
        "version-control.md",
        "ticketing.md",
        "documentation.md",
        "security.md",
    ] {
        assert!(
            deployed.iter().any(|d| d == required),
            "required agent {required} missing from deployed roster: {deployed:?}"
        );
    }
    assert!(
        missing_desc.is_empty(),
        "these deployed agents are not spawnable (no description): {missing_desc:?}"
    );
}

// ---------------------------------------------------------------------------
// #4873 — the SKILL half of the every-run deploy, and the warning that makes
// a declined refresh visible.
// ---------------------------------------------------------------------------

/// Pin `fw.skills` as already-current so `ensure_skill_source_fresh` leaves a
/// hand-seeded skill source alone.
///
/// Why: that function materializes the REAL compiled-in bundle over
/// `fw.skills` (and prunes anything the bundle does not list) whenever the
/// stamp file is absent or stale — which would erase every fixture below
/// before the deploy under test ever saw it. Writing the current stamp is the
/// supported way to say "this source is the bundle", and it is what an
/// installed machine's source dir looks like in steady state.
/// What: writes `<fw.skills>/.bundle-stamp` with `skill_bundle_stamp()`.
/// Test: used by every `#4873` case below; its necessity is proven by
/// `pinning_the_stamp_preserves_a_seeded_skill_source`.
fn pin_skill_source_stamp(fw: &FrameworkPaths) {
    std::fs::create_dir_all(&fw.skills).unwrap();
    std::fs::write(
        fw.skills.join(crate::core::skill_source::STAMP_FILE_NAME),
        crate::core::skill_source::skill_bundle_stamp(),
    )
    .unwrap();
}

/// Seed one bundled-tier skill with the given body.
fn seed_skill(fw: &FrameworkPaths, stem: &str, body: &str) {
    std::fs::create_dir_all(&fw.skills).unwrap();
    std::fs::write(fw.skills.join(format!("{stem}.md")), body).unwrap();
}

/// Guard on the fixture itself: without the pin, the seeded source is replaced
/// by the real bundle and every assertion below would be measuring the wrong
/// thing.
///
/// Test: itself.
#[test]
fn pinning_the_stamp_preserves_a_seeded_skill_source() {
    let tmp = TempDir::new().unwrap();
    let fw = FrameworkPaths::under(tmp.path());
    pin_skill_source_stamp(&fw);
    seed_skill(&fw, "probe-skill", "---\nname: probe-skill\n---\n\nV1\n");

    crate::core::skill_source::ensure_skill_source_fresh(&fw).unwrap();

    assert_eq!(
        std::fs::read_to_string(fw.skills.join("probe-skill.md")).unwrap(),
        "---\nname: probe-skill\n---\n\nV1\n",
        "a pinned-current source must not be re-materialized from the bundle"
    );
}

/// #4873 test 1+2: the shared choke point every run path reaches — a fresh
/// spawn, a `resume_managed`, and a bare-`tm` in-place relaunch all arrive
/// here via `runtime::claude_code::prepare_managed_config` — refreshes a
/// deployed skill that is managed, unmodified, and stale relative to the
/// bundled source.
///
/// Why one test for both paths: `resume_managed` and `run_inplace_relaunch`
/// perform NO skill work of their own; each delegates to
/// `prepare_managed_config`, whose only skill step is this function. Asserting
/// the behaviour here is what proves it for both, and does so without standing
/// up a daemon or a tmux pane.
/// Test: itself.
#[test]
fn ensure_managed_config_dir_refreshes_a_stale_managed_skill() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    pin_skill_source_stamp(&fw);
    seed_skill(&fw, "probe-skill", "---\nname: probe-skill\n---\n\nV1\n");
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let deployed = config_dir.join("skills/probe-skill/SKILL.md");

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();
    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap(),
        "---\nname: probe-skill\n---\n\nV1\n"
    );

    // The binary now embeds newer content — the exact #4873 shape.
    seed_skill(
        &fw,
        "probe-skill",
        "---\nname: probe-skill\n---\n\nV2-REFRESHED\n",
    );
    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();

    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap(),
        "---\nname: probe-skill\n---\n\nV2-REFRESHED\n",
        "a managed, user-unmodified skill must be refreshed on every run, not only on `tm install`"
    );
}

/// #4873 test 3: a second run with nothing changed rewrites nothing.
///
/// Why mtime and not just content: "cheap and idempotent" is the property the
/// every-run deploy depends on; an unconditional rewrite would still leave the
/// content correct while churning every skill file on every spawn. This is the
/// skill mirror of `skill_source::ensure_skill_source_fresh_is_noop_when_current`.
/// Test: itself.
#[test]
fn ensure_managed_config_dir_skill_deploy_is_a_noop_when_unchanged() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    pin_skill_source_stamp(&fw);
    seed_skill(&fw, "probe-skill", "---\nname: probe-skill\n---\n\nV1\n");
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let deployed = config_dir.join("skills/probe-skill/SKILL.md");

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();
    let before = std::fs::metadata(&deployed).unwrap().modified().unwrap();

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();
    let after = std::fs::metadata(&deployed).unwrap().modified().unwrap();

    assert_eq!(
        before, after,
        "an unchanged skill must take the manifest checksum no-op path, not a rewrite"
    );
}

/// #4873 test 4 (negative): a project-custom skill — present on disk, absent
/// from the manifest — is never overwritten.
///
/// Why: this is what makes `cargo-publish` and `tm-slack-canvas-delivery`
/// immune to the every-run deploy. `deploy_one_file` skips an unmanaged target
/// unconditionally; deploying more often must not weaken that.
/// Test: itself.
#[test]
fn ensure_managed_config_dir_preserves_a_project_custom_skill() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    pin_skill_source_stamp(&fw);
    seed_skill(
        &fw,
        "probe-skill",
        "---\nname: probe-skill\n---\n\nBUNDLED\n",
    );
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");

    // A hand-placed skill that no manifest has ever recorded, sharing a stem
    // with a bundled one so it is the deployer's ownership rule under test and
    // not mere absence from the source.
    let custom = config_dir.join("skills/probe-skill/SKILL.md");
    std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
    std::fs::write(&custom, "PROJECT CUSTOM — DO NOT TOUCH\n").unwrap();

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();

    assert_eq!(
        std::fs::read_to_string(&custom).unwrap(),
        "PROJECT CUSTOM — DO NOT TOUCH\n",
        "an unmanaged on-disk skill must survive the every-run deploy untouched"
    );
}

/// #4873 test 5 (negative): a checksum-frozen (hand-edited) skill is still
/// skipped.
///
/// Why: #4873 explicitly must NOT become "overwrite user edits on every run".
/// The remedy for a frozen skill stays the opt-in
/// `tm doctor --fix-skills --include-frozen`; this pins that the normal path
/// leaves it alone even though the bundled source has moved on.
/// Test: itself.
#[test]
fn ensure_managed_config_dir_skips_a_frozen_skill() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    // Deployed once, then hand-edited, with the source now at V2 — the
    // manifest checksum records V1 while disk holds the edit.
    let (config_dir, deployed) = frozen_skill_fixture(&tmp, &fw);

    ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();

    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap(),
        "HAND EDITED BY THE OPERATOR\n",
        "a checksum-frozen skill must still be preserved — `tm doctor --fix-skills \
         --include-frozen` remains the only way to adopt it"
    );
}

/// Drive the fixture to the state the #4873 warning describes: one bundled
/// skill deployed, then hand-edited, with the source moved on beyond it.
///
/// Why: three cases below need the same checksum-frozen shape, and building it
/// takes two provisioning runs plus a write — duplicating that setup is how the
/// three would drift apart.
/// What: seeds `probe-skill` at V1, provisions once so the skill becomes
/// MANAGED with V1's checksum recorded, overwrites the deployed copy by hand,
/// then advances the source to V2. Returns the config dir and the deployed
/// path. Leaves the NEXT provisioning run to the caller — that run is what each
/// test is actually measuring.
/// Test: used by `ensure_managed_config_dir_skips_a_frozen_skill`,
/// `ensure_managed_config_dir_emits_the_frozen_skill_warning`,
/// `a_declined_skill_reaches_skill_skip_summary_from_a_real_deploy`.
fn frozen_skill_fixture(
    tmp: &TempDir,
    fw: &FrameworkPaths,
) -> (std::path::PathBuf, std::path::PathBuf) {
    pin_skill_source_stamp(fw);
    seed_skill(fw, "probe-skill", "---\nname: probe-skill\n---\n\nV1\n");
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let deployed = config_dir.join("skills/probe-skill/SKILL.md");

    ensure_managed_config_dir_with_root(fw, &config_dir, &project_dir(tmp.path())).unwrap();
    std::fs::write(&deployed, "HAND EDITED BY THE OPERATOR\n").unwrap();
    seed_skill(fw, "probe-skill", "---\nname: probe-skill\n---\n\nV2\n");

    (config_dir, deployed)
}

/// #4873's own fix, AT THE CALL SITE: `ensure_managed_config_dir_with_root`
/// actually emits the warning when the deploy declines a skill.
///
/// Why (PR #4876 review): the three `skill_skip_summary_*` cases below cover
/// the pure function, and the case after this one covers the deploy that feeds
/// it — but deleting the entire `if let Some(line) = …` emit block left all of
/// them green. The wiring was the whole production change and nothing tested
/// it. This is the test that fails when that block is removed.
/// What: installs a capturing subscriber for the duration of one provisioning
/// run and asserts a `WARN` line carrying the module prefix, the declined
/// skill's name, and the remedy pointer reaches it. Uses the crate's existing
/// capture entry point (`trusty_common::log_buffer::LogBufferLayer` +
/// `tracing::subscriber::with_default`, the pattern `log_buffer`'s own
/// `layer_captures_events` and `trusty-search`'s
/// `fallback_logs_build_failure_exactly_once` already use) rather than adding a
/// capture dependency. `#[serial]` for the same reason that test carries it:
/// installing a subscriber perturbs the process-global interest cache.
/// Test: itself.
#[test]
#[serial_test::serial]
fn ensure_managed_config_dir_emits_the_frozen_skill_warning() {
    use tracing_subscriber::layer::SubscriberExt;

    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    // Build the frozen state OUTSIDE the capture so the only lines recorded
    // belong to the run under test.
    let (config_dir, _deployed) = frozen_skill_fixture(&tmp, &fw);

    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );

    tracing::subscriber::with_default(subscriber, || {
        ensure_managed_config_dir_with_root(&fw, &config_dir, &project_dir(tmp.path())).unwrap();
    });

    let lines = buffer.tail(64);
    let hit = lines
        .iter()
        .find(|l| l.contains("managed config dir:") && l.contains("probe-skill"));
    let line = hit.unwrap_or_else(|| {
        panic!(
            "provisioning declined `probe-skill` but emitted no warning naming it — \
             the emit block in `ensure_managed_config_dir_with_root` is missing or \
             unreachable. Captured lines: {lines:#?}"
        )
    });
    assert!(
        line.contains("WARN"),
        "the declined-skill line must be logged at WARN, not a lower level: {line}"
    );
    assert!(
        line.contains("tm doctor --fix-skills --include-frozen"),
        "the emitted warning must carry the actionable remedy: {line}"
    );
}

/// The deploy really does report a frozen skill as `skipped`, so
/// [`skill_skip_summary`]'s input is the shape production hands it.
///
/// Why: the pure-function cases below feed hand-written vectors. This one
/// closes the gap between "the summary formats a skip list" and "a real
/// checksum-frozen skill lands in that list" — without it, a change to the
/// deployer's classification could empty the list and every other case would
/// still pass.
/// Test: itself.
#[test]
fn a_declined_skill_reaches_skill_skip_summary_from_a_real_deploy() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    let (config_dir, _deployed) = frozen_skill_fixture(&tmp, &fw);

    let stats = deploy_all_skill_tiers(
        &fw.skill_source_dir(),
        &fw.user_skill_source_dir(),
        &config_dir.join("skills"),
        |_| true,
    )
    .unwrap();

    assert!(
        stats.stats.skipped.iter().any(|s| s == "probe-skill"),
        "a checksum-frozen skill must be reported as skipped: {:?}",
        stats.stats.skipped
    );
    let line = skill_skip_summary(&stats.stats.skipped)
        .expect("a declined skill must produce exactly one summary line");
    assert!(
        line.contains("probe-skill") && line.contains("tm doctor --fix-skills --include-frozen"),
        "{line}"
    );
}

#[test]
fn skill_skip_summary_is_none_on_a_clean_deploy() {
    assert!(skill_skip_summary(&[]).is_none());
}

#[test]
fn skill_skip_summary_counts_and_previews() {
    let line = skill_skip_summary(&["tm".to_string(), "code-review-standards".to_string()])
        .expect("a non-empty skip set must summarise");
    assert!(line.contains("2 skill file(s)"), "{line}");
    assert!(line.contains("tm, code-review-standards"), "{line}");
}

#[test]
fn skill_skip_summary_elides_beyond_five() {
    // Bounded output is the whole reason this is a summary and not one line
    // per file — it runs on every spawn, resume, and relaunch.
    let many: Vec<String> = (0..9).map(|i| format!("skill-{i}")).collect();
    let line = skill_skip_summary(&many).expect("summary");
    assert!(line.contains("9 skill file(s)"), "{line}");
    assert!(line.contains("skill-4, …"), "{line}");
    assert!(
        !line.contains("skill-5"),
        "beyond the preview limit: {line}"
    );
}

// ---------------------------------------------------------------------------
// #4880 — the PROJECT tier reaches the same choke point, gated on the project
// manifest.
// ---------------------------------------------------------------------------

/// #4880 test 5: a fresh spawn, a `resume_managed`, and a bare-`tm` in-place
/// relaunch all deploy the PROJECT skill tier.
///
/// Why one test for all three: none of those paths performs skill work of its
/// own — each reaches `runtime::claude_code::prepare_managed_config`, whose only
/// skill steps are the user-tier deploy above and the project-tier call this
/// asserts. The same argument `ensure_managed_config_dir_refreshes_a_stale_managed_skill`
/// already makes for the user tier (#4873) is what makes this test the proof for
/// the project tier, without standing up a daemon or a tmux pane. Delete the
/// `project_skill_tier` block from `ensure_managed_config_dir_with_root` and
/// this fails.
/// Test: itself.
#[test]
fn ensure_managed_config_dir_deploys_the_project_skill_tier() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    pin_skill_source_stamp(&fw);
    seed_skill(&fw, "probe-skill", "---\nname: probe-skill\n---\n\nV1\n");
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let workspace = project_dir(tmp.path());
    let project_copy = workspace
        .join(".claude")
        .join("skills")
        .join("probe-skill")
        .join("SKILL.md");

    ensure_managed_config_dir_with_root(&fw, &config_dir, &workspace).unwrap();

    assert_eq!(
        std::fs::read_to_string(&project_copy).unwrap(),
        "---\nname: probe-skill\n---\n\nV1\n",
        "the project tier — which OUTRANKS the config-dir tier — must be deployed \
         by the shared choke point, not only by `prepare_session`"
    );

    // Newer text plus an edited `[skills]` selection: the trigger fires without
    // a version bump. (The version half is asserted hermetically in
    // `project_skill_tier_tests::version_bump_redeploys`; this crate's compiled
    // `CARGO_PKG_VERSION` cannot move mid-test.) The exclude names a DIFFERENT
    // stem so `probe-skill` stays selected.
    seed_skill(&fw, "probe-skill", "---\nname: probe-skill\n---\n\nV2\n");
    let framework = crate::core::harness_root::framework_dir(&workspace);
    std::fs::create_dir_all(&framework).unwrap();
    std::fs::write(
        framework.join("manifest.toml"),
        "[skills]\nexclude = [\"something-else\"]\n",
    )
    .unwrap();

    ensure_managed_config_dir_with_root(&fw, &config_dir, &workspace).unwrap();

    assert_eq!(
        std::fs::read_to_string(&project_copy).unwrap(),
        "---\nname: probe-skill\n---\n\nV2\n",
        "an updated skill selection must refresh the project tier on resume"
    );
}

/// #4880: a relaunch with nothing changed rewrites nothing in the project tier.
///
/// Why: the owner's ruling is "when the project manifest is updated", not "every
/// run". mtime equality is what distinguishes a stamp no-op from a redeploy that
/// merely happens to write identical bytes.
/// Test: itself.
#[test]
fn ensure_managed_config_dir_project_tier_is_a_noop_when_unchanged() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path());
    pin_skill_source_stamp(&fw);
    seed_skill(&fw, "probe-skill", "---\nname: probe-skill\n---\n\nV1\n");
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let workspace = project_dir(tmp.path());
    let project_copy = workspace
        .join(".claude")
        .join("skills")
        .join("probe-skill")
        .join("SKILL.md");

    ensure_managed_config_dir_with_root(&fw, &config_dir, &workspace).unwrap();
    let before = std::fs::metadata(&project_copy)
        .unwrap()
        .modified()
        .unwrap();

    ensure_managed_config_dir_with_root(&fw, &config_dir, &workspace).unwrap();
    let after = std::fs::metadata(&project_copy)
        .unwrap()
        .modified()
        .unwrap();

    assert_eq!(
        before, after,
        "an unchanged version and selection must take the stamp no-op path"
    );
}
