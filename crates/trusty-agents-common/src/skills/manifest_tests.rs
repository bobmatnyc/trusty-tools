//! Tests for `skills::manifest` — split out to mirror the
//! `deployer`/`deployer_tests` pattern the rest of this module already uses,
//! and to keep `manifest.rs` under the 500-SLOC production cap once the #4881
//! ledger lock and its concurrency tests landed.
//!
//! Why: moved verbatim from `manifest.rs`'s inline `#[cfg(test)] mod tests` —
//! a behavior-preserving extraction, not a rewrite — plus the #4881 lock and
//! merging-save coverage.
//! What: covers load-of-missing, round-trip save/load, checksum matching, the
//! ledger lock's serialisation and release, and the merging save's handling of
//! a concurrent writer, of this run's removals, and of an unreadable ledger.
//! Test: this file IS the test module for `manifest`; run with
//! `cargo test -p trusty-agents-common -- skills::manifest`.

use super::*;
use tempfile::TempDir;

fn sample_entry() -> SkillManifestEntry {
    SkillManifestEntry {
        checksum: checksum("hello world"),
        deployed_at: "2026-05-19T00:00:00Z".into(),
    }
}

#[test]
fn skill_manifest_load_missing_returns_empty() {
    // A directory with no manifest file must yield an empty, valid
    // manifest rather than an error. #5626 tightened the error arms around
    // this case; the first-ever deploy must stay a silent success.
    let tmp = TempDir::new().unwrap();
    let manifest = SkillManifest::load(tmp.path()).expect("an absent ledger is not an error");
    assert_eq!(manifest.version, SKILL_MANIFEST_VERSION);
    assert!(manifest.managed.is_empty());
}

#[test]
fn skill_manifest_load_malformed_is_an_error() {
    // #5626: a torn or hand-mangled ledger must NOT read as "nothing is
    // owned here" — that licenses the deployer to write over every managed
    // skill and record none of it.
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(SKILL_MANIFEST_FILE), b"not valid json{{{").unwrap();
    let err = SkillManifest::load(tmp.path()).expect_err("a malformed ledger must be an error");
    assert!(
        matches!(err, ManifestError::Json(_)),
        "expected a Json error, got {err:?}"
    );
}

#[test]
fn skill_manifest_load_truncated_is_an_error() {
    // A crash mid-write leaves valid JSON prefix and nothing else. It is the
    // shape `save_merging` already refuses to merge from (#4881); `load` now
    // refuses it too (#5626).
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join(SKILL_MANIFEST_FILE),
        b"{\"version\":1,\"managed\":{",
    )
    .unwrap();
    assert!(
        SkillManifest::load(tmp.path()).is_err(),
        "a truncated ledger must be an error"
    );
}

#[test]
#[cfg(unix)]
fn skill_manifest_load_unreadable_is_an_error() {
    // #5626, ADR-0045: EACCES is the arm the old `Err(_) => Self::default()`
    // swallowed. The ledger is present and holds an entry; the process simply
    // cannot read it. Reporting an empty ledger here asserts an absence the
    // read never established.
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let mut manifest = SkillManifest::default();
    manifest.managed.insert("tm-doctor".into(), sample_entry());
    manifest.save(tmp.path()).unwrap();

    let path = tmp.path().join(SKILL_MANIFEST_FILE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let loaded = SkillManifest::load(tmp.path());
    // Restore before asserting so a failure still leaves a removable TempDir.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let err = loaded.expect_err("an unreadable ledger must be an error, not an empty one");
    assert!(
        matches!(&err, ManifestError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied),
        "expected a PermissionDenied Io error, got {err:?}"
    );
    assert!(
        err.to_string().contains(SKILL_MANIFEST_FILE),
        "the error must name the ledger it could not read: {err}"
    );
}

#[test]
fn skill_manifest_deploy_refuses_a_ledger_it_could_not_read() {
    // #5626: the consumer half, and the sharper of the two outcomes. On the
    // empty default the deploy did not merely mis-classify — it RAN, skipped
    // every managed skill as untracked, and then `save_merging` took its
    // `OverwroteUnreadable` arm and published this run's near-empty ledger over
    // the unreadable one. The entries it held are then gone for good, and the
    // files they described are frozen against every future update (#4881's
    // shape). Post-fix the deploy stops at the load, so the bytes on disk are
    // untouched and an operator can still repair them.
    let src = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    std::fs::write(src.path().join("tm-doctor.md"), "v1").unwrap();

    crate::skills::deployer::deploy_skills(src.path(), dest.path()).unwrap();
    let ledger = dest.path().join(SKILL_MANIFEST_FILE);
    assert!(
        SkillManifest::load(dest.path())
            .unwrap()
            .is_managed("tm-doctor"),
        "the first deploy must record the skill"
    );

    // Corrupt the ledger the way a crash mid-write does, keeping enough of the
    // document that its loss is visible.
    let corrupt = "{\"version\":1,\"managed\":{\"tm-doctor\":";
    std::fs::write(&ledger, corrupt).unwrap();
    std::fs::write(src.path().join("tm-doctor.md"), "v2").unwrap();

    let result = crate::skills::deployer::deploy_skills(src.path(), dest.path());

    assert!(
        result.is_err(),
        "the deploy must refuse rather than act as if nothing is owned"
    );
    assert_eq!(
        std::fs::read_to_string(&ledger).unwrap(),
        corrupt,
        "a refused deploy must leave the ledger exactly as it found it"
    );
    assert_eq!(
        std::fs::read_to_string(dest.path().join("tm-doctor").join("SKILL.md")).unwrap(),
        "v1",
        "a refused deploy writes no skill content either"
    );
}

#[test]
fn skill_manifest_round_trip() {
    // A saved manifest must reload identically.
    let tmp = TempDir::new().unwrap();
    let mut manifest = SkillManifest::default();
    manifest
        .managed
        .insert("tm-doctor.md".into(), sample_entry());
    manifest.save(tmp.path()).unwrap();

    let loaded = SkillManifest::load(tmp.path()).unwrap();
    assert_eq!(loaded, manifest);
    assert!(tmp.path().join(SKILL_MANIFEST_FILE).exists());
}

#[test]
fn skill_manifest_checksum_matches() {
    // Correct content matches; modified content does not.
    let mut manifest = SkillManifest::default();
    manifest
        .managed
        .insert("tm-doctor.md".into(), sample_entry());
    assert!(manifest.checksum_matches("tm-doctor.md", "hello world"));
    assert!(!manifest.checksum_matches("tm-doctor.md", "hello world!"));
    // An unmanaged file never matches.
    assert!(!manifest.checksum_matches("other.md", "hello world"));
}

#[test]
fn skill_manifest_is_managed() {
    let mut manifest = SkillManifest::default();
    manifest
        .managed
        .insert("tm-doctor.md".into(), sample_entry());
    assert!(manifest.is_managed("tm-doctor.md"));
    assert!(!manifest.is_managed("user-skill.md"));
}

#[test]
fn skill_manifest_file_name_differs_from_agent_manifest() {
    // The skill manifest must use a distinct filename so it never collides
    // with the agent manifest if both ever share a directory.
    assert_ne!(SKILL_MANIFEST_FILE, crate::agents::manifest::MANIFEST_FILE);
}

#[test]
fn skill_manifest_lock_path_is_a_sidecar() {
    // #4881: the lock must be a stable sibling, not the ledger itself — a lock
    // on the document is discarded by the rename that publishes each version.
    let dir = Path::new("/some/skills");
    assert_eq!(
        skill_manifest_lock_path(dir),
        dir.join(".trusty-mpm-skills-manifest.json.lock")
    );
    // And it must not collide with the agent ledger's sidecar if the two ever
    // share a directory.
    assert_ne!(
        skill_manifest_lock_path(dir),
        crate::agents::manifest::manifest_lock_path(dir)
    );
}

#[test]
fn skill_manifest_lock_serialises_concurrent_writers() {
    // #4881: the load-modify-save cycle must be serialised, or two writers that
    // both load before either saves silently drop each other's entries — and a
    // skill whose entry was lost then reads as hand-edited and freezes.
    //
    // The interleaving is FORCED, not hoped for: each thread sleeps 5ms while
    // holding its loaded snapshot, which is four orders of magnitude longer
    // than the load itself, so without the lock every thread is guaranteed to
    // have loaded before any thread saves and exactly one entry survives.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    SkillManifest::default().save(&dir).unwrap();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                with_skill_manifest_lock::<(), ManifestError, _>(&dir, || {
                    let mut m = SkillManifest::load(&dir).unwrap();
                    m.managed.insert(format!("skill-{i}"), sample_entry());
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    m.save(&dir)
                })
                .unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let final_manifest = SkillManifest::load(&dir).unwrap();
    assert_eq!(
        final_manifest.managed.len(),
        8,
        "every writer's entry must survive: {:?}",
        final_manifest.managed.keys().collect::<Vec<_>>()
    );
}

#[test]
fn skill_manifest_lock_releases_so_a_second_acquisition_succeeds() {
    // The lock is RAII: a completed critical section must not leave the
    // directory locked, or the next deploy in this process deadlocks.
    let tmp = TempDir::new().unwrap();
    for _ in 0..3 {
        with_skill_manifest_lock::<(), ManifestError, _>(tmp.path(), || Ok(())).unwrap();
    }
}

#[test]
fn skill_manifest_save_merging_folds_in_a_concurrent_writer() {
    // #4881: a writer whose snapshot went stale must publish a document holding
    // BOTH its own delta and the racing writer's — never one at the other's
    // expense, and never nothing. Staleness is constructed directly; no
    // scheduling is involved.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    // The snapshot both writers start from.
    let mut base = SkillManifest::default();
    base.managed.insert("shared".into(), sample_entry());
    base.save(dir).unwrap();

    // A concurrent writer publishes an entry our snapshot never saw.
    let mut newer = base.clone();
    newer
        .managed
        .insert("from-the-other-writer".into(), sample_entry());
    newer.save(dir).unwrap();

    // Our own edit, computed against the now-stale `base`.
    let mut ours = base.clone();
    ours.managed.insert("ours".into(), sample_entry());

    assert_eq!(
        ours.save_merging(dir, &base).unwrap(),
        SkillManifestSave::Merged
    );

    let on_disk = SkillManifest::load(dir).unwrap();
    assert!(on_disk.is_managed("ours"), "our own entry must be recorded");
    assert!(
        on_disk.is_managed("from-the-other-writer"),
        "the racing writer's entry must survive"
    );
    assert!(on_disk.is_managed("shared"));
}

#[test]
fn skill_manifest_save_merging_applies_this_runs_removals() {
    // The prune path's delta is REMOVALS. Merging must apply them to the
    // current on-disk document, not silently revert them, and must still leave
    // a concurrent writer's inserts alone.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    let mut base = SkillManifest::default();
    base.managed.insert("keep".into(), sample_entry());
    base.managed.insert("prune-me".into(), sample_entry());
    base.save(dir).unwrap();

    let mut newer = base.clone();
    newer.managed.insert("arrived-later".into(), sample_entry());
    newer.save(dir).unwrap();

    // Our delta: drop `prune-me`.
    let mut ours = base.clone();
    ours.managed.remove("prune-me");

    assert_eq!(
        ours.save_merging(dir, &base).unwrap(),
        SkillManifestSave::Merged
    );

    let on_disk = SkillManifest::load(dir).unwrap();
    assert!(!on_disk.is_managed("prune-me"), "our removal must apply");
    assert!(on_disk.is_managed("keep"));
    assert!(
        on_disk.is_managed("arrived-later"),
        "the racing writer's insert must survive a prune merge"
    );
}

#[test]
fn skill_manifest_save_merging_writes_when_unchanged() {
    // The uncontended path: an unraced save publishes as-is, and a first-ever
    // save (no file on disk, snapshot = empty default) publishes too.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    let base = SkillManifest::default();
    let mut first = base.clone();
    first.managed.insert("tm-doctor".into(), sample_entry());
    assert_eq!(
        first.save_merging(dir, &base).unwrap(),
        SkillManifestSave::Written
    );
    assert!(SkillManifest::load(dir).unwrap().is_managed("tm-doctor"));

    let base = SkillManifest::load(dir).unwrap();
    let mut second = base.clone();
    second.managed.insert("tm-workflow".into(), sample_entry());
    assert_eq!(
        second.save_merging(dir, &base).unwrap(),
        SkillManifestSave::Written
    );
    let on_disk = SkillManifest::load(dir).unwrap();
    assert!(on_disk.is_managed("tm-doctor"));
    assert!(on_disk.is_managed("tm-workflow"));
}

#[test]
fn skill_manifest_save_merging_over_a_corrupt_ledger_keeps_the_base() {
    // #4881 review: `load` maps an UNPARSEABLE ledger to the empty default, so
    // merging from it would publish only this run's delta and silently drop
    // everything `base` holds — unreadable treated as absent, the same fail-open
    // shape the merge exists to remove. The base's entries must survive.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    let mut base = SkillManifest::default();
    for i in 0..9 {
        base.managed.insert(format!("s{i}"), sample_entry());
    }
    base.save(dir).unwrap();

    // The ledger becomes unparseable after this run loaded its base.
    std::fs::write(dir.join(SKILL_MANIFEST_FILE), "{ not json").unwrap();

    let mut ours = base.clone();
    ours.managed.insert("ours".into(), sample_entry());

    assert_eq!(
        ours.save_merging(dir, &base).unwrap(),
        SkillManifestSave::OverwroteUnreadable
    );

    let on_disk = SkillManifest::load(dir).unwrap();
    assert_eq!(
        on_disk.managed.len(),
        10,
        "the base's 9 entries plus ours must survive, not just ours: {:?}",
        on_disk.managed.keys().collect::<Vec<_>>()
    );
    for i in 0..9 {
        assert!(on_disk.is_managed(&format!("s{i}")));
    }
    assert!(on_disk.is_managed("ours"));
}
