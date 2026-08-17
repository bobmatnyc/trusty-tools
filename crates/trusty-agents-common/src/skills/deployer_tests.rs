//! Tests for `deployer` — split out to mirror the `builder`/`builder_tests`
//! pattern and keep `deployer.rs` under the 500-line SLOC cap.
//!
//! Why: moved verbatim from `trusty-mpm::core::skill_deployer`'s inline
//! `#[cfg(test)] mod tests` (#2892, #2818), plus the reference-file mirroring
//! tests ported from `trusty-mpm` PR #2915 (issue #2903) — behavior-preserving
//! extraction, not a rewrite.
//! What: covers a new deploy, a skipped user-modified file, an unchanged
//! file, a user-owned file, the `mcp`-name collision guard, and the
//! `references/*.md` sibling-file mirroring cases.
//! Test: this file IS the test module for `deployer`; run with
//! `cargo test -p trusty-agents-common -- skills::deployer`.

use super::*;
use crate::agents::manifest::ManifestError;
use std::fs;
use tempfile::TempDir;

/// A two-file skill source set.
fn write_sources(dir: &Path) {
    fs::write(
        dir.join("tm-doctor.md"),
        "---\nname: tm-doctor\n---\n\n# Doctor\n\nDiagnostic skill.\n",
    )
    .unwrap();
    fs::write(
        dir.join("example-skill.md"),
        "---\nname: example-skill\n---\n\n# Example\n\nExample skill.\n",
    )
    .unwrap();
}

#[test]
fn deploy_new_skill() {
    // A first-ever deploy must write every skill as <dest>/<name>/SKILL.md
    // and record the skill name (stem) in the manifest.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert_eq!(stats.deployed.len(), 2);
    // Stats report stems, not filenames.
    assert!(stats.deployed.contains(&"tm-doctor".to_string()));
    assert!(stats.skipped.is_empty());
    assert!(stats.unchanged.is_empty());

    // Each skill lands at <dest>/<name>/SKILL.md — not a flat .md file.
    let doctor = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
    assert!(doctor.contains("Diagnostic skill."));

    let manifest = SkillManifest::load(tgt.path()).unwrap();
    assert!(manifest.is_managed("tm-doctor"));
}

#[test]
fn deploy_filtered_respects_predicate() {
    // HR-2: a selection predicate restricts which source skills deploy.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md

    let stats =
        deploy_skills_filtered(src.path(), tgt.path(), |name| name == "example-skill").unwrap();

    assert!(stats.deployed.contains(&"example-skill".to_string()));
    assert!(!stats.deployed.contains(&"tm-doctor".to_string()));
    assert!(tgt.path().join("example-skill").join("SKILL.md").exists());
    assert!(!tgt.path().join("tm-doctor").exists());
}

#[test]
fn deploy_skips_user_modified() {
    // A managed file the user edited must be skipped, not overwritten.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    deploy_skills(src.path(), tgt.path()).unwrap();

    // Simulate the user editing the deployed SKILL.md.
    fs::write(
        tgt.path().join("tm-doctor").join("SKILL.md"),
        "---\nname: tm-doctor\n---\n\nUSER HAND-EDIT\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(stats.skipped.contains(&"tm-doctor".to_string()));
    assert!(!stats.deployed.contains(&"tm-doctor".to_string()));

    let still = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
    assert!(still.contains("USER HAND-EDIT"));
}

#[test]
fn deploy_unchanged_no_write() {
    // A second deploy with no source changes must report files unchanged
    // and not rewrite them.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    deploy_skills(src.path(), tgt.path()).unwrap();
    let skill_md = tgt.path().join("tm-doctor").join("SKILL.md");
    let before = fs::metadata(&skill_md).unwrap().modified().unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(stats.unchanged.contains(&"tm-doctor".to_string()));
    assert!(stats.deployed.is_empty());

    let after = fs::metadata(&skill_md).unwrap().modified().unwrap();
    assert_eq!(before, after, "unchanged file must not be rewritten");
}

#[test]
fn deploy_user_owned_skipped() {
    // A SKILL.md in the target that trusty-mpm never deployed (absent from
    // the manifest) must be left completely untouched.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // Pre-create a user-owned skill directory for tm-doctor.
    let user_dir = tgt.path().join("tm-doctor");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(user_dir.join("SKILL.md"), "USER OWNED — not trusty-mpm's\n").unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(stats.skipped.contains(&"tm-doctor".to_string()));

    let content = fs::read_to_string(user_dir.join("SKILL.md")).unwrap();
    assert_eq!(content, "USER OWNED — not trusty-mpm's\n");

    // example-skill had no conflict, so it deploys normally.
    assert!(stats.deployed.contains(&"example-skill".to_string()));
}

#[test]
fn deploy_refreshes_stale_managed_skill() {
    // A managed, unmodified file whose source changed must be refreshed.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    deploy_skills(src.path(), tgt.path()).unwrap();

    // The framework updates the source skill.
    fs::write(
        src.path().join("tm-doctor.md"),
        "---\nname: tm-doctor\n---\n\n# Doctor v2\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(stats.deployed.contains(&"tm-doctor".to_string()));
    let refreshed = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
    assert!(refreshed.contains("Doctor v2"));
}

#[test]
fn deploy_skips_mcp_named_skill() {
    // #2186: a skill whose name (stem) contains "mcp" must never be
    // deployed — Claude Code's built-in `/mcp` command would be shadowed
    // by `/toolchains-ai-protocols-mcp` (or any other mcp-containing
    // name). It must be recorded as skipped, not deployed, and must not
    // block sibling skills in the same batch from deploying normally.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md
    fs::write(
        src.path().join("toolchains-ai-protocols-mcp.md"),
        "---\nname: toolchains-ai-protocols-mcp\n---\n\n# AI Protocols\n\nMCP guidance.\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(
        stats
            .skipped
            .contains(&"toolchains-ai-protocols-mcp".to_string())
    );
    assert!(
        !stats
            .deployed
            .contains(&"toolchains-ai-protocols-mcp".to_string())
    );
    assert!(
        !tgt.path()
            .join("toolchains-ai-protocols-mcp")
            .join("SKILL.md")
            .exists(),
        "an mcp-named skill must never be written to disk"
    );

    // Sibling non-mcp skills in the same batch must still deploy.
    assert!(stats.deployed.contains(&"tm-doctor".to_string()));
    assert!(stats.deployed.contains(&"example-skill".to_string()));
}

#[test]
fn deploy_skips_mcp_named_skill_case_insensitive() {
    // The guard must match "mcp" regardless of case (e.g. `Foo-MCP.md`).
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md
    fs::write(
        src.path().join("Foo-MCP.md"),
        "---\nname: Foo-MCP\n---\n\n# Foo MCP\n\nSomething.\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(stats.skipped.contains(&"Foo-MCP".to_string()));
    assert!(!stats.deployed.contains(&"Foo-MCP".to_string()));
    assert!(!tgt.path().join("Foo-MCP").exists());
}

#[test]
fn deploy_missing_source_dir_is_empty_result() {
    // Deploying from a non-existent source directory is a no-op success.
    let tgt = TempDir::new().unwrap();
    let stats = deploy_skills(Path::new("/nonexistent/trusty-mpm/skills"), tgt.path()).unwrap();
    assert_eq!(stats, DeployStats::default());
}

/// Write a multi-file skill: `<dir>/<stem>.md` plus `<dir>/<stem>/references/*.md`.
fn write_multi_file_skill(dir: &Path, stem: &str, refs: &[(&str, &str)]) {
    fs::write(
        dir.join(format!("{stem}.md")),
        format!("---\nname: {stem}\n---\n\nEntry point.\n"),
    )
    .unwrap();
    let refs_dir = dir.join(stem).join("references");
    fs::create_dir_all(&refs_dir).unwrap();
    for (name, body) in refs {
        fs::write(refs_dir.join(name), body).unwrap();
    }
}

#[test]
fn deploy_reference_files_land_alongside_skill() {
    // Issue #2903: a multi-file skill's references/*.md siblings must
    // deploy to <dest>/<stem>/references/<file> alongside SKILL.md.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_multi_file_skill(
        src.path(),
        "systematic-debugging",
        &[("workflow.md", "WORKFLOW"), ("examples.md", "EXAMPLES")],
    );

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(stats.deployed.contains(&"systematic-debugging".to_string()));
    assert!(
        stats
            .deployed
            .contains(&"systematic-debugging/references/workflow.md".to_string())
    );
    assert!(
        stats
            .deployed
            .contains(&"systematic-debugging/references/examples.md".to_string())
    );

    let workflow = fs::read_to_string(
        tgt.path()
            .join("systematic-debugging")
            .join("references")
            .join("workflow.md"),
    )
    .unwrap();
    assert_eq!(workflow, "WORKFLOW");
    let examples = fs::read_to_string(
        tgt.path()
            .join("systematic-debugging")
            .join("references")
            .join("examples.md"),
    )
    .unwrap();
    assert_eq!(examples, "EXAMPLES");
}

#[test]
fn deploy_reference_files_skip_user_modified() {
    // A user-edited reference file must be preserved, not overwritten,
    // exactly like a user-edited SKILL.md.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_multi_file_skill(src.path(), "systematic-debugging", &[("workflow.md", "V1")]);
    deploy_skills(src.path(), tgt.path()).unwrap();

    let ref_path = tgt
        .path()
        .join("systematic-debugging")
        .join("references")
        .join("workflow.md");
    fs::write(&ref_path, "USER HAND-EDIT").unwrap();

    // Source changes; user's edit must survive the redeploy.
    fs::write(
        src.path()
            .join("systematic-debugging")
            .join("references")
            .join("workflow.md"),
        "V2",
    )
    .unwrap();
    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(
        stats
            .skipped
            .contains(&"systematic-debugging/references/workflow.md".to_string())
    );
    assert_eq!(fs::read_to_string(&ref_path).unwrap(), "USER HAND-EDIT");
}

#[test]
fn deploy_reference_files_sync_even_when_entry_unchanged() {
    // A new reference file added to a skill whose SKILL.md content did
    // NOT change must still deploy — reference sync is independent of
    // whether the entry point itself changed this pass.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_multi_file_skill(src.path(), "systematic-debugging", &[]);
    deploy_skills(src.path(), tgt.path()).unwrap();

    // SKILL.md is untouched, but a reference file is added afterward.
    let refs_dir = src.path().join("systematic-debugging").join("references");
    fs::write(refs_dir.join("new-ref.md"), "NEW REF").unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(
        stats
            .unchanged
            .contains(&"systematic-debugging".to_string()),
        "entry point content is unchanged: {stats:?}"
    );
    assert!(
        stats
            .deployed
            .contains(&"systematic-debugging/references/new-ref.md".to_string())
    );
    assert!(
        tgt.path()
            .join("systematic-debugging")
            .join("references")
            .join("new-ref.md")
            .is_file()
    );
}

#[test]
fn deploy_single_file_skill_has_no_references_dir() {
    // A skill with no references/ subtree must not create an empty
    // references/ directory under the deployed skill.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md, no refs

    deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(!tgt.path().join("tm-doctor").join("references").exists());
}

#[test]
fn deploy_blocks_while_the_skill_ledger_lock_is_held() {
    // #4881: proves the WHOLE load-modify-save cycle is inside the lock, not
    // just the save. The main thread holds the target's ledger lock; a deploy
    // running concurrently must not finish, or write anything, until that lock
    // is released.
    //
    // Deterministic in both directions, with no reliance on scheduling luck:
    // an UNLOCKED deploy of two tiny files completes in well under a
    // millisecond, so the 2s "still blocked?" probe cannot pass by accident;
    // and a deploy that IS blocked on `flock` stays blocked for exactly as long
    // as the lock is held, so the locked version cannot fail by accident.
    use std::sync::mpsc;

    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    let (tx, rx) = mpsc::channel();
    let src_path = src.path().to_path_buf();
    let deploy_target = tgt.path().to_path_buf();
    let deployed_marker = tgt.path().join("tm-doctor").join("SKILL.md");

    // The handle leaves the critical section rather than being joined inside
    // it: the deploy cannot finish until we release, so joining under the lock
    // would deadlock this test rather than exercise it.
    let handle = crate::skills::manifest::with_skill_manifest_lock::<_, ManifestError, _>(
        tgt.path(),
        || {
            let handle = std::thread::spawn(move || {
                tx.send(deploy_skills(&src_path, &deploy_target).unwrap())
                    .ok();
            });

            assert!(
                rx.recv_timeout(std::time::Duration::from_secs(2)).is_err(),
                "deploy completed while the skill ledger lock was held — its \
                 load-modify-save is not inside the lock"
            );
            assert!(
                !deployed_marker.exists(),
                "deploy wrote a skill file while the ledger lock was held"
            );
            Ok(handle)
        },
    )
    .unwrap();

    handle.join().unwrap();
    let stats = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("deploy must complete once the lock is released");
    assert_eq!(stats.deployed.len(), 2);
    assert!(
        SkillManifest::load(tgt.path())
            .unwrap()
            .is_managed("tm-doctor"),
        "the released deploy must have recorded its entries"
    );
}

#[test]
fn a_racing_writer_freezes_nothing_on_either_side() {
    // #4881 — the invariant a save must uphold once files are on disk. This is
    // the regression test for a CAS that returned `Err` after `atomic_write`
    // had already published every `SKILL.md`: the bytes were newer than their
    // recorded checksums, so the next deploy read all of them as hand-edits and
    // skipped the whole tier forever — manufacturing the freeze this issue
    // exists to prevent.
    //
    // The interleaving is CONSTRUCTED, not raced: it replays the deployer's own
    // ordering (load, write files, save against the base loaded earlier) with a
    // competing writer publishing in between. The assertion that matters is
    // made by the REAL deployer afterwards — its classifier is what freezes.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // A first, uncontended deploy. This is the `base` an in-flight deploy loads.
    deploy_skills(src.path(), tgt.path()).unwrap();
    let base = SkillManifest::load(tgt.path()).unwrap();

    // A writer that does NOT take the ledger lock — an older installed `tm`
    // during a rollout — publishes its own entry after our base was loaded.
    let mut racer = base.clone();
    racer.managed.insert(
        "racing-writer-skill".into(),
        SkillManifestEntry {
            checksum: checksum("racer content"),
            deployed_at: "2026-08-05T00:00:00Z".into(),
        },
    );
    let racer_dir = tgt.path().join("racing-writer-skill");
    fs::create_dir_all(&racer_dir).unwrap();
    fs::write(racer_dir.join("SKILL.md"), "racer content").unwrap();
    racer.save(tgt.path()).unwrap();

    // Our in-flight deploy: new content for every source skill is written to
    // disk FIRST (as `deploy_one_file` does), then the ledger is saved against
    // the now-stale `base`.
    let mut ours = base.clone();
    for stem in ["tm-doctor", "example-skill"] {
        let content = format!("---\nname: {stem}\n---\n\nv2 content\n");
        let dir = tgt.path().join(stem);
        fs::write(dir.join("SKILL.md"), &content).unwrap();
        fs::write(src.path().join(format!("{stem}.md")), &content).unwrap();
        ours.managed.insert(
            stem.to_string(),
            SkillManifestEntry {
                checksum: checksum(&content),
                deployed_at: "2026-08-05T00:00:01Z".into(),
            },
        );
    }
    assert_eq!(
        ours.save_merging(tgt.path(), &base).unwrap(),
        SkillManifestSave::Merged
    );

    // Nothing is frozen on OUR side: the real deployer sees content it wrote,
    // matching its recorded checksum — `unchanged`, never `skipped`.
    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(
        stats.skipped.is_empty(),
        "a racing writer must freeze nothing: skipped={:?}",
        stats.skipped
    );
    assert_eq!(stats.unchanged.len(), 2, "both skills must stay writable");

    // Nothing is frozen on the RACER's side either: a blind save would have
    // dropped its entry, leaving its file untracked and skipped from then on.
    let final_manifest = SkillManifest::load(tgt.path()).unwrap();
    assert!(
        final_manifest.is_managed("racing-writer-skill"),
        "the racing writer's entry must survive, or ITS file freezes instead"
    );
    assert!(final_manifest.checksum_matches("racing-writer-skill", "racer content"));
}

#[test]
fn deploy_records_files_written_before_a_mid_loop_failure() {
    // #4881 review (pre-existing, fixed here): the write loop's `?` sites can
    // return AFTER earlier SKILL.md files are on disk. Propagating straight out
    // skipped the manifest save, leaving those files unrecorded — and the next
    // deploy then read them as hand-edits and skipped them forever. The ledger
    // must be saved on the error path too.
    //
    // The failure is deterministic: `aaa-skill` sorts first and deploys, then
    // `zzz-skill.md` holds invalid UTF-8, so `read_to_string` fails on it every
    // run. Invalid bytes rather than a chmod, so the test behaves the same when
    // the suite happens to run as root.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("aaa-skill.md"),
        "---\nname: aaa-skill\n---\n\nfirst\n",
    )
    .unwrap();
    fs::write(src.path().join("zzz-skill.md"), [0xff, 0xfe, 0xff]).unwrap();

    let err = deploy_skills(src.path(), tgt.path());
    assert!(err.is_err(), "an unreadable source must still fail the run");

    // The file that DID get written is on disk...
    let written = tgt.path().join("aaa-skill").join("SKILL.md");
    assert!(written.exists(), "aaa-skill was written before the failure");
    // ...and the ledger records it, so it is not orphaned.
    let manifest = SkillManifest::load(tgt.path()).unwrap();
    assert!(
        manifest.is_managed("aaa-skill"),
        "a file written before the failure must still be recorded"
    );

    // The proof that matters: a later deploy still owns it, rather than reading
    // it as a hand-edit and skipping it forever.
    fs::remove_file(src.path().join("zzz-skill.md")).unwrap();
    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(
        stats.skipped.is_empty(),
        "nothing may be frozen by a mid-loop failure: skipped={:?}",
        stats.skipped
    );
    assert!(stats.unchanged.contains(&"aaa-skill".to_string()));
}

// ---------------------------------------------------------------------------
// #4949 — directory-shaped skills at the user tier
// ---------------------------------------------------------------------------

/// Write a directory-shaped skill mirroring the real `duetto-design-system`.
fn write_directory_skill(dir: &Path, stem: &str) {
    let skill = dir.join(stem);
    fs::create_dir_all(skill.join("references")).unwrap();
    fs::write(skill.join("SKILL.md"), format!("# {stem}\n")).unwrap();
    fs::write(skill.join("metadata.json"), "{\"v\":1}\n").unwrap();
    fs::write(skill.join("references").join("tokens.md"), "# Tokens\n").unwrap();
    fs::write(skill.join("references").join("index.md"), "# Index\n").unwrap();
}

#[test]
fn deploy_directory_shaped_skill() {
    // #4949: `~/.trusty-mpm/skills/<stem>/SKILL.md` is a skill. Before the fix
    // the source scan's `is_file()` test rejected the directory outright and
    // the deploy reported success having written nothing.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_directory_skill(src.path(), "duetto-design-system");

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(
        stats.deployed.contains(&"duetto-design-system".to_string()),
        "the entry point must deploy: {stats:?}"
    );
    // Every file the skill carries lands at the destination.
    let dest = tgt.path().join("duetto-design-system");
    assert!(dest.join("SKILL.md").is_file(), "SKILL.md missing");
    assert!(
        dest.join("metadata.json").is_file(),
        "metadata.json missing"
    );
    assert!(
        dest.join("references").join("tokens.md").is_file(),
        "references/tokens.md missing"
    );
    assert!(
        dest.join("references").join("index.md").is_file(),
        "references/index.md missing"
    );
    assert!(
        stats.skipped.is_empty(),
        "nothing may be skipped: {stats:?}"
    );
}

#[test]
fn deploy_directory_skill_records_every_file_in_the_manifest() {
    // Ownership tracking must cover a multi-file skill, not just its entry
    // point — an unrecorded file reads as user-owned on the next pass and is
    // then frozen forever.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_directory_skill(src.path(), "duetto-design-system");

    deploy_skills(src.path(), tgt.path()).unwrap();

    let manifest = SkillManifest::load(tgt.path()).unwrap();
    for key in [
        "duetto-design-system",
        "duetto-design-system/metadata.json",
        "duetto-design-system/references/tokens.md",
        "duetto-design-system/references/index.md",
    ] {
        assert!(manifest.is_managed(key), "manifest missing key {key}");
    }

    // The proof the ledger is right: a second deploy changes nothing and
    // freezes nothing.
    let again = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(again.deployed.is_empty(), "second deploy wrote: {again:?}");
    assert!(again.skipped.is_empty(), "second deploy froze: {again:?}");
    assert_eq!(again.unchanged.len(), 4, "{again:?}");
}

#[test]
fn deploy_flat_skill_still_works_alongside_a_directory_skill() {
    // The pre-#4949 flat path must not regress, including its hybrid
    // `<stem>/references/` companion directory.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md
    fs::create_dir_all(src.path().join("tm-doctor").join("references")).unwrap();
    fs::write(
        src.path()
            .join("tm-doctor")
            .join("references")
            .join("cli.md"),
        "# CLI\n",
    )
    .unwrap();
    write_directory_skill(src.path(), "duetto-design-system");

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    for stem in ["tm-doctor", "example-skill", "duetto-design-system"] {
        assert!(
            tgt.path().join(stem).join("SKILL.md").is_file(),
            "{stem}/SKILL.md missing: {stats:?}"
        );
    }
    // The flat skill's companion reference file keeps its established key.
    assert!(
        tgt.path()
            .join("tm-doctor")
            .join("references")
            .join("cli.md")
            .is_file()
    );
    let manifest = SkillManifest::load(tgt.path()).unwrap();
    assert!(manifest.is_managed("tm-doctor/references/cli.md"));
}

#[test]
fn deploy_reports_a_skill_directory_with_no_entry_point() {
    // #4949's other half: a source name the deployer cannot use must be
    // REPORTED, not silently dropped. `stats.skipped` is the channel callers
    // already summarise for the user (`managed_config::skill_skip_summary`,
    // `tm sync-assets`), so the rejection rides it alongside a `tracing::warn!`.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::create_dir_all(src.path().join("broken-skill")).unwrap();
    fs::write(src.path().join("broken-skill").join("notes.md"), "x\n").unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(
        stats.skipped.contains(&"broken-skill".to_string()),
        "a malformed skill directory must be reported: {stats:?}"
    );
    assert!(stats.deployed.is_empty(), "{stats:?}");
    assert!(
        !tgt.path().join("broken-skill").exists(),
        "a malformed skill must not be partially written"
    );
}

#[test]
fn deploy_directory_skill_respects_the_select_predicate() {
    // Tier precedence drives deploys through `select`; a directory skill must
    // be selectable by the same stem the planner derives.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_directory_skill(src.path(), "duetto-design-system");
    write_directory_skill(src.path(), "cto-kb-ingest");

    let stats =
        deploy_skills_filtered(src.path(), tgt.path(), |name| name == "cto-kb-ingest").unwrap();

    assert!(stats.deployed.contains(&"cto-kb-ingest".to_string()));
    assert!(!tgt.path().join("duetto-design-system").exists());
}

#[test]
fn deploy_directory_skill_preserves_a_user_edited_reference() {
    // The ownership rule applies per file, so a hand-edited reference doc is
    // preserved while the rest of the skill still refreshes.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_directory_skill(src.path(), "duetto-design-system");
    deploy_skills(src.path(), tgt.path()).unwrap();

    let edited = tgt
        .path()
        .join("duetto-design-system")
        .join("references")
        .join("tokens.md");
    fs::write(&edited, "# Tokens (hand-edited)\n").unwrap();
    fs::write(
        src.path()
            .join("duetto-design-system")
            .join("references")
            .join("index.md"),
        "# Index v2\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(
        stats
            .skipped
            .contains(&"duetto-design-system/references/tokens.md".to_string()),
        "{stats:?}"
    );
    assert_eq!(
        fs::read_to_string(&edited).unwrap(),
        "# Tokens (hand-edited)\n"
    );
    assert!(
        stats
            .deployed
            .contains(&"duetto-design-system/references/index.md".to_string()),
        "{stats:?}"
    );
}
