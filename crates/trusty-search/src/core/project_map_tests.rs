//! `project-map.toml` model and I/O (#9169 PR-B1).
//!
//! Why: the map is persisted state that pins and parks real indexes; these
//! cases pin the fail-closed read, the locked read-modify-write and the
//! quarantine the operator's rebuild uses.
//! Test: this module.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    ParkReason, ParkedIndex, PinnedBy, PinnedProject, ProjectMap, ProjectMapError, PROJECT_MAP_FILE,
};
use crate::core::project_map_io::{load, map_path_beside, quarantine_and_rebuild, update};

fn pin(key: &str, id: &str) -> PinnedProject {
    PinnedProject {
        key: key.to_string(),
        index_id: id.to_string(),
        root_path: PathBuf::from(format!("/w/{id}")),
        pinned_by: PinnedBy::Operator,
        pinned_unix: 7,
    }
}

fn map_in(dir: &tempfile::TempDir) -> PathBuf {
    map_path_beside(&dir.path().join("indexes.toml"))
}

/// Why: the map sits beside `indexes.toml`, and a missing one is the only
/// read that is not an error.
/// Test: this test.
#[test]
fn a_missing_map_reads_as_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = map_in(&dir);
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some(PROJECT_MAP_FILE)
    );
    assert!(load(&path).expect("missing is not an error").is_none());
}

/// Why: #9169 fail closed — garbage, a file cut off mid-write and a schema
/// from a newer build must each be an error naming the file and the remedy,
/// and no write may land on top of them.
/// Test: this test.
#[test]
fn a_corrupt_truncated_or_future_map_fails_closed_and_keeps_its_bytes() {
    let good = toml::to_string_pretty(&ProjectMap {
        projects: vec![pin("bobmatnyc/apex", "apex")],
        ..ProjectMap::default()
    })
    .expect("serialise");
    let truncated = good[..good.len() / 2].to_string();
    let future = good.replacen("schema = 1", "schema = 2", 1);
    for (case, bytes) in [
        ("garbage", "this is [[ not a map".to_string()),
        ("truncated", truncated),
        ("future", future),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = map_in(&dir);
        std::fs::write(&path, &bytes).expect("seed");
        let err = load(&path).expect_err(case);
        let text = err.to_string();
        assert!(text.contains(&path.display().to_string()), "{case}: {text}");
        assert!(text.contains("rebuild-map"), "{case}: {text}");
        if case == "future" {
            assert!(
                matches!(err, ProjectMapError::Schema { found: 2, .. }),
                "{err:?}"
            );
        }
        let wrote = update(&path, |map| {
            let mut map = map.unwrap_or_default();
            map.set_pin(pin("k", "x"));
            Ok((Some(map), ()))
        });
        assert!(wrote.is_err(), "{case}: an update over a bad map must fail");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            bytes,
            "{case}"
        );
    }
}

/// Why: same race #4871 fixed for `indexes.toml` — every writer's load and
/// save must be one critical section, or concurrent pins vanish.
/// Test: this test.
#[test]
fn concurrent_map_writers_lose_no_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = Arc::new(map_in(&dir));
    let writers: Vec<_> = (0..16)
        .map(|i| {
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                update(&path, |map| {
                    let mut map = map.unwrap_or_default();
                    map.set_pin(pin(&format!("key-{i}"), &format!("idx-{i}")));
                    Ok((Some(map), ()))
                })
                .expect("update");
            })
        })
        .collect();
    for w in writers {
        w.join().expect("writer");
    }
    let map = load(&path).expect("load").expect("written");
    assert_eq!(map.projects.len(), 16, "{:?}", map.projects);
}

/// Why: the CLI writes the map from another process, so a writer must wait
/// on the `.lock` sidecar another descriptor holds, never bypass it.
/// What: a second descriptor takes the sidecar lock and holds it; an update
/// started meanwhile publishes only after the holder releases.
/// Test: this test.
#[test]
fn a_held_cross_process_lock_blocks_a_writer_until_released() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = map_in(&dir);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let holder_path = path.clone();
    let holder = std::thread::spawn(move || {
        trusty_common::file_lock::with_exclusive_lock(&holder_path, || {
            held_tx.send(()).expect("signal");
            std::thread::sleep(Duration::from_millis(400));
        })
        .expect("hold the sidecar lock");
    });
    held_rx.recv().expect("holder acquired");
    let started = Instant::now();
    update(&path, |map| {
        let mut map = map.unwrap_or_default();
        map.set_pin(pin("k", "x"));
        Ok((Some(map), ()))
    })
    .expect("update after release");
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "the writer must wait for the holder: {:?}",
        started.elapsed()
    );
    holder.join().expect("holder");
    assert_eq!(load(&path).expect("load").expect("map").projects.len(), 1);
}

/// Why: rebuild keeps the bad bytes as evidence and publishes the new map in
/// the same critical section.
/// Test: this test.
#[test]
fn quarantine_moves_the_file_aside_and_publishes_the_rebuild() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = map_in(&dir);
    std::fs::write(&path, "garbage [[").expect("seed");
    let (moved, map) =
        quarantine_and_rebuild(&path, 42, || Ok(ProjectMap::default())).expect("rebuild");
    let aside = moved.expect("the old file was moved");
    assert!(aside
        .to_string_lossy()
        .ends_with("project-map.toml.corrupt-42"));
    assert_eq!(
        std::fs::read_to_string(&aside).expect("aside"),
        "garbage [["
    );
    assert_eq!(load(&path).expect("load"), Some(map));
}

/// Why: the map's own edits are what park, unpark and pin rely on.
/// Test: this test.
#[test]
fn pin_park_and_unpark_edit_one_row_each() {
    let mut map = ProjectMap::default();
    assert_eq!(map.set_pin(pin("k", "a")), None);
    assert_eq!(map.set_pin(pin("k", "b")).as_deref(), Some("a"));
    assert_eq!(map.projects.len(), 1);
    let row = ParkedIndex {
        index_id: "wt".to_string(),
        key: "k".to_string(),
        root_path: PathBuf::from("/w/wt"),
        reason: ParkReason::Worktree,
        parked_unix: 1,
    };
    map.park(row.clone());
    map.park(row.clone());
    assert_eq!(map.parked.len(), 1);
    assert!(map.view().is_parked("wt"));
    assert_eq!(map.view().pin("k"), Some("b"));
    assert_eq!(map.unpark("wt"), Some(row));
    assert_eq!(map.unpark("wt"), None);
}
