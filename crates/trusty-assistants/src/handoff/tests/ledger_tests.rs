//! The handoff ledger (#11650 item 4).

use std::sync::Arc;

use super::super::ledger::{LEDGER_RECORD_VERSION, NO_COMPLETION};
use super::super::{HandoffError, Ledger, LedgerRecord, Outcome, Target, TmSessionUse};
use super::home;

fn intent(key: &str, target: Target, task: &str) -> LedgerRecord {
    LedgerRecord::Intent {
        v: LEDGER_RECORD_VERSION,
        key: key.into(),
        at: "2026-11-23T00:00:00Z".into(),
        target,
        task: task.into(),
    }
}

fn done(key: &str, result: Outcome) -> LedgerRecord {
    LedgerRecord::Done {
        v: LEDGER_RECORD_VERSION,
        key: key.into(),
        at: "2026-11-23T00:00:01Z".into(),
        result,
    }
}

fn line(record: &LedgerRecord) -> String {
    format!("{}\n", serde_json::to_string(record).expect("json"))
}

/// Why: the ledger is append-only jsonl in the instance's store and reads
/// back what was written; the tm session id is recorded there.
/// Test: itself.
#[tokio::test]
async fn records_round_trip_through_the_file() {
    let (_root, home) = home();
    let path = Ledger::path_for(&home);
    assert_eq!(path, home.stores_dir().join("handoffs.jsonl"));
    {
        let mut ledger = Ledger::open(&path).await.expect("open");
        ledger
            .append(intent("k1", Target::Tm, "t"))
            .expect("intent");
        ledger
            .append(done(
                "k1",
                Outcome::Dispatched {
                    handle: "tm-1".into(),
                    tm_session: Some(TmSessionUse::Created),
                },
            ))
            .expect("done");
    }
    let raw = std::fs::read_to_string(&path).expect("read");
    assert_eq!(raw.lines().count(), 2, "{raw}");
    assert!(
        raw.lines()
            .next()
            .expect("first")
            .contains(r#""kind":"intent""#)
    );

    let ledger = Ledger::open(&path).await.expect("reopen");
    let entry = ledger.entries().remove("k1").expect("k1");
    assert_eq!((entry.target, entry.task.as_str()), (Target::Tm, "t"));
    assert!(matches!(entry.outcome, Outcome::Dispatched { ref handle, .. } if handle == "tm-1"));
    assert_eq!(ledger.tm_session().as_deref(), Some("tm-1"));
}

/// Why: item 4 — a crash after the intent and before a handle exists must
/// read as `outcome_unknown`, never as success.
/// Test: itself.
#[tokio::test]
async fn an_intent_without_completion_reads_outcome_unknown() {
    let (_root, home) = home();
    let path = Ledger::path_for(&home);
    std::fs::create_dir_all(home.stores_dir()).expect("stores");
    let body = [
        line(&intent("a", Target::Tc, "x")),
        line(&done("a", Outcome::Failed { error: "e".into() })),
        line(&intent("a", Target::Tc, "x")),
        line(&intent("b", Target::Tm, "y")),
    ]
    .concat();
    std::fs::write(&path, body).expect("write");
    let entries = Ledger::open(&path).await.expect("open").entries();
    for key in ["a", "b"] {
        assert_eq!(
            entries[key].outcome,
            Outcome::OutcomeUnknown {
                error: NO_COMPLETION.into()
            },
            "{key}"
        );
    }
}

/// Why: a crash mid-append leaves an unterminated line; it is moved aside so
/// later appends stay whole, and a torn completion leaves its key unknown.
/// Test: itself.
#[tokio::test]
async fn a_torn_last_line_is_quarantined() {
    let (_root, home) = home();
    let path = Ledger::path_for(&home);
    std::fs::create_dir_all(home.stores_dir()).expect("stores");
    let first = line(&intent("k", Target::Tc, "x"));
    let torn = r#"{"kind":"done","v":1,"key":"k","at":"t","result":{"outcome":"dispa"#;
    std::fs::write(&path, format!("{first}{torn}")).expect("write");

    let mut ledger = Ledger::open(&path).await.expect("open");
    assert!(matches!(
        ledger.entries()["k"].outcome,
        Outcome::OutcomeUnknown { .. }
    ));
    assert_eq!(std::fs::read_to_string(&path).expect("read"), first);
    let quarantined: Vec<_> = std::fs::read_dir(home.stores_dir())
        .expect("dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
        .collect();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(
        std::fs::read_to_string(quarantined[0].path()).expect("q"),
        torn
    );
    ledger
        .append(intent("k2", Target::Tm, "y"))
        .expect("append");
    drop(ledger);
    assert_eq!(
        Ledger::open(&path).await.expect("reopen").entries().len(),
        2
    );
}

/// Why: corruption before the last line is not a crash artefact; it stops
/// the handoff and the file is left alone.
/// Test: itself.
#[tokio::test]
async fn a_bad_middle_line_is_an_error() {
    let (_root, home) = home();
    let path = Ledger::path_for(&home);
    std::fs::create_dir_all(home.stores_dir()).expect("stores");
    let body = format!("not json\n{}", line(&intent("k", Target::Tc, "x")));
    std::fs::write(&path, &body).expect("write");
    let err = Ledger::open(&path).await.expect_err("corrupt");
    assert!(
        matches!(err, HandoffError::CorruptLedger { line: 1, .. }),
        "{err:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), body);
}

/// Why: item 4 — concurrent writers are safe: each holds the lock while it
/// appends, so every line stays whole.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_keep_every_line_whole() {
    let (_root, home) = home();
    let path = Arc::new(Ledger::path_for(&home));
    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let path = Arc::clone(&path);
            tokio::spawn(async move {
                let mut ledger = Ledger::open(&path).await.expect("open");
                ledger
                    .append(intent(&format!("k{i}"), Target::Tc, &"x".repeat(4096)))
                    .expect("append");
            })
        })
        .collect();
    for task in tasks {
        task.await.expect("join");
    }
    assert_eq!(Ledger::open(&path).await.expect("open").entries().len(), 16);
}
