//! #9758: the PM ledger writer — the prototype's write cases (`test_datg.py`
//! WriteCase, SessionRecordCase, ArchiveCase, SizeAndRotateCase write half),
//! ported, plus AC2 (secret refusal), AC3 (kill recovery), AC5 (prune to the
//! archive), AC6 (tamper), request-id replay and the single-writer rule.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_architect::FakeClock;
use trusty_architect::ledger::rpc::{BatchRequest, BatchResponse, Op, RpcErrorCode, StoreRef};
use trusty_architect::ledger::writer::{self, CommitStep, Faults, Writer};
use trusty_architect::ledger::{LedgerError, StorePaths, init_store};

type R = Result<(), Box<dyn std::error::Error>>;

/// 2026-10-21T10:00:00Z.
const NOW: f64 = 1_792_576_800.0;

/// A test scanner: a `ghp_` token with 16+ trailing characters.
fn scan(text: &str) -> Option<String> {
    let at = text.find("ghp_")?;
    let run = text[at + 4..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .count();
    (run >= 16).then(|| "ghp_…".to_owned())
}

struct Store {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    paths: StorePaths,
}

impl Store {
    /// `init` code `th`, then goal `th-G01` (the prototype tests' `Base`).
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let root = fs::canonicalize(tmp.path())?;
        let paths = init_store(
            &root.join("codes.json"),
            &root,
            "pm-a",
            "th",
            "2026-10-21T09:00Z",
            &|_| true,
        )?;
        let s = Self {
            _tmp: tmp,
            root,
            paths,
        };
        s.ok(json!([{"op": "add", "kind": "G", "title": "mobile UI", "maps": "G0c"}]))?;
        Ok(s)
    }

    fn store(&self) -> StoreRef {
        StoreRef {
            project: self.root.clone(),
            session: "pm-a".into(),
        }
    }

    fn req(&self, ops: Value, request_id: &str) -> Result<BatchRequest, serde_json::Error> {
        let ops: Vec<Op> = serde_json::from_value(ops)?;
        Ok(BatchRequest {
            store: self.store(),
            request_id: request_id.into(),
            if_v: None,
            ops,
        })
    }

    fn run_with(
        &self,
        faults: &dyn Faults,
        req: &BatchRequest,
    ) -> Result<BatchResponse, LedgerError> {
        let clock = FakeClock::new(NOW);
        let scan = scan;
        let mut w = Writer::new(&clock, &scan);
        w.faults = faults;
        w.batch(req)
    }

    fn run(&self, ops: Value) -> Result<BatchResponse, LedgerError> {
        let req = self.req(ops, "").map_err(LedgerError::Json)?;
        self.run_with(&writer::NoFaults, &req)
    }

    fn ok(&self, ops: Value) -> Result<BatchResponse, Box<dyn std::error::Error>> {
        Ok(self.run(ops)?)
    }

    fn state(&self) -> Value {
        serde_json::from_str(&read(&self.paths.state)).unwrap_or(Value::Null)
    }

    fn log(&self) -> Vec<Value> {
        read(&self.paths.log)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// `state.json`, `log.jsonl` and `archive.jsonl` bytes.
    fn files(&self) -> [String; 3] {
        [
            read(&self.paths.state),
            read(&self.paths.log),
            read(&self.paths.archive),
        ]
    }

    fn ids(&self, field: &str) -> Vec<String> {
        let st = self.state();
        st[field].as_array().map_or_else(Vec::new, |a| {
            a.iter()
                .filter_map(|o| o["id"].as_str().map(str::to_owned))
                .collect()
        })
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn exit(e: &LedgerError) -> i32 {
    e.code().exit_code()
}

/// `n` copies of `v`, as a JSON array.
fn rep(v: Value, n: usize) -> Value {
    Value::Array(vec![v; n])
}

fn task(title: &str) -> Value {
    json!({"op": "add", "kind": "T", "title": title, "goal": "th-G01"})
}

#[test]
fn ledger_writer_commit_bumps_v_and_leaves_no_temp_or_journal() -> R {
    let s = Store::new()?;
    let v = s.state()["v"].as_u64().ok_or("v")?;
    let resp = s.ok(json!([task("x")]))?;
    assert_eq!((resp.v, s.state()["v"].as_u64()), (v + 1, Some(v + 1)));
    let stray: Vec<_> = fs::read_dir(&s.paths.dir)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp.") || n.ends_with(".journal"))
        .collect();
    assert_eq!(stray, Vec::<String>::new());
    Ok(())
}

#[test]
fn ledger_writer_logs_one_line_per_change() -> R {
    let s = Store::new()?;
    let n = s.log().len();
    s.ok(json!([
        {"op": "add", "kind": "D", "title": "which db", "goal": "th-G01", "opts": [{"l": "a", "rec": true}, {"l": "b"}]},
        {"op": "update", "id": "th-D01", "esc": "architect"}
    ]))?;
    let new: Vec<_> = s.log()[n..]
        .iter()
        .map(|x| (x["id"].clone(), x["ev"].clone()))
        .collect();
    assert_eq!(
        new,
        vec![
            (json!("th-D01"), json!("open")),
            (json!("th-D01"), json!("escalated"))
        ]
    );
    Ok(())
}

#[test]
fn ledger_writer_refuses_a_malformed_store_and_leaves_it() -> R {
    let s = Store::new()?;
    fs::write(&s.paths.state, "{not json")?;
    let before = s.files();
    let err = s
        .run(json!([task("x")]))
        .err()
        .ok_or("malformed store was written")?;
    assert_eq!(exit(&err), 2, "{err}");
    assert_eq!(s.files(), before);
    Ok(())
}

#[test]
fn ledger_writer_refuses_id_collisions_live_and_archived() -> R {
    let s = Store::new()?;
    let err = s
        .run(json!([{"op": "add", "kind": "G", "title": "dup", "id": "th-G01"}]))
        .err();
    assert_eq!(err.as_ref().map(exit), Some(2));
    s.ok(json!([{"op": "add", "kind": "T", "title": "t", "goal": "th-G01", "id": "th-T01"}]))?;
    s.ok(json!([task("t2")]))?;
    assert_eq!(s.ids("tasks"), ["th-T01", "th-T02"]);
    s.ok(json!([{"op": "close", "id": "th-T01", "outcome": "merged"}]))?;
    let err =
        s.run(json!([{"op": "add", "kind": "T", "title": "t", "goal": "th-G01", "id": "th-T01"}]));
    assert!(
        err.is_err_and(|e| e.to_string().contains("archived")),
        "an archived id was reissued"
    );
    Ok(())
}

#[test]
fn ledger_writer_batch_is_all_or_nothing() -> R {
    let s = Store::new()?;
    let before = s.files();
    let ops = json!([task("ok"), {"op": "add", "kind": "T", "title": "bad", "goal": "th-G99"}]);
    assert_eq!(s.run(ops).err().as_ref().map(exit), Some(2));
    assert_eq!(s.files(), before);
    Ok(())
}

#[test]
fn ledger_writer_done_needs_outcome_and_close_archives() -> R {
    let s = Store::new()?;
    s.ok(json!([task("t")]))?;
    let err = s
        .run(json!([{"op": "update", "id": "th-T01", "status": "done"}]))
        .err();
    assert_eq!(err.as_ref().map(exit), Some(2));
    s.ok(json!([{"op": "close", "id": "th-T01", "outcome": "merged"}]))?;
    assert_eq!(s.ids("tasks"), Vec::<String>::new());
    let line: Value =
        serde_json::from_str(read(&s.paths.archive).lines().last().ok_or("archive")?)?;
    assert_eq!(
        (&line["item"]["id"], &line["item"]["status"], &line["kind"]),
        (&json!("th-T01"), &json!("done"), &json!("task"))
    );
    Ok(())
}

#[test]
fn ledger_writer_show_does_not_write() -> R {
    let s = Store::new()?;
    let before = s.files();
    assert_eq!(writer::show(&s.store(), true)?, "TOP th-G01 mobile UI");
    assert!(writer::show(&s.store(), false)?.starts_with("{\n \"schema\": 1,\n \"code\": \"th\""));
    assert_eq!(s.files(), before);
    Ok(())
}

#[test]
fn ledger_writer_session_limits_refuse_and_leave_files() -> R {
    let s = Store::new()?;
    s.ok(json!([{"op": "session", "task": "th-T01 ship it", "next": "merge PR 5",
        "waiting_on": [{"who": "owner", "what": "ruling on D01"}],
        "owed": [{"check": "main sha after merge", "expect": "abc1234", "ref": "https://x/pull/5"}],
        "agents": [{"id": "a1", "goal": "th-G01", "what": "review"}],
        "crons": [{"id": "c1", "schedule": "*/5 * * * *", "prompt": "check ci"}],
        "boundary": true, "ctx_use": {"active": 40, "stale": 60, "ctx": 150000, "keep": "the PR diff"}}]))?;
    let ses = s.state()["session"].clone();
    assert_eq!(
        ses["owed"],
        json!([{"check": "main sha after merge", "expect": "abc1234", "ref": "https://x/pull/5"}])
    );
    assert_eq!(
        (
            &ses["owed_none"],
            &ses["ctx_use"]["ctx"],
            &ses["boundary_at"]
        ),
        (&json!(false), &json!(150000), &json!("2026-10-21T10:00Z"))
    );
    let before = s.files();
    let bad = [
        json!({"task": "x".repeat(161)}),
        json!({"next": "y".repeat(201)}),
        json!({"owed": [{"check": "c", "expect": "z".repeat(81), "ref": "r"}]}),
        json!({"waiting_on": [{"who": "nobody", "what": "what"}]}),
        json!({"ctx_use": {"active": 101, "stale": 0, "ctx": 1}}),
        json!({"owed": [{"check": "main sha", "expect": "latest", "ref": "r"}]}),
        json!({"crons": [{"id": "a", "schedule": "b", "prompt": "p".repeat(201)}]}),
        json!({"refs": [{"key": "Bad Key", "value": "v"}]}),
        json!({"refs": [{"key": "k", "value": ""}]}),
        json!({"refs": (0..13).map(|i| json!({"key": format!("k{i}"), "value": "v"})).collect::<Vec<_>>()}),
        json!({"waiting_on": rep(json!({"who": "ci", "what": "w".repeat(120)}), 5),
               "owed": rep(json!({"check": "c".repeat(80), "expect": "e".repeat(80), "ref": "r".repeat(80)}), 8)}),
    ];
    for mut op in bad {
        op["op"] = json!("session");
        let err = s.run(json!([op])).err();
        assert_eq!(err.as_ref().map(exit), Some(2), "{op}");
    }
    assert_eq!(s.files(), before);
    let v = s.state()["v"].as_u64().ok_or("v")?;
    s.ok(json!([task("t"), {"op": "session", "task": "th-T01", "owed": [], "crons": [], "agents": [],
        "refs": [{"key": "doc", "value": "https://docs/x"}, {"key": "marker", "value": "[A 1Z]"}]}]))?;
    s.ok(json!([{"op": "session", "refs": [{"key": "doc", "value": "https://docs/y"}]}]))?;
    let ses = s.state()["session"].clone();
    assert_eq!(s.state()["v"].as_u64(), Some(v + 2));
    assert_eq!(
        (&ses["owed"], &ses["owed_none"], &ses["crons"]),
        (&json!([]), &json!(true), &json!([]))
    );
    assert_eq!(
        ses["refs"],
        json!([{"key": "doc", "value": "https://docs/y"}, {"key": "marker", "value": "[A 1Z]"}])
    );
    s.ok(json!([{"op": "session", "refs": []}]))?;
    assert_eq!(s.state()["session"]["refs"], json!([]));
    Ok(())
}

#[test]
fn ledger_writer_prune_archives_done_and_old_answered() -> R {
    let s = Store::new()?;
    s.ok(json!(
        (0..4)
            .map(|i| json!({"op": "add", "kind": "D", "title": format!("q{i}"), "goal": "th-G01"}))
            .collect::<Vec<_>>()
    ))?;
    let mut st = s.state();
    // AC5: a done item closed 8 days ago by a writer that did not archive it.
    st["items"][0]["status"] = json!("done");
    st["items"][0]["outcome"] = json!("x");
    st["items"][1]["status"] = json!("answered"); // no log entry: old
    st["items"][2]["status"] = json!("answered"); // answered just now
    fs::write(&s.paths.state, serde_json::to_string(&st)? + "\n")?;
    let mut log = read(&s.paths.log);
    log.push_str("{\"t\":\"2026-10-13T10:00Z\",\"id\":\"th-D01\",\"ev\":\"done\"}\n");
    log.push_str("{\"t\":\"2026-10-21T10:00Z\",\"id\":\"th-D03\",\"ev\":\"answered\"}\n");
    fs::write(&s.paths.log, log)?;
    let dry = s.ok(json!([{"op": "prune", "dry_run": true}]))?;
    assert_eq!(
        writer::render_prune(&dry, true, 0, 0),
        "dry-run: would archive 2: th-D01, th-D02"
    );
    assert_eq!(s.ids("items").len(), 4);
    s.ok(json!([{"op": "prune"}]))?;
    assert_eq!(s.ids("items"), ["th-D03", "th-D04"]);
    let archive = read(&s.paths.archive);
    assert!(archive.contains("\"id\":\"th-D01\"") && archive.contains("\"id\":\"th-D02\""));
    let again = s.ok(json!([{"op": "prune"}]))?;
    assert_eq!(
        writer::render_prune(&again, false, 0, 0),
        "nothing to prune"
    );
    Ok(())
}

#[test]
fn ledger_writer_title_limit_is_120() -> R {
    let s = Store::new()?;
    s.ok(json!([task(&"t".repeat(120))]))?;
    assert_eq!(
        s.run(json!([task(&"t".repeat(121))]))
            .err()
            .as_ref()
            .map(exit),
        Some(2)
    );
    Ok(())
}

fn big_item(i: usize, status: &str) -> Value {
    json!({"id": format!("th-D{i:02x}"), "kind": "question", "status": status, "goal": "th-G01",
        "title": "t".repeat(110), "opts": rep(json!({"l": "o".repeat(150), "rec": true}), 3),
        "answer": "a".repeat(280), "outcome": "o".repeat(280), "esc": null})
}

#[test]
fn ledger_writer_auto_prune_brings_live_shaped_state_under_8kb() -> R {
    let s = Store::new()?;
    let mut st = s.state();
    st["items"] = json!(
        (1..80)
            .map(|i| big_item(i, if i % 2 == 1 { "done" } else { "answered" }))
            .collect::<Vec<_>>()
    );
    fs::write(&s.paths.state, serde_json::to_string(&st)?)?;
    assert!(fs::metadata(&s.paths.state)?.len() > 60_000);
    let resp = s.ok(json!([{"op": "prune"}]))?;
    assert_eq!(resp.archived.len(), 79);
    assert!(fs::metadata(&s.paths.state)?.len() < 8192);
    Ok(())
}

fn wide_adds(n: usize, width: usize) -> Value {
    json!(
        (0..n)
            .map(
                |_| json!({"op": "add", "kind": "D", "title": "q", "goal": "th-G01",
        "opts": rep(json!({"l": "o".repeat(width), "rec": false}), 4)})
            )
            .collect::<Vec<_>>()
    )
}

#[test]
fn ledger_writer_refuses_over_16kb_naming_largest_items() -> R {
    let s = Store::new()?;
    let before = s.files();
    let err = s
        .run(wide_adds(30, 160))
        .err()
        .ok_or("16 KB write was accepted")?;
    assert!(err.to_string().contains("largest"), "{err}");
    assert_eq!(exit(&err), 2);
    assert_eq!(s.files(), before);
    Ok(())
}

#[test]
fn ledger_writer_logs_size_warning_once_on_crossing() -> R {
    let s = Store::new()?;
    let resp = s.ok(wide_adds(14, 160))?;
    assert_eq!(resp.warnings.len(), 1, "{:?}", resp.warnings);
    assert!(fs::metadata(&s.paths.state)?.len() > 8192);
    s.ok(json!([{"op": "update", "id": "th-D01", "title": "again"}]))?;
    let warnings = s
        .log()
        .iter()
        .filter(|x| x["kind"] == "size_warning")
        .count();
    assert_eq!(warnings, 1);
    Ok(())
}

/// AC2: exit 2, `state.json` and `archive.jsonl` untouched, one log line that
/// records the refusal without the value.
#[test]
fn ledger_writer_refuses_secret_shaped_input_and_logs_without_the_value() -> R {
    let s = Store::new()?;
    let secret = "ghp_abcdefghijklmnop0123456789";
    let [state, log, archive] = s.files();
    let err = s
        .run(json!([task("ok"), task(&format!("token {secret}"))]))
        .err()
        .ok_or("secret written")?;
    assert_eq!(exit(&err), 2, "{err}");
    assert!(!err.to_string().contains(secret));
    let [state2, log2, archive2] = s.files();
    assert_eq!((state2, archive2), (state, archive));
    let added = &log2[log.len()..];
    assert_eq!(added.lines().count(), 1, "{added}");
    let line: Value = serde_json::from_str(added.trim_end())?;
    assert_eq!(line["kind"], "secret_refused");
    assert!(
        !added.contains("abcdefghijklmnop"),
        "the log line holds the value"
    );
    Ok(())
}

/// Kills the commit at `step`, as a process killed there would.
struct KillAt(CommitStep);

impl Faults for KillAt {
    fn at(&self, step: CommitStep, _: &StorePaths) -> io::Result<()> {
        if step == self.0 {
            Err(io::Error::other("killed"))
        } else {
            Ok(())
        }
    }
}

/// AC3 and the amendment: a writer killed between the log append and the
/// snapshot replace leaves the item in the log only; the next writer replays
/// the journal, and a retry of the same `request_id` returns the first
/// result with the item present exactly once.
#[test]
fn ledger_writer_recovers_a_commit_killed_after_the_log_append() -> R {
    let s = Store::new()?;
    let before = read(&s.paths.state);
    let req = s.req(json!([task("crash me")]), "req-1")?;
    assert!(s.run_with(&KillAt(CommitStep::Logged), &req).is_err());
    assert_eq!(
        read(&s.paths.state),
        before,
        "state.json replaced before the kill point"
    );
    assert!(read(&s.paths.log).contains("crash me"));
    let first = s.run_with(&writer::NoFaults, &req)?;
    assert_eq!(writer::render_ids(&first), "th-T01");
    assert_eq!(s.ids("tasks"), ["th-T01"]);
    assert_eq!(
        read(&s.paths.log).matches("crash me").count(),
        2,
        "one event and one request line"
    );
    let retry = s.run_with(&writer::NoFaults, &req)?;
    assert_eq!(retry, first);
    assert_eq!(s.ids("tasks"), ["th-T01"]);
    assert!(!s.paths.journal.exists());
    Ok(())
}

/// A writer that bypasses the lock moves `v` once the journal is staged.
struct LocklessWrite;

impl Faults for LocklessWrite {
    fn at(&self, step: CommitStep, paths: &StorePaths) -> io::Result<()> {
        if step == CommitStep::Staged {
            let text = fs::read_to_string(&paths.state)?;
            let mut st: Value = serde_json::from_str(&text)?;
            st["v"] = json!(st["v"].as_u64().unwrap_or(0) + 7);
            fs::write(&paths.state, serde_json::to_string(&st)? + "\n")?;
        }
        Ok(())
    }
}

/// AC6: a read-modify-write that finds `v` changed logs `tamper`, writes no
/// event and does not overwrite.
#[test]
fn ledger_writer_logs_tamper_and_does_not_overwrite() -> R {
    let s = Store::new()?;
    let req = s.req(json!([task("t")]), "")?;
    let n = s.log().len();
    let err = s
        .run_with(&LocklessWrite, &req)
        .err()
        .ok_or("tampered store was overwritten")?;
    assert_eq!(err.code(), RpcErrorCode::Conflict, "{err}");
    let tampered = read(&s.paths.state);
    assert_eq!(s.ids("tasks"), Vec::<String>::new());
    let added: Vec<_> = s.log()[n..].iter().map(|x| x["kind"].clone()).collect();
    assert_eq!(added, vec![json!("tamper")]);
    assert!(!s.paths.journal.exists());
    s.ok(json!([task("t")]))?;
    assert_ne!(read(&s.paths.state), tampered);
    Ok(())
}

#[test]
fn ledger_writer_replays_a_retried_request_id_once() -> R {
    let s = Store::new()?;
    let req = s.req(json!([task("once")]), "turn-42")?;
    let first = s.run_with(&writer::NoFaults, &req)?;
    let again = s.run_with(&writer::NoFaults, &req)?;
    assert_eq!(again, first);
    assert_eq!(s.ids("tasks"), ["th-T01"]);
    let other = s.run_with(&writer::NoFaults, &s.req(json!([task("once")]), "turn-43")?)?;
    assert_eq!(writer::render_ids(&other), "th-T02");
    Ok(())
}

#[test]
fn ledger_writer_if_v_mismatch_is_a_conflict() -> R {
    let s = Store::new()?;
    let before = s.files();
    let mut req = s.req(json!([task("t")]), "")?;
    req.if_v = Some(99);
    let err = s
        .run_with(&writer::NoFaults, &req)
        .err()
        .ok_or("stale if_v was written")?;
    assert_eq!(exit(&err), 1);
    assert_eq!(s.files(), before);
    Ok(())
}

/// The single-writer rule: eight writers, five adds each, on one store; the
/// lock is held across each read-modify-write, so no add is lost.
#[test]
fn ledger_writer_two_concurrent_writers_lose_no_update() -> R {
    let s = Arc::new(Store::new()?);
    let v0 = s.state()["v"].as_u64().ok_or("v")?;
    let handles: Vec<_> = (0..8)
        .map(|w| {
            let s = Arc::clone(&s);
            std::thread::spawn(move || {
                (0..5).try_for_each(|i| s.run(json!([task(&format!("w{w} {i}"))])).map(drop))
            })
        })
        .collect();
    for h in handles {
        h.join().map_err(|_| "writer panicked")??;
    }
    assert_eq!(s.ids("tasks").len(), 40);
    assert_eq!(s.state()["v"].as_u64(), Some(v0 + 40));
    Ok(())
}

#[test]
fn ledger_writer_lock_contention_reports_unavailable() -> R {
    use std::os::fd::AsRawFd;
    let s = Store::new()?;
    let held = fs::OpenOptions::new().write(true).open(&s.paths.lock)?;
    // SAFETY: the fd is open for the life of `held`.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let before = s.files();
    let clock = FakeClock::new(NOW);
    let scan = scan;
    let mut w = Writer::new(&clock, &scan);
    w.lock_wait = Duration::from_millis(100);
    let err = w
        .batch(&s.req(json!([task("t")]), "")?)
        .err()
        .ok_or("locked store was written")?;
    assert_eq!(exit(&err), 3, "{err}");
    assert_eq!(s.files(), before);
    drop(held);
    w.batch(&s.req(json!([task("t")]), "")?)?;
    Ok(())
}

#[test]
fn ledger_writer_normalises_the_project_path() -> R {
    let s = Store::new()?;
    let sub = s.root.join("crates/x");
    fs::create_dir_all(&sub)?;
    fs::create_dir_all(s.root.join(".git"))?;
    let via_sub = StoreRef {
        project: sub,
        session: "pm-a".into(),
    };
    assert_eq!(via_sub.paths()?, s.paths);
    let relative = StoreRef {
        project: PathBuf::from("crates/x"),
        session: "pm-a".into(),
    };
    assert!(relative.paths().is_err());
    Ok(())
}

#[test]
fn ledger_op_check_enforces_the_writer_limits() -> R {
    let bad = [
        json!({"op": "add", "kind": "D", "title": "q", "opts": [{"l": "a"}, {"l": "b"}, {"l": "c"}, {"l": "d"}, {"l": "e"}]}),
        json!({"op": "add", "kind": "D", "title": "q", "opts": [{"l": "o".repeat(161)}]}),
        json!({"op": "add", "kind": "G", "title": "g", "finish": "f".repeat(161)}),
        json!({"op": "session", "agents": rep(json!({"id": "a"}), 7)}),
        json!({"op": "session", "owed": [{"check": "c", "expect": "TBD"}]}),
    ];
    for op in bad {
        let op: Op = serde_json::from_value(op.clone())?;
        assert!(op.check().is_err(), "{op:?}");
    }
    Ok(())
}
