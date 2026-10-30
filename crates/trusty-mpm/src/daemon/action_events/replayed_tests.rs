//! Unit tests for [`super::replayed::Pending::credit`] (#10467).
//!
//! Why: the credit decides when a replayed spool file is deleted; a wrong
//! boundary deletes events that never reached the bus.
//! Test: this file.

use std::path::Path;

use super::replayed::Pending;
use super::spool;
use super::spool_tests::fresh_events;

/// Claims on files holding `counts` events each, oldest first; a zero is a
/// file with one torn line and no event.
fn claims(dir: &Path, counts: &[usize]) -> Vec<spool::Claim> {
    std::fs::create_dir_all(dir).expect("spool dir");
    for (i, &count) in counts.iter().enumerate() {
        let mut text = String::new();
        for event in fresh_events(count) {
            text.push_str(&serde_json::to_string(&event).expect("encode"));
            text.push('\n');
        }
        if count == 0 {
            text.push_str("{\"torn\":\n");
        }
        std::fs::write(dir.join(format!("{i:024}-0-0.ndjson")), text).expect("write");
    }
    let replay = spool::take_all(dir).expect("take");
    let got: Vec<usize> = replay.claims.iter().map(spool::Claim::events).collect();
    assert_eq!(got, counts, "claims out of order");
    replay.claims
}

struct Row {
    name: &'static str,
    files: &'static [usize],
    /// `(sent, dropped)` per credit call.
    steps: &'static [(usize, u64)],
    /// The event counts of the claims each step retires.
    retired: &'static [&'static [usize]],
}

const ROWS: &[Row] = &[
    Row {
        name: "across a file boundary",
        files: &[2, 3],
        steps: &[(4, 0), (1, 0)],
        retired: &[&[2], &[3]],
    },
    Row {
        name: "two files in one credit",
        files: &[2, 3],
        steps: &[(5, 0)],
        retired: &[&[2, 3]],
    },
    Row {
        name: "zero-event claims retire once reached",
        files: &[0, 2, 0],
        steps: &[(0, 0), (1, 0), (1, 0)],
        retired: &[&[0], &[], &[2, 0]],
    },
    Row {
        name: "an eviction stops every retire",
        files: &[2, 3],
        steps: &[(1, 1), (4, 1)],
        retired: &[&[], &[]],
    },
    Row {
        name: "an eviction after a retire keeps the rest",
        files: &[2, 3],
        steps: &[(2, 0), (3, 1)],
        retired: &[&[2], &[]],
    },
];

#[test]
fn credit_retires_whole_claims_and_never_after_an_eviction_10467() {
    for row in ROWS {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut pending = Pending {
            claims: claims(&dir.path().join("spool"), row.files).into(),
            ..Pending::default()
        };
        for (step, (&(sent, dropped), want)) in row.steps.iter().zip(row.retired).enumerate() {
            let got: Vec<usize> = pending
                .credit(sent, dropped)
                .iter()
                .map(spool::Claim::events)
                .collect();
            assert_eq!(got, *want, "{}: step {step}", row.name);
        }
        let evicted = row.steps.iter().any(|&(_, dropped)| dropped > 0);
        assert_eq!(pending.evicted, evicted, "{}", row.name);
    }
}
