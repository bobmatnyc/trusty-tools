//! The ledger's on-disk format across the S2c change (#8994 S2c).
//!
//! Why: S2c adds the `pr_linked` event. Two promises ride on that: a ledger
//! written before S2c folds to exactly the state it folded to before, and a
//! binary built before S2c survives a line written by S2c.
//! What: a golden replay of a checked-in pre-S2c ledger, the `pr_linked` fold
//! rule, and a replay of an S2c ledger through a copy of the pre-S2c reader.
//! Test: this file.

use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use serde::Deserialize;

use super::super::{EventKind, LedgerEvent, Origin, WorktreeLedger};
use super::fold;

/// The checked-in ledger, written in the slice-1 / S2a / S2b format.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/core/worktree_ledger/testdata")
        .join(name)
}

/// #8994 S2c criterion 1: a pre-S2c ledger folds to the golden state.
///
/// The golden file was written from the fold before S2c (base 464d033149),
/// and this test passes there too. It fails if a pre-S2c line folds
/// differently, or if the S2c `pr` field leaks into the serialized state of a
/// tree no `pr_linked` event named.
#[test]
fn a_pre_s2c_ledger_folds_to_the_golden_state() {
    let read = WorktreeLedger::at(fixture("pre_s2c_ledger.jsonl"))
        .read()
        .expect("read the fixture");
    assert_eq!(read.events.len(), 14, "parseable lines");
    assert_eq!(read.malformed, 2, "the torn line and the unknown kind");

    let state = fold(&read.events);
    let got = serde_json::to_value(state.live.values().collect::<Vec<_>>()).expect("encode");
    let golden: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("pre_s2c_fold.json")).expect("read the golden file"),
    )
    .expect("parse the golden file");
    assert_eq!(got, golden);
    assert_eq!(fold(&read.events), state, "a replay folds identically");
}

fn at(secs: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 2, 0, 0, secs)
        .single()
        .expect("valid time")
}

fn event(path: &str, secs: u32, kind: EventKind) -> LedgerEvent {
    LedgerEvent {
        ts: at(secs),
        path: PathBuf::from(path),
        kind,
    }
}

fn created(path: &str, secs: u32) -> LedgerEvent {
    event(
        path,
        secs,
        EventKind::Created {
            repo: PathBuf::from("/r/alpha"),
            branch: Some("feat/x".into()),
            origin: Origin::TmDaemon,
            session: None,
        },
    )
}

/// #8994 S2c criterion 1: the latest `pr_linked` of a live tree is its PR; a
/// `pr_linked` for a path that is not live is ignored, and a tree recorded
/// again after `removed` starts with no PR.
#[test]
fn fold_carries_the_latest_pr_of_a_live_tree_only() {
    let a = "/r/alpha/.worktrees/a";
    let mut events = vec![
        created(a, 1),
        event(a, 2, EventKind::PrLinked { pr: 10 }),
        event(a, 3, EventKind::PrLinked { pr: 12 }),
        event("/r/alpha/.worktrees/z", 4, EventKind::PrLinked { pr: 5 }),
    ];
    let state = fold(&events);
    assert_eq!(state.live[Path::new(a)].pr, Some(12));
    assert_eq!(state.live.len(), 1, "a pr_linked line makes nothing live");
    let json = serde_json::to_value(&state.live[Path::new(a)]).expect("encode");
    assert_eq!(json["pr"], 12, "the PR is serialized once known: {json}");

    events.push(event(a, 5, EventKind::Removed));
    events.push(created(a, 6));
    assert_eq!(fold(&events).live[Path::new(a)].pr, None);
}

/// The pre-S2c `EventKind`, copied verbatim from 464d033149 minus doc comments.
#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[allow(dead_code)]
enum PreS2cKind {
    Created {
        repo: PathBuf,
        #[serde(default)]
        branch: Option<String>,
        origin: Origin,
        #[serde(default)]
        session: Option<String>,
    },
    Observed {
        repo: PathBuf,
        #[serde(default)]
        branch: Option<String>,
        origin: Origin,
        #[serde(default)]
        session: Option<String>,
    },
    Measured {
        bytes: u64,
    },
    Removed,
}

/// The pre-S2c `LedgerEvent`, copied verbatim from 464d033149.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PreS2cEvent {
    ts: chrono::DateTime<Utc>,
    path: PathBuf,
    #[serde(flatten)]
    kind: PreS2cKind,
}

/// #8994 S2c criterion 2: a pre-S2c reader skips a `pr_linked` line as ONE
/// malformed line and parses every other line, including the ones after it.
///
/// The pre-S2c `WorktreeLedger::read` splits the file on `\n` and parses each
/// line on its own, counting a failure in `malformed`; this replays that loop
/// over the pre-S2c types.
#[test]
fn a_pr_linked_line_is_one_malformed_line_to_a_pre_s2c_reader() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("worktrees.jsonl");
    std::fs::copy(fixture("pre_s2c_ledger.jsonl"), &path).expect("copy the fixture");
    let ledger = WorktreeLedger::at(&path);
    ledger
        .append(&event(
            "/r/alpha/.worktrees/a",
            20,
            EventKind::PrLinked { pr: 77 },
        ))
        .expect("append pr_linked");
    ledger
        .append(&event("/r/alpha/.worktrees/a", 21, EventKind::Removed))
        .expect("append removed after it");

    let bytes = std::fs::read(&path).expect("read");
    let (mut parsed, mut malformed) = (Vec::new(), 0usize);
    for line in bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
    {
        match serde_json::from_slice::<PreS2cEvent>(line) {
            Ok(e) => parsed.push(e),
            Err(_) => malformed += 1,
        }
    }
    assert_eq!(malformed, 3, "the fixture's two bad lines plus pr_linked");
    assert_eq!(parsed.len(), 15, "the fixture's 14 events plus the removed");
    assert!(
        matches!(parsed.last().map(|e| &e.kind), Some(PreS2cKind::Removed)),
        "the line after pr_linked still parses"
    );

    // The current reader keeps every line too, and reads the PR.
    let read = ledger.read().expect("read");
    assert_eq!((read.events.len(), read.malformed), (16, 2));
}
