//! Regression tests for the `errors.jsonl` size bound and corruption fixes
//! (#8028). The older round-trip tests stay inline in `store.rs`.

use std::path::Path;
use std::sync::{Arc, Barrier};

use crate::error_capture::rotation::{self, RotationPolicy};
use crate::error_capture::store::ErrorStore;
use crate::error_capture::types::CapturedError;

fn record(msg: &str) -> CapturedError {
    CapturedError {
        timestamp_secs: 1_000_000,
        crate_target: "test_crate".to_string(),
        crate_version: "0.1.0".to_string(),
        message: msg.to_string(),
        fields: String::new(),
        file: Some("src/lib.rs".to_string()),
        line: Some(10),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        fingerprint: "fp".to_string(),
    }
}

/// Every non-empty line of `path` split on raw `\n` bytes.
fn raw_lines(path: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).unwrap_or_default();
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

/// #8028: separate store handles on one path stand in for separate daemon
/// and CLI processes, which share `errors.jsonl` with no common mutex. A
/// record written as two `write(2)` calls (body, then `\n`) lets another
/// writer's body land between them and fuse two records into one line.
#[test]
fn concurrent_writers_produce_only_well_formed_lines() {
    const WRITERS: usize = 8;
    const PER_WRITER: usize = 200;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let barrier = Arc::new(Barrier::new(WRITERS));
    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let store = ErrorStore::with_path(Some(path), 10);
                let body = "x".repeat(512);
                barrier.wait();
                for i in 0..PER_WRITER {
                    store.append(record(&format!("w{w}-{i}-{body}")));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("writer thread");
    }

    let lines = raw_lines(&path);
    let malformed = lines
        .iter()
        .filter(|l| serde_json::from_slice::<CapturedError>(l).is_err())
        .count();
    assert_eq!(malformed, 0, "interleaved writes produced malformed lines");
    assert_eq!(
        lines.len(),
        WRITERS * PER_WRITER,
        "every record is one line"
    );
}

/// #8028: one line of invalid UTF-8 (a torn multi-byte write) used to fail
/// `read_to_string` for the whole file, so the reader returned nothing.
#[test]
fn malformed_line_does_not_break_reading() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let mut bytes = serde_json::to_vec(&record("before")).expect("json");
    bytes.extend_from_slice(b"\n{\"message\":\"torn \xE2\x82\n");
    bytes.extend_from_slice(&serde_json::to_vec(&record("after")).expect("json"));
    bytes.push(b'\n');
    std::fs::write(&path, bytes).expect("seed");

    let records = ErrorStore::read_records(&path, 10);
    let messages: Vec<&str> = records.iter().map(|r| r.message.as_str()).collect();
    assert_eq!(messages, ["before", "after"]);
}

/// Serialised length of one test record, newline included.
fn line_len(rec: &CapturedError) -> u64 {
    serde_json::to_vec(rec).expect("json").len() as u64 + 1
}

/// Total bytes of every `errors.jsonl*` file in `dir`, the lock sidecar aside.
fn store_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("errors.jsonl") && !name.ends_with(".lock")
        })
        .map(|e| e.metadata().expect("metadata").len())
        .sum()
}

/// #8028: writing well past the cap rotates, keeps at most `keep` rotated
/// files, holds total disk use to `(keep + 1) * (cap + one record)`, and a
/// reopened store still reads the newest records across the rotated files.
#[test]
fn writing_past_the_cap_rotates_and_bounds_disk_use() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let policy = RotationPolicy {
        max_bytes: 4096,
        keep: 2,
    };
    let store = ErrorStore::with_path_and_rotation(Some(path.clone()), 10, policy);
    let rec_len = line_len(&record(&format!("m{:04}-{}", 0, "y".repeat(400))));
    for i in 0..200 {
        store.append(record(&format!("m{i:04}-{}", "y".repeat(400))));
    }
    let written = 200 * rec_len;

    let bound = 3 * (policy.max_bytes + rec_len);
    let on_disk = store_bytes(dir.path());
    assert!(
        on_disk <= bound,
        "on-disk {on_disk} exceeds bound {bound} after writing {written}"
    );
    assert!(rotation::rotated_path(&path, 2).exists(), "rotation ran");
    assert!(!rotation::rotated_path(&path, 3).exists(), "keep=2 holds");
    for file in rotation::files_newest_first(&path, policy) {
        for line in raw_lines(&file) {
            assert!(serde_json::from_slice::<CapturedError>(&line).is_ok());
        }
    }

    // `.1` and `.2` each hold at least 4096 / rec_len (~7) records, so 12 of
    // the newest records always span more than the live file.
    let reopened = ErrorStore::with_path_and_rotation(Some(path), 12, policy);
    let recent = reopened.recent_errors(12);
    let live_count = raw_lines(&dir.path().join("errors.jsonl")).len();
    assert!(
        live_count < 12,
        "live holds {live_count}; the read must span files"
    );
    assert_eq!(recent.len(), 12, "reader spans the rotated files");
    assert!(recent[11].message.starts_with("m0199-"), "newest last");
    assert!(recent[0].message.starts_with("m0188-"), "oldest first");
}

/// #8028: a pre-fix file far over the cap keeps only its tail when rotated,
/// written to a temp file and renamed, so no rotated slot stays unbounded.
#[test]
fn oversized_legacy_file_is_compacted_on_rotation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let policy = RotationPolicy {
        max_bytes: 4096,
        keep: 2,
    };
    let mut legacy = Vec::new();
    for i in 0..500 {
        legacy.extend(serde_json::to_vec(&record(&format!("old{i:03}"))).expect("json"));
        legacy.push(b'\n');
    }
    assert!(legacy.len() as u64 > 10 * policy.max_bytes);
    std::fs::write(&path, &legacy).expect("seed legacy file");

    let store = ErrorStore::with_path_and_rotation(Some(path.clone()), 5, policy);
    store.append(record("fresh"));

    let first = rotation::rotated_path(&path, 1);
    let first_len = std::fs::metadata(&first).expect(".1 exists").len();
    assert!(first_len <= policy.max_bytes, ".1 holds {first_len} bytes");
    let tail = raw_lines(&first);
    assert!(!tail.is_empty());
    for line in &tail {
        assert!(serde_json::from_slice::<CapturedError>(line).is_ok());
    }
    let last: CapturedError = serde_json::from_slice(&tail[tail.len() - 1]).expect("json");
    assert_eq!(last.message, "old499", "the tail keeps the newest records");
    let live: Vec<CapturedError> = raw_lines(&path)
        .iter()
        .map(|l| serde_json::from_slice(l).expect("json"))
        .collect();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].message, "fresh");
    assert!(
        !dir.path().join("errors.jsonl.1.tmp").exists(),
        "temp renamed away"
    );
}

/// #8028 fail-open arm. A directory squatting on `errors.jsonl.1` makes the
/// rotation rename fail. The store must refuse the disk write and count it
/// rather than append past the cap, keep every record in memory, and resume
/// disk writes once rotation succeeds again. Mutation this catches: falling
/// through to `serialise_and_append` after a rotation error.
#[test]
fn a_rotation_failure_refuses_the_disk_write_and_counts_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let blocker = rotation::rotated_path(&path, 1);
    std::fs::create_dir(&blocker).expect("blocker dir");
    std::fs::write(blocker.join("occupant"), b"x").expect("occupant");
    let policy = RotationPolicy {
        max_bytes: 2048,
        keep: 1,
    };
    let store = ErrorStore::with_path_and_rotation(Some(path.clone()), 100, policy);
    let rec_len = line_len(&record(&format!("r{:02}-{}", 0, "z".repeat(300))));
    for i in 0..40 {
        store.append(record(&format!("r{i:02}-{}", "z".repeat(300))));
    }

    let live_len = std::fs::metadata(&path).expect("live").len();
    assert!(
        live_len < policy.max_bytes + rec_len,
        "live file grew to {live_len} past the cap"
    );
    assert!(store.refused_disk_writes() > 0, "refusals are counted");
    assert_eq!(store.len(), 40, "logging continues in memory");

    std::fs::remove_dir_all(&blocker).expect("clear blocker");
    store.append(record("after-recovery"));
    assert!(blocker.is_file(), "rotation resumed");
    let live = raw_lines(&path);
    assert_eq!(live.len(), 1, "disk writes resumed on a fresh live file");
}
