//! Tests for the worktree ledger, its fold, the recorded creation and the
//! backfill (#8994).

use std::cell::Cell;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};

use super::backfill::backfill_checkouts;
use super::fold::{GIB, fold};
use super::record::{CreationIntent, RecordedCreateError, create_recorded, measure_live};
use super::{EventKind, LedgerEvent, Origin, WorktreeLedger, ledger_key};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

fn ev(secs: i64, path: &str, kind: EventKind) -> LedgerEvent {
    LedgerEvent {
        ts: Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap(),
        path: PathBuf::from(path),
        kind,
    }
}

fn created(repo: &str) -> EventKind {
    EventKind::Created {
        repo: PathBuf::from(repo),
        branch: Some("session/x".into()),
        origin: Origin::TmDaemon,
        session: Some("sid".into()),
    }
}

fn scratch_ledger() -> (tempfile::TempDir, WorktreeLedger) {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = WorktreeLedger::under_home(dir.path());
    (dir, ledger)
}

fn intent(repo: &Path) -> CreationIntent<'_> {
    CreationIntent {
        repo,
        branch: Some("session/t".into()),
        origin: Origin::TmCli,
        session: Some("sid".into()),
    }
}

#[test]
fn append_then_read_round_trips_every_event_kind() {
    let (_dir, ledger) = scratch_ledger();
    let events = vec![
        ev(0, "/w/a", created("/r")),
        ev(
            1,
            "/w/b",
            EventKind::Observed {
                repo: PathBuf::from("/r"),
                branch: None,
                origin: Origin::AgentIsolation,
            },
        ),
        ev(2, "/w/a", EventKind::Measured { bytes: 42 }),
        ev(3, "/w/a", EventKind::Removed),
    ];
    for e in &events {
        ledger.append(e).expect("append");
    }
    let read = ledger.read().expect("read");
    assert!(read.present);
    assert_eq!(read.malformed, 0);
    assert_eq!(read.events, events);
    let text = std::fs::read_to_string(ledger.path()).expect("raw");
    assert!(
        text.lines()
            .next()
            .unwrap()
            .contains(r#""event":"created""#),
        "{text}"
    );
}

#[test]
fn read_of_an_absent_ledger_is_empty_not_an_error() {
    let (_dir, ledger) = scratch_ledger();
    let read = ledger.read().expect("absent is not an error");
    assert!(!read.present);
    assert!(read.events.is_empty());
}

#[test]
fn read_counts_a_torn_line_without_losing_the_rest() {
    let (_dir, ledger) = scratch_ledger();
    ledger
        .append(&ev(0, "/w/a", created("/r")))
        .expect("append");
    let mut text = std::fs::read_to_string(ledger.path()).unwrap();
    text.push_str("{\"ts\":\"2027-01-15T08:00:00Z\",\"pa\n");
    std::fs::write(ledger.path(), text).unwrap();
    ledger
        .append(&ev(1, "/w/b", created("/r")))
        .expect("append");
    let read = ledger.read().expect("read");
    assert_eq!(read.malformed, 1);
    assert_eq!(read.events.len(), 2);
}

// ── criterion 2: the fold ────────────────────────────────────────────────

#[test]
fn fold_created_measured_removed_leaves_nothing_live() {
    let state = fold(&[
        ev(0, "/w/a", created("/r")),
        ev(1, "/w/a", EventKind::Measured { bytes: 10 }),
        ev(2, "/w/a", EventKind::Removed),
    ]);
    assert!(state.live.is_empty(), "{state:?}");
}

#[test]
fn fold_keeps_one_live_entry_with_its_last_measured_size() {
    let state = fold(&[
        ev(0, "/w/a", created("/r")),
        ev(1, "/w/a", EventKind::Measured { bytes: 10 }),
        ev(2, "/w/a", EventKind::Measured { bytes: 30 }),
        // A second `created` for a live path keeps the first attribution.
        ev(3, "/w/a", created("/other")),
        // A measurement of a path that is not live is ignored.
        ev(4, "/w/ghost", EventKind::Measured { bytes: 99 }),
    ]);
    assert_eq!(state.live.len(), 1, "{state:?}");
    let wt = &state.live[Path::new("/w/a")];
    assert_eq!(wt.bytes, Some(30));
    assert_eq!(wt.repo, PathBuf::from("/r"));
    assert_eq!(
        wt.measured_at,
        Some(Utc.timestamp_opt(1_800_000_002, 0).unwrap())
    );
}

#[test]
fn fold_replay_of_the_same_file_is_identical() {
    let (_dir, ledger) = scratch_ledger();
    for e in [
        ev(0, "/w/b", created("/r2")),
        ev(1, "/w/a", created("/r1")),
        ev(2, "/w/a", EventKind::Measured { bytes: 7 }),
        ev(3, "/w/c", created("/r1")),
        ev(4, "/w/c", EventKind::Removed),
    ] {
        ledger.append(&e).unwrap();
    }
    let first = fold(&ledger.read().unwrap().events);
    let second = fold(&ledger.read().unwrap().events);
    assert_eq!(first, second);
    assert_eq!(
        first.live.keys().cloned().collect::<Vec<_>>(),
        vec![PathBuf::from("/w/a"), PathBuf::from("/w/b")]
    );
}

#[test]
fn summary_groups_by_repo_with_count_and_gib() {
    let gib = GIB as u64;
    let state = fold(&[
        ev(0, "/w/a", created("/r1")),
        ev(1, "/w/b", created("/r1")),
        ev(2, "/w/c", created("/r2")),
        ev(3, "/w/a", EventKind::Measured { bytes: gib }),
        ev(4, "/w/b", EventKind::Measured { bytes: gib / 2 }),
    ]);
    let summary = state.by_project();
    assert_eq!(summary.len(), 2);
    assert_eq!(
        (summary[0].count, summary[0].gib, summary[0].unmeasured),
        (2, 1.5, 0)
    );
    assert_eq!(
        (summary[1].count, summary[1].gib, summary[1].unmeasured),
        (1, 0.0, 1)
    );
}

// ── criterion 1 (unit level): the append error is returned ───────────────

#[test]
fn create_recorded_refuses_before_creating_when_the_ledger_is_unwritable() {
    let dir = tempfile::tempdir().unwrap();
    // The ledger path is a DIRECTORY, so the append open fails for any user.
    let ledger_path = dir.path().join("worktrees.jsonl");
    std::fs::create_dir_all(&ledger_path).unwrap();
    let ledger = WorktreeLedger::at(&ledger_path);
    let ran = Cell::new(false);
    let result = create_recorded(&ledger, intent(dir.path()), || {
        ran.set(true);
        Ok(dir.path().join("tree"))
    });
    assert!(
        matches!(result, Err(RecordedCreateError::Ledger(_))),
        "an unwritable ledger must refuse the creation: {result:?}"
    );
    assert!(
        !ran.get(),
        "nothing may be created when the ledger cannot record it"
    );
}

#[test]
fn create_recorded_appends_one_created_event() {
    let (dir, ledger) = scratch_ledger();
    let tree = dir.path().join("tree");
    let path = create_recorded(&ledger, intent(dir.path()), || {
        std::fs::create_dir_all(&tree).map_err(|e| e.to_string())?;
        Ok(tree.clone())
    })
    .expect("recorded");
    assert_eq!(path, tree);
    let state = fold(&ledger.read().unwrap().events);
    let wt = &state.live[&ledger_key(&tree)];
    assert_eq!(wt.origin, Origin::TmCli);
    assert_eq!(wt.session.as_deref(), Some("sid"));
    assert_eq!(state.live.len(), 1);
}

#[test]
fn create_recorded_appends_nothing_when_creation_fails() {
    let (dir, ledger) = scratch_ledger();
    let result = create_recorded(&ledger, intent(dir.path()), || Err("boom".to_string()));
    assert!(matches!(result, Err(RecordedCreateError::Create(ref m)) if m == "boom"));
    assert!(ledger.read().unwrap().events.is_empty());
}

#[test]
fn measure_live_records_the_size_of_each_present_tree() {
    let (dir, ledger) = scratch_ledger();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("f"), vec![7u8; 64 * 1024]).unwrap();
    ledger
        .append(&LedgerEvent::now(ledger_key(&tree), created("/r")))
        .unwrap();
    ledger
        .append(&LedgerEvent::now(dir.path().join("gone"), created("/r")))
        .unwrap();
    let state = fold(&ledger.read().unwrap().events);
    let report = measure_live(&ledger, &state).expect("measure");
    assert_eq!((report.measured, report.missing), (1, 1));
    let after = fold(&ledger.read().unwrap().events);
    let bytes = after.live[&ledger_key(&tree)].bytes.expect("measured");
    assert!(bytes >= 64 * 1024, "allocated bytes {bytes}");
    assert_eq!(after.live[&dir.path().join("gone")].bytes, None);
}

// ── criterion 3: the backfill ────────────────────────────────────────────

#[test]
fn backfill_twice_appends_no_duplicate_events() {
    let fixture = GitWorktreeFixture::new();
    let a = fixture.add_worktree("one");
    let b = fixture.add_worktree("two");
    let (_dir, ledger) = scratch_ledger();
    let checkouts = vec![fixture.repo.clone(), fixture.repo.clone()];

    let first = backfill_checkouts(&ledger, &checkouts).expect("first pass");
    assert_eq!((first.created, first.observed), (2, 0), "{first:?}");
    let after_first = ledger.read().unwrap().events.len();

    let second = backfill_checkouts(&ledger, &checkouts).expect("second pass");
    assert_eq!((second.created, second.observed), (0, 0), "{second:?}");
    let events = ledger.read().unwrap().events;
    assert_eq!(
        events.len(),
        after_first,
        "a second pass must append nothing"
    );
    for tree in [&a, &b] {
        let n = events
            .iter()
            .filter(|e| e.path == ledger_key(tree) && matches!(e.kind, EventKind::Created { .. }))
            .count();
        assert_eq!(n, 1, "exactly one created event for {}", tree.display());
    }
    let state = fold(&events);
    assert_eq!(state.live[&ledger_key(&a)].repo, ledger_key(&fixture.repo));
}

#[test]
fn backfill_observes_agent_isolation_trees() {
    let fixture = GitWorktreeFixture::new();
    let agent = fixture.add_worktree_at(&fixture.repo.join(".claude").join("worktrees"), "agent-x");
    let (_dir, ledger) = scratch_ledger();
    let report = backfill_checkouts(&ledger, std::slice::from_ref(&fixture.repo)).unwrap();
    assert_eq!((report.created, report.observed), (0, 1), "{report:?}");
    let state = fold(&ledger.read().unwrap().events);
    let wt = &state.live[&ledger_key(&agent)];
    assert!(wt.observed_only);
    assert_eq!(wt.origin, Origin::AgentIsolation);
}

#[test]
fn backfill_skips_a_tree_provisioning_already_recorded() {
    let fixture = GitWorktreeFixture::new();
    let (_dir, ledger) = scratch_ledger();
    let tree = create_recorded(&ledger, intent(&fixture.repo), || {
        Ok(fixture.add_worktree("provisioned"))
    })
    .expect("recorded");
    let report = backfill_checkouts(&ledger, std::slice::from_ref(&fixture.repo)).unwrap();
    assert_eq!(
        (report.created, report.already_recorded),
        (0, 1),
        "{report:?}"
    );
    assert_eq!(
        fold(&ledger.read().unwrap().events).live[&ledger_key(&tree)].origin,
        Origin::TmCli
    );
}

#[test]
fn backfill_reports_a_checkout_git_cannot_list() {
    let dir = tempfile::tempdir().unwrap();
    let (_l, ledger) = scratch_ledger();
    let report = backfill_checkouts(&ledger, &[dir.path().to_path_buf()]).unwrap();
    assert_eq!(report.unanswered, vec![dir.path().to_path_buf()]);
    assert!(
        !ledger.read().unwrap().present,
        "nothing to append, so no file"
    );
}
