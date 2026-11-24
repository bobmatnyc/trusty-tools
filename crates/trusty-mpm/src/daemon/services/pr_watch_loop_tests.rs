//! Tests for the PR watch loop (#11673).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parking_lot::Mutex;
use trusty_architect::ledger::model::WaitingOn;
use trusty_common::control_bus::{HarnessEvent, HarnessPayload};

use super::*;
use crate::core::gh_session_shim::{RATE_LIMIT_REFUSAL_MARKER, RATE_LIMIT_RESET_EPOCH_OPEN};
use crate::core::pr_watch::gh::{PrSnapshot, parse_pr_view};
use crate::core::pr_watch::table::WatchRow;
use crate::daemon::services::pr_watch_ports::{BusEmitter, ci_rows};

/// The PR as GitHub reports it; `None` answers every read with `err`.
#[derive(Default)]
struct World {
    state: String,
    head: String,
    rollup: String,
    err: Option<String>,
    views: usize,
}

#[derive(Default)]
struct FakeReader(Mutex<World>);

impl FakeReader {
    fn set(&self, state: &str, head: &str, rollup: &str) {
        let mut w = self.0.lock();
        w.state = state.into();
        w.head = head.into();
        w.rollup = rollup.into();
        w.err = None;
    }
}

impl PrReader for FakeReader {
    fn view(&self, _repo: &str, _pr: u64) -> anyhow::Result<PrSnapshot> {
        let mut w = self.0.lock();
        w.views += 1;
        if let Some(e) = &w.err {
            anyhow::bail!("{e}");
        }
        parse_pr_view(&format!(
            r#"{{"state":"{}","headRefOid":"{}","baseRefName":"main","statusCheckRollup":{}}}"#,
            w.state, w.head, w.rollup
        ))
    }
    fn required(&self, _repo: &str, _base: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(Some(vec!["ci".into()]))
    }
}

/// Counts calls; fails while `fail` is set.
#[derive(Default)]
struct Port {
    calls: AtomicUsize,
    fail: AtomicBool,
    lines: Mutex<Vec<String>>,
    events: Mutex<Vec<HarnessEvent>>,
}

impl Port {
    fn hit(&self) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(!self.fail.load(Ordering::SeqCst), "refused");
        Ok(())
    }
    fn n(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

struct E(Arc<Port>);
struct L(Arc<Port>);
struct W(Arc<Port>);

#[async_trait::async_trait]
impl Emitter for E {
    async fn emit(&self, event: HarnessEvent) -> anyhow::Result<()> {
        self.0.hit()?;
        self.0.events.lock().push(event);
        Ok(())
    }
}
#[async_trait::async_trait]
impl LedgerPort for L {
    async fn set_ci_wait(&self, _row: &WatchRow, _t: &Transition) -> anyhow::Result<()> {
        self.0.hit()
    }
}
#[async_trait::async_trait]
impl Waker for W {
    async fn wake(&self, owner: &str, line: &str) -> anyhow::Result<()> {
        self.0.hit()?;
        self.0.lines.lock().push(format!("{owner}: {line}"));
        Ok(())
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    table: Table,
    reader: Arc<FakeReader>,
    emit: Arc<Port>,
    ledger: Arc<Port>,
    wake: Arc<Port>,
}

impl Rig {
    fn new(rows: usize) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let table = Table::at(dir.path());
        for pr in 1..=rows as u64 {
            table.upsert(row(pr, "h1")).expect("add");
        }
        Self {
            _dir: dir,
            table,
            reader: Arc::default(),
            emit: Arc::default(),
            ledger: Arc::default(),
            wake: Arc::default(),
        }
    }

    /// A fresh loop over the same table: a daemon restart.
    fn driver(&self) -> PrWatchLoop {
        let ports = Ports {
            reader: Arc::clone(&self.reader) as Arc<dyn PrReader>,
            emitter: Arc::new(E(Arc::clone(&self.emit))),
            ledger: Arc::new(L(Arc::clone(&self.ledger))),
            waker: Arc::new(W(Arc::clone(&self.wake))),
        };
        PrWatchLoop::new(self.table.clone(), ports, 60)
    }

    fn row(&self) -> WatchRow {
        self.table.list().expect("list").remove(0)
    }
}

fn row(pr: u64, head: &str) -> WatchRow {
    WatchRow {
        repo: "o/r".into(),
        pr,
        head: head.into(),
        base: "main".into(),
        required: vec!["ci".into()],
        owner: "pm-a".into(),
        project: None,
        verdict: Some(Verdict::Pending),
        emitted_key: None,
        ledger_key: None,
        last_woken_key: None,
        added_at: "2026-11-23T00:00:00Z".into(),
    }
}

const GREEN: &str = r#"[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"},
                        {"name":"lint","status":"COMPLETED","conclusion":"FAILURE"}]"#;
const RED: &str = r#"[{"name":"ci","status":"COMPLETED","conclusion":"FAILURE"}]"#;
const RUNNING: &str = r#"[{"name":"ci","status":"IN_PROGRESS","conclusion":""}]"#;

#[tokio::test]
async fn a_green_head_wakes_the_owner_once_across_ticks_and_restart() {
    let rig = Rig::new(1);
    let mut l = rig.driver();
    rig.reader.set("OPEN", "h1", RUNNING);
    assert_eq!(l.tick(100).await.woken, 0, "pending is no transition");
    rig.reader.set("OPEN", "h1", GREEN);
    assert_eq!(l.tick(200).await.woken, 1);
    assert_eq!(l.tick(300).await.woken, 0, "same key, no second wake");
    // A restart re-reads the persisted key.
    assert_eq!(rig.driver().tick(400).await.woken, 0);
    assert_eq!((rig.emit.n(), rig.ledger.n(), rig.wake.n()), (1, 1, 1));
    let line = rig.wake.lines.lock()[0].clone();
    assert!(
        line.starts_with("pm-a: [tm pr watch] o/r#1 @ h1: required checks are green"),
        "{line}"
    );
    assert!(!line.contains('\n'));
    let ev = rig.emit.events.lock()[0].clone();
    let HarnessPayload::Hook { kind, data } = ev.payload else {
        panic!("not a Hook payload");
    };
    assert_eq!(kind, EVENT_KIND);
    assert_eq!(data["key"], "o/r#1@h1:green");
    assert_eq!(data["verdict"], "green");
    let r = rig.row();
    assert_eq!(r.last_woken_key.as_deref(), Some("o/r#1@h1:green"));
    assert_eq!(r.verdict, Some(Verdict::Green));
}

#[tokio::test]
async fn new_head_red_and_rerun_each_wake_once() {
    let rig = Rig::new(1);
    let mut l = rig.driver();
    rig.reader.set("OPEN", "h2", RUNNING);
    assert_eq!(l.tick(1).await.woken, 1, "new head");
    assert_eq!(rig.row().head, "h2");
    assert_eq!(
        l.tick(2).await.woken,
        0,
        "first pending on the new head is recorded"
    );
    rig.reader.set("OPEN", "h2", RED);
    assert_eq!(l.tick(3).await.woken, 1, "red");
    rig.reader.set("OPEN", "h2", RUNNING);
    assert_eq!(l.tick(4).await.woken, 1, "rerun");
    rig.reader.set("OPEN", "h2", RED);
    assert_eq!(l.tick(5).await.woken, 1, "red again after a rerun");
    let keys: Vec<_> = rig
        .emit
        .events
        .lock()
        .iter()
        .map(|e| match &e.payload {
            HarnessPayload::Hook { data, .. } => {
                data["transition"].as_str().unwrap_or("").to_string()
            }
            _ => String::new(),
        })
        .collect();
    assert_eq!(keys, ["new_head", "red", "rerun", "red"]);
}

#[tokio::test]
async fn a_failed_wake_retries_next_tick_and_never_re_emits() {
    let rig = Rig::new(1);
    let mut l = rig.driver();
    rig.reader.set("OPEN", "h1", GREEN);
    rig.wake.fail.store(true, Ordering::SeqCst);
    let r = l.tick(1).await;
    assert_eq!((r.woken, r.failed), (0, 1));
    let row = rig.row();
    assert_eq!(row.last_woken_key, None, "a refused wake advances nothing");
    assert_eq!(row.verdict, Some(Verdict::Pending), "verdict not advanced");
    assert_eq!(row.emitted_key.as_deref(), Some("o/r#1@h1:green"));
    rig.wake.fail.store(false, Ordering::SeqCst);
    assert_eq!(rig.driver().tick(2).await.woken, 1, "retried after restart");
    assert_eq!((rig.emit.n(), rig.ledger.n(), rig.wake.n()), (1, 1, 2));
}

#[tokio::test]
async fn a_failed_emit_advances_nothing() {
    let rig = Rig::new(1);
    let mut l = rig.driver();
    rig.reader.set("OPEN", "h1", RED);
    rig.emit.fail.store(true, Ordering::SeqCst);
    assert_eq!(l.tick(1).await.failed, 1);
    assert_eq!(rig.row(), row(1, "h1"), "the row is unchanged");
    assert_eq!((rig.ledger.n(), rig.wake.n()), (0, 0));
    rig.ledger.fail.store(true, Ordering::SeqCst);
    rig.emit.fail.store(false, Ordering::SeqCst);
    assert_eq!(l.tick(2).await.failed, 1);
    let r = rig.row();
    assert_eq!((r.ledger_key, r.last_woken_key), (None, None));
    assert_eq!(rig.wake.n(), 0, "no wake past a failed ledger write");
}

#[tokio::test]
async fn a_merged_pr_row_is_removed() {
    let rig = Rig::new(1);
    rig.reader.set("MERGED", "h1", GREEN);
    let r = rig.driver().tick(1).await;
    assert_eq!((r.removed, r.woken), (1, 0));
    assert!(rig.table.list().expect("list").is_empty());
}

#[tokio::test]
async fn each_tick_reads_at_most_the_bound() {
    let rig = Rig::new(MAX_ROWS_PER_TICK + 3);
    rig.reader.set("OPEN", "h1", RUNNING);
    let mut l = rig.driver();
    assert_eq!(l.tick(1).await.read, MAX_ROWS_PER_TICK);
    assert_eq!(l.tick(2).await.read, MAX_ROWS_PER_TICK);
    assert_eq!(rig.reader.0.lock().views, 2 * MAX_ROWS_PER_TICK);
}

#[tokio::test]
async fn a_rate_limited_read_pauses_the_watch_until_the_reset() {
    let rig = Rig::new(3);
    let mut l = rig.driver();
    rig.reader.0.lock().err = Some(format!(
        "gh: {RATE_LIMIT_REFUSAL_MARKER} {RATE_LIMIT_RESET_EPOCH_OPEN}1000)"
    ));
    let r = l.tick(100).await;
    assert!(r.rate_limited);
    assert_eq!(
        rig.reader.0.lock().views,
        1,
        "the tick stops at the refusal"
    );
    assert!(l.tick(999).await.rate_limited, "paused until the reset");
    assert_eq!(rig.reader.0.lock().views, 1);
    rig.reader.set("OPEN", "h1", RUNNING);
    assert!(!l.tick(1000).await.rate_limited);
}

#[tokio::test]
async fn a_publisherless_emit_lands_in_the_spool() {
    let dir = tempfile::tempdir().expect("tempdir");
    let e = BusEmitter {
        publisher: None,
        spool_dir: Some(dir.path().join("spool")),
    };
    let t = Transition {
        kind: Kind::Green,
        head: "h1".into(),
        verdict: Verdict::Green,
        key: "o/r#1@h1:green".into(),
        required: vec!["ci".into()],
    };
    e.emit(transition_event(&row(1, "h1"), &t))
        .await
        .expect("spooled");
    let back = crate::daemon::action_events::spool::take_all(&dir.path().join("spool"))
        .expect("spool readable");
    assert_eq!(back.events.len(), 1);
    let none = BusEmitter {
        publisher: None,
        spool_dir: None,
    };
    assert!(
        none.emit(transition_event(&row(1, "h1"), &t))
            .await
            .is_err()
    );
}

#[test]
fn ledger_rows_replace_only_this_prs_ci_row() {
    let w = |who: &str, what: &str| WaitingOn {
        who: who.into(),
        what: what.into(),
        since: "2026-11-23T00:00:00Z".into(),
    };
    let t = Transition {
        kind: Kind::Red,
        head: "abcdef0123456".into(),
        verdict: Verdict::Red,
        key: "k".into(),
        required: vec![],
    };
    let rows = vec![
        w("owner", "a ruling"),
        w("ci", "o/r#1 CI pending @x"),
        w("ci", "o/r#12 CI green"),
    ];
    let out = ci_rows(&rows, &row(1, "h"), &t).expect("set");
    assert_eq!(out.len(), 3);
    assert_eq!(out[1].what, "o/r#1 CI red @abcdef0123");
    assert_eq!(out[2].what, "o/r#12 CI green", "another PR's row is kept");
    let full: Vec<_> = (0..5).map(|i| w("owner", &format!("q{i}"))).collect();
    assert!(
        ci_rows(&full, &row(1, "h"), &t).is_none(),
        "a full list is skipped"
    );
}

#[test]
fn parse_interval_defaults_on_junk_and_zero() {
    assert_eq!(parse_interval_secs(None), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some("0")), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some("x")), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some(" 30 ")), 30);
    assert!(!parse_enabled(Some("off")));
}
