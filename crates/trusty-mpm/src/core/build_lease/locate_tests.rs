//! Tests for `locate`: the freshest built binary across a temp pool (#10835).

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::*;

const SEEDED: &str = "#8261 builder slot pool\nstatus: seeded\nseed: ClonedFromShared\n";

/// A seeded `slot-<index>` under `repo_dir`.
fn seeded_slot(repo_dir: &Path, index: u32) -> PathBuf {
    let slot = repo_dir.join(format!("slot-{index}"));
    std::fs::create_dir_all(&slot).expect("slot");
    std::fs::write(slot.join(".trusty-slot-seeded"), SEEDED).expect("marker");
    slot
}

/// Write `<dir>/<profile>/<bin>` with an mtime `age_secs` in the past.
fn artifact(dir: &Path, profile: &str, bin: &str, age_secs: u64) -> PathBuf {
    let path = dir
        .join(profile)
        .join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
    std::fs::create_dir_all(path.parent().expect("parent")).expect("profile dir");
    std::fs::write(&path, b"bin").expect("artifact");
    let file = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open");
    file.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
        .expect("mtime");
    path
}

#[test]
fn the_newest_slot_artifact_wins() {
    let tmp = tempfile::tempdir().expect("tmp");
    let repo = tmp.path().join("pool/acme/widget");
    let env = tmp.path().join("envrc-target");
    artifact(&env, "debug", "tm", 3_000);
    artifact(&seeded_slot(&repo, 0), "debug", "tm", 10);
    artifact(&seeded_slot(&repo, 1), "debug", "tm", 600);
    artifact(&seeded_slot(&repo, 2), "debug", "tm", 1_200);

    let candidates = candidate_dirs(tmp.path(), Some(&env), None, Some(&repo));
    let found = freshest(&candidates, "tm", "debug").expect("found");

    assert_eq!(found.source, "slot-0");
    assert_eq!(found.path, repo.join("slot-0/debug/tm"));
    assert_eq!(found.target_dir, repo.join("slot-0"));
}

#[test]
fn a_missing_binary_names_every_dir_searched() {
    let tmp = tempfile::tempdir().expect("tmp");
    let repo = tmp.path().join("pool/acme/widget");
    let env = tmp.path().join("envrc-target");
    artifact(&seeded_slot(&repo, 0), "debug", "other", 10);

    let candidates = candidate_dirs(tmp.path(), Some(&env), None, Some(&repo));
    let searched = freshest(&candidates, "tm", "debug").expect_err("absent");

    assert_eq!(
        searched,
        vec![env.join("debug/tm"), repo.join("slot-0/debug/tm")]
    );
}

#[test]
fn the_profile_selects_the_artifact_directory() {
    let tmp = tempfile::tempdir().expect("tmp");
    let repo = tmp.path().join("pool/acme/widget");
    let slot0 = seeded_slot(&repo, 0);
    let slot1 = seeded_slot(&repo, 1);
    artifact(&slot0, "debug", "tm", 10);
    artifact(&slot1, "release", "tm", 900);

    let candidates = candidate_dirs(tmp.path(), None, None, Some(&repo));

    let release = freshest(&candidates, "tm", "release").expect("release");
    assert_eq!(release.path, slot1.join("release/tm"));
    let debug = freshest(&candidates, "tm", "debug").expect("debug");
    assert_eq!(debug.path, slot0.join("debug/tm"));
}

#[test]
fn candidates_put_the_target_in_effect_first_and_skip_unseeded_slots() {
    let tmp = tempfile::tempdir().expect("tmp");
    let repo = tmp.path().join("pool/acme/widget");
    seeded_slot(&repo, 3);
    std::fs::create_dir_all(repo.join("slot-1")).expect("unseeded slot");
    let shared = tmp.path().join("shared");

    let unset = candidate_dirs(tmp.path(), None, Some(&shared), Some(&repo));
    let sources: Vec<&str> = unset.iter().map(|c| c.source.as_str()).collect();
    assert_eq!(sources, ["checkout target/", "shared target dir", "slot-3"]);

    // The `.envrc` value and the resolved shared dir are one directory.
    let set = candidate_dirs(tmp.path(), Some(&shared), Some(&shared), Some(&repo));
    let sources: Vec<&str> = set.iter().map(|c| c.source.as_str()).collect();
    assert_eq!(sources, ["CARGO_TARGET_DIR", "slot-3"]);
}

#[test]
fn only_seeded_slots_are_searched_10835() {
    let tmp = tempfile::tempdir().expect("tmp");
    let repo = tmp.path().join("pool/acme/widget");
    artifact(&seeded_slot(&repo, 0), "debug", "tm", 900);
    let unseeded = repo.join("slot-1");
    artifact(&unseeded, "debug", "tm", 1);

    let candidates = candidate_dirs(tmp.path(), None, None, Some(&repo));
    let found = freshest(&candidates, "tm", "debug").expect("found");

    assert_eq!(found.source, "slot-0", "a half-seeded slot is never chosen");
}

#[test]
fn the_located_json_names_path_source_dir_and_mtime() {
    let tmp = tempfile::tempdir().expect("tmp");
    let repo = tmp.path().join("pool/acme/widget");
    let path = artifact(&seeded_slot(&repo, 2), "debug", "tm", 60);

    let candidates = candidate_dirs(tmp.path(), None, None, Some(&repo));
    let found = freshest(&candidates, "tm", "debug").expect("found");
    let json = serde_json::to_value(&found).expect("json");

    assert_eq!(json["path"], path.display().to_string());
    assert_eq!(json["source"], "slot-2");
    assert_eq!(
        json["target_dir"],
        repo.join("slot-2").display().to_string()
    );
    let mtime = json["mtime_unix"].as_u64().expect("mtime is an integer");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_secs();
    assert!((now - 70..=now - 50).contains(&mtime), "{mtime} vs {now}");
}

#[test]
fn a_binary_name_with_a_path_is_rejected() {
    assert!(is_bare_name("tm"));
    for bad in ["", ".", "..", "../tm", "debug/tm"] {
        assert!(!is_bare_name(bad), "{bad:?}");
    }
}
