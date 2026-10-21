//! #8454 S6 prep: the `tripwire_record` doctor row's grading.

use std::os::unix::fs::PermissionsExt as _;

use chrono::{TimeZone as _, Utc};

use super::*;
use crate::core::channels_tripwire::{FORMAT_VERSION, TripLine, append};

fn trip(secs: i64) -> TripLine {
    TripLine {
        v: FORMAT_VERSION,
        at: Utc.timestamp_opt(secs, 0).single().expect("timestamp"),
        session_id: Some("s-1".into()),
        tool_name: "Read".into(),
        cause: "inside".into(),
        path: Some("/p/a".into()),
    }
}

/// Set `path`'s mode, returning a guard that restores `0o700` on drop so the
/// tempdir can be removed.
struct Mode<'a>(&'a Path);

impl<'a> Mode<'a> {
    fn set(path: &'a Path, mode: u32) -> Self {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        Self(path)
    }
}

impl Drop for Mode<'_> {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
    }
}

/// Whether this process ignores file modes (root), which voids a mode test.
fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

/// 🔴 #8454 S6: no record, an empty record and a blank-line record are Ok.
#[test]
fn tripwire_record_row_ok_when_absent_or_empty() {
    let home = tempfile::tempdir().expect("tempdir");
    let row = check_tripwire_record(home.path());
    assert_eq!(row.status, CheckStatus::Ok, "absent dir: {}", row.message);
    assert_eq!(row.name, "tripwire_record");
    std::fs::create_dir_all(record_dir(home.path())).expect("mkdir");
    let row = check_tripwire_record(home.path());
    assert_eq!(row.status, CheckStatus::Ok, "absent file: {}", row.message);
    std::fs::write(record_path(home.path()), "\n\n").expect("write empty");
    let row = check_tripwire_record(home.path());
    assert_eq!(row.status, CheckStatus::Ok, "empty file: {}", row.message);
}

/// 🔴 #8454 S6 (Architect 11:13Z): trips Warn, naming the count and the
/// newest timestamp, whatever order the lines are in.
#[test]
fn tripwire_record_row_warns_with_count_and_newest() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = record_dir(home.path());
    for secs in [1_800_000_000, 1_900_000_000, 1_700_000_000] {
        append(&dir, &trip(secs)).expect("append");
    }
    let row = check_tripwire_record(home.path());
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message.contains("3 CTO tripwire trip(s)"),
        "{}",
        row.message
    );
    let newest = trip(1_900_000_000).at.to_rfc3339();
    assert!(row.message.contains(&newest), "{}", row.message);
}

/// 🔴 #8454 S6: a line that does not parse, a file where the directory
/// belongs, and a directory the hook cannot write each Fail.
#[test]
fn tripwire_record_row_fails_on_unparseable_line_or_unwritable_dir() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = record_dir(home.path());
    append(&dir, &trip(1_800_000_000)).expect("append");
    let mut text = std::fs::read_to_string(record_path(home.path())).expect("read");
    text.push_str("{\"v\":1,\"torn\n");
    std::fs::write(record_path(home.path()), text).expect("write torn");
    let row = check_tripwire_record(home.path());
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("line 2"), "{}", row.message);

    let blocked = tempfile::tempdir().expect("tempdir");
    let dir = record_dir(blocked.path());
    std::fs::create_dir_all(dir.parent().expect("parent")).expect("mkdir");
    std::fs::write(&dir, b"x").expect("a file at the directory");
    let row = check_tripwire_record(blocked.path());
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("not a directory"), "{}", row.message);

    if is_root() {
        return;
    }
    let locked = tempfile::tempdir().expect("tempdir");
    let dir = record_dir(locked.path());
    append(&dir, &trip(1_800_000_000)).expect("append");
    let _mode = Mode::set(&dir, 0o500);
    let row = check_tripwire_record(locked.path());
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("not writable"), "{}", row.message);
}

/// 🔴 #8454 S6: a record or directory that cannot be read is Unknown.
#[test]
fn tripwire_record_row_unknown_when_unreadable() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = record_dir(home.path());
    std::fs::create_dir_all(record_path(home.path())).expect("a dir at the file");
    let row = check_tripwire_record(home.path());
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    std::fs::remove_dir(record_path(home.path())).expect("rmdir");

    if is_root() {
        return;
    }
    append(&dir, &trip(1_800_000_000)).expect("append");
    let file = record_path(home.path());
    let _file_mode = Mode::set(&file, 0o000);
    let row = check_tripwire_record(home.path());
    assert_eq!(
        row.status,
        CheckStatus::Unknown,
        "unreadable file: {}",
        row.message
    );
    drop(_file_mode);

    let parent = dir.parent().expect("channels dir").to_path_buf();
    let _dir_mode = Mode::set(&parent, 0o000);
    let row = check_tripwire_record(home.path());
    assert_eq!(
        row.status,
        CheckStatus::Unknown,
        "unstattable dir: {}",
        row.message
    );
}
