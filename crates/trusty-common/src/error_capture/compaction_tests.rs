//! #8028 part 2: legacy compaction loses no concurrent record, fails open on
//! every branch, and a read overlapping a rotation counts each record once.
//! Interleavings are driven by `test_hook`, never by sleeps.

use std::cell::Cell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::error_capture::rotation::{self, RotationPolicy};
use crate::error_capture::store::{ErrorStore, write_record_line};
use crate::error_capture::test_hook::{self, Point};
use crate::error_capture::types::CapturedError;

const POLICY: RotationPolicy = RotationPolicy {
    max_bytes: 4096,
    keep: 1,
};

fn record(msg: &str) -> CapturedError {
    CapturedError {
        timestamp_secs: 1_000_000,
        crate_target: "test_crate".to_string(),
        crate_version: "0.1.0".to_string(),
        message: msg.to_string(),
        fields: String::new(),
        file: None,
        line: None,
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        fingerprint: "fp".to_string(),
    }
}

/// A pre-#8028 file far past the read limit, `count` records named `<prefix>NNN`.
fn legacy_bytes(prefix: &str, count: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..count {
        write_record_line(&mut out, &record(&format!("{prefix}{i:03}"))).expect("encode");
    }
    assert!(out.len() as u64 > POLICY.read_limit());
    out
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Messages of every line in `files`; panics on a malformed line.
fn messages(files: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for file in files {
        let bytes = std::fs::read(file).unwrap_or_default();
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let rec: CapturedError = serde_json::from_slice(line).expect("well-formed line");
            out.push(rec.message);
        }
    }
    out
}

fn squat_dir(path: &Path) {
    std::fs::create_dir(path).expect("squatter dir");
    std::fs::write(path.join("occupant"), b"x").expect("occupant");
}

/// Append one record to `path` through a fresh `O_APPEND` open, as
/// `ErrorStore::append` does.
fn append_fresh(path: &Path, msg: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .expect("fresh open");
    write_record_line(&mut file, &record(msg)).expect("write");
}

/// #8028 HIGH 1: appends take no lock, so writers land mid-compaction. Two
/// write after the tail was read — one opening the path fresh, one through a
/// descriptor opened before the rotation — and a third opens the path fresh
/// after the compacted copy is complete. All must survive exactly once and
/// every line must parse. On origin/main (compact the live file in place,
/// then delete it) all three were lost.
#[test]
fn compaction_keeps_records_appended_during_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    std::fs::write(&path, legacy_bytes("old", 500)).expect("seed");
    let mut early_fd = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("early descriptor");
    let store = ErrorStore::with_path_and_rotation(Some(path.clone()), 5, POLICY);

    let _hook = {
        let path = path.clone();
        test_hook::install(move |point| match point {
            Point::CompactionTailRead => {
                append_fresh(&path, "late-fresh-open");
                write_record_line(&mut early_fd, &record("late-early-fd")).expect("write");
                early_fd.flush().expect("flush");
            }
            Point::CompactionCopied => append_fresh(&path, "after-copy"),
            _ => {}
        })
    };
    store.append(record("trigger"));

    let files = rotation::files_newest_first(&path, POLICY);
    let all = messages(&files);
    for want in [
        "late-fresh-open",
        "late-early-fd",
        "after-copy",
        "trigger",
        "old499",
    ] {
        let n = all.iter().filter(|msg| msg.as_str() == want).count();
        assert_eq!(n, 1, "{want} appears {n} times in {all:?}");
    }
    assert!(!suffixed(&rotation::rotated_path(&path, 1), ".src").exists());
    assert!(!suffixed(&rotation::rotated_path(&path, 1), ".tmp").exists());
}

/// #8028 fail-open arm 1: the rename of the live file to `.1.src` fails. The
/// live file keeps every byte and the write is refused and counted.
#[test]
fn a_failed_source_rename_leaves_the_live_file_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let legacy = legacy_bytes("old", 500);
    std::fs::write(&path, &legacy).expect("seed");
    squat_dir(&suffixed(&rotation::rotated_path(&path, 1), ".src"));
    let store = ErrorStore::with_path_and_rotation(Some(path.clone()), 5, POLICY);

    store.append(record("refused"));

    assert_eq!(std::fs::read(&path).expect("live"), legacy);
    assert_eq!(store.refused_disk_writes(), 1);
    assert_eq!(store.recent_errors(1)[0].message, "refused");
}

/// #8028 fail-open arm 2: writing the compacted temp file fails. The whole
/// source moves into `.1` instead, and the append proceeds on a fresh file.
#[test]
fn a_failed_compaction_keeps_the_whole_source_in_the_first_slot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let legacy = legacy_bytes("old", 500);
    std::fs::write(&path, &legacy).expect("seed");
    let first = rotation::rotated_path(&path, 1);
    squat_dir(&suffixed(&first, ".tmp"));
    let store = ErrorStore::with_path_and_rotation(Some(path.clone()), 5, POLICY);

    store.append(record("after"));

    assert_eq!(std::fs::read(&first).expect(".1"), legacy);
    assert!(!suffixed(&first, ".src").exists());
    assert_eq!(messages(&[path]), ["after"]);
    assert_eq!(store.refused_disk_writes(), 0);
}

/// #8028 fail-open arms 3 and 4: the compaction and its fallback both fail.
/// The source keeps every byte, the append returns and is counted, and the
/// next append writes a fresh live file. A later oversized live file then
/// rotates whole rather than overwrite the leftover source.
#[test]
fn a_failed_fallback_keeps_the_source_and_the_writer_moving() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let legacy = legacy_bytes("old", 500);
    std::fs::write(&path, &legacy).expect("seed");
    let first = rotation::rotated_path(&path, 1);
    let src = suffixed(&first, ".src");
    squat_dir(&suffixed(&first, ".tmp"));
    squat_dir(&first);
    let store = ErrorStore::with_path_and_rotation(Some(path.clone()), 5, POLICY);

    store.append(record("refused"));
    assert_eq!(std::fs::read(&src).expect("source kept"), legacy);
    assert_eq!(store.refused_disk_writes(), 1);
    store.append(record("next"));
    assert_eq!(messages(std::slice::from_ref(&path)), ["next"]);

    std::fs::remove_dir_all(&first).expect("clear .1");
    let second = legacy_bytes("new", 500);
    std::fs::write(&path, &second).expect("second oversized live");
    store.append(record("third"));
    assert_eq!(std::fs::read(&src).expect("source untouched"), legacy);
    assert_eq!(std::fs::read(&first).expect(".1"), second);
    assert_eq!(messages(&[path]), ["third"]);
}

/// #8028 MED 3: a rotation between the reader's live-file read and its `.1`
/// read moves the file it just read into `.1`. Each record must count once.
#[test]
fn a_read_overlapping_a_rotation_counts_each_record_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("errors.jsonl");
    let mut live = Vec::new();
    for m in ["a", "b", "c"] {
        write_record_line(&mut live, &record(m)).expect("encode");
    }
    std::fs::write(&path, &live).expect("seed");
    // Due now, and the whole file is inside the read limit.
    let policy = RotationPolicy {
        max_bytes: live.len() as u64,
        keep: 2,
    };
    let rotated = Rc::new(Cell::new(false));
    let _hook = {
        let (path, rotated) = (path.clone(), Rc::clone(&rotated));
        test_hook::install(move |point| {
            if point == Point::BetweenStoreFiles && !rotated.replace(true) {
                rotation::rotate_if_due(&path, policy).expect("rotate");
            }
        })
    };

    let store = ErrorStore::with_path_and_rotation(Some(path), 10, policy);

    assert!(rotated.get(), "the rotation ran mid-read");
    let got: Vec<String> = store
        .recent_errors(10)
        .into_iter()
        .map(|r| r.message)
        .collect();
    assert_eq!(got, ["a", "b", "c"]);
}
