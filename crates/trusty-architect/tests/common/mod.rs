//! Helpers that several trusty-architect test binaries used to copy.
//!
//! Why: #10571 dead/duplicate review, item G: the same helper bodies lived in
//! two to six test files each.
//! What: the dashboard store's no-op scanner and file snapshot, the sorted
//! alert kinds, the golden-file compare, the parity shadow-record builders
//! and an mtime stamp. Each test binary uses a subset, so unused items are
//! allowed here.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use trusty_architect::classify::Alert;
use trusty_architect::sched::shadow::{EventKey, ShadowAlert, ShadowRecord, WokenKey};

/// A test scanner: nothing is secret-shaped (#10063 PR 2: a store with no
/// scan refuses writes).
pub fn no_secret(_: &str) -> Option<String> {
    None
}

/// Every file under `root` with its bytes, `.lock` excluded.
/// Test: `stale_if_version_conflicts_and_leaves_bytes`.
pub fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n != ".lock") {
                out.push((path.clone(), fs::read(&path).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

/// The alert kinds, sorted, for an order-free compare.
pub fn kinds(alerts: &[Alert]) -> Vec<&str> {
    let mut k: Vec<&str> = alerts.iter().map(|a| a.kind.as_str()).collect();
    k.sort_unstable();
    k
}

/// Compares `rows` with the golden file `dir/name` line by line, naming the
/// first diff.
///
/// Why: the Python-parity suites each compare Rust rows with a golden file
/// the prototype wrote; only the fixture directory differs.
/// What: one `assert_eq!` per line (Rust left, Python right), then one on
/// the whole text so a row-count or trailing-byte drift also fails.
/// Test: `host_alerts_match_python_goldens`.
pub fn assert_golden_in(
    dir: &Path,
    name: &str,
    rows: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let golden = fs::read_to_string(dir.join(name))?;
    let got: String = rows.iter().map(|r| format!("{r}\n")).collect();
    for (i, (g, r)) in golden.lines().zip(got.lines()).enumerate() {
        assert_eq!(
            r,
            g,
            "{name} line {}: Rust (left) differs from Python (right)",
            i + 1
        );
    }
    assert_eq!(got, golden, "{name}: row count or trailing bytes differ");
    Ok(())
}

/// An [`EventKey`] from its session, kind and fingerprint.
pub fn key(s: &str, k: &str, f: &str) -> EventKey {
    EventKey {
        session: s.to_owned(),
        kind: k.to_owned(),
        fingerprint: f.to_owned(),
    }
}

/// A successful `poll` shadow record at `slot` with these alert and woken keys.
/// Test: `parity_reports_identity_and_fallback_numbers`.
pub fn record(slot: i64, alerts: Vec<EventKey>, woken: Vec<EventKey>) -> ShadowRecord {
    ShadowRecord {
        ts: "x".into(),
        job: "poll".into(),
        slot,
        mode: "shadow".into(),
        interval_s: 60,
        ok: true,
        panes: Some(1),
        new: Some(alerts.len() as u64),
        woken: Some(woken.len() as u64),
        logged: Some(0),
        suppressed: Some(0),
        wake_phrase: None,
        wake_target: None,
        alerts: alerts
            .iter()
            .map(|k| ShadowAlert {
                session: k.session.clone(),
                kind: k.kind.clone(),
            })
            .collect(),
        digest_sha: "d".into(),
        error: None,
        haiku_woken: Some(0),
        alert_keys: Some(alerts),
        woken_keys: Some(
            woken
                .into_iter()
                .map(|key| WokenKey { key, haiku: false })
                .collect(),
        ),
    }
}

/// Writes `path` (parents created) and sets its mtime to `mtime` epoch seconds.
/// Test: `glob_freshness_uses_the_newest_mtime`.
pub fn stamp_file(path: &Path, mtime: i64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "x").unwrap();
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(mtime as u64))
        .unwrap();
}
