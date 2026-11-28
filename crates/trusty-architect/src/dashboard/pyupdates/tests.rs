//! Error arms of the `dashboard_updates.py` port (#12029 P2h).

use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::re::{bare, full_match, handback, search, tag};
use super::sources::{ANSWERED, Patterns, candidates, item_entries, one_paragraph};
use super::{ItemStore, ingest, pick_task_items};
use crate::clock::FakeClock;
use crate::dashboard::store::py::{PyDocOut, PyStore, PyStoreError};
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::spec_drift::Outcome;

type R = Result<(), Box<dyn std::error::Error>>;

/// 2026-11-27T13:00:00Z, the recordings' instant.
const NOW: f64 = 1_795_784_400.0;

fn g(p: &super::re::Pattern, t: &str) -> Option<String> {
    search(p, t).map(|v| v[0].clone())
}

/// Expected values printed by the frozen `TAG`, `BARE` and `HANDBACK`.
#[test]
fn tag_bare_and_handback_follow_python_re() {
    let (t, b) = (tag(), bare());
    assert_eq!(g(&t, "Task: Tqa"), Some("Tqa".into()));
    assert_eq!(g(&t, "xTask: Tqa"), None);
    assert_eq!(g(&t, "-Task: Tqa@1127"), Some("Tqa".into()));
    // The optional `@dddd` backs off when its lookahead fails.
    assert_eq!(g(&t, "Task: Tqa@11271"), Some("Tqa".into()));
    assert_eq!(g(&t, "Task: Tqal"), None);
    assert_eq!(g(&t, "Task: ar-Tqcz_"), Some("ar-Tqcz".into()));
    assert_eq!(g(&t, "Task: ar-x Task: Tqh"), Some("Tqh".into()));
    assert_eq!(g(&b, "(ar-Tqa)"), Some("Tqa".into()));
    assert_eq!(g(&b, "xar-Tqa ar-Tqb"), Some("Tqb".into()));
    assert_eq!(g(&b, "-ar-Tqa"), None);
    assert_eq!(g(&b, "ar-TqaA"), None);
    assert_eq!(g(&b, "ar-Tqa\u{e9}"), Some("Tqa".into()));
    let h = handback();
    let m = |s: &str| full_match(&h, s);
    let got = m("Task: Tqb | done: a | b | est: x | y | remaining: z | w | pct: 3");
    assert_eq!(got, Some(vec!["Tqb".into(), "a | b".into(), "3".into()]));
    assert_eq!(m("Task: Tqb | done: d | est: e | pct: 5"), None);
    assert_eq!(
        m("Task: Tqb | done: d | est: e | remaining: r | pct: 1000"),
        None
    );
}

/// `\d` and `\s` are Unicode in `re` on `str`; the lookarounds are ASCII classes.
#[test]
fn unicode_digits_and_spaces_follow_python_re() {
    let t = tag();
    assert_eq!(
        g(&t, "Task:\u{3000}\u{2003}\tar-Tqj"),
        Some("ar-Tqj".into())
    );
    // `@` and four Arabic-Indic digits are consumed, then the lookahead
    // fails on `x`, so the engine backs off to the match without them.
    assert_eq!(
        g(&t, "Task: Tqa@\u{661}\u{662}\u{663}\u{664}x"),
        Some("Tqa".into())
    );
    assert_eq!(
        g(&t, "Task:\u{a0}Tqa\u{e9}"),
        Some("Tqa".into()),
        "the lookahead is ASCII"
    );
    assert_eq!(
        g(&t, "Task: Tqa@\u{661}\u{662}\u{663}\u{664}\u{665}"),
        Some("Tqa".into())
    );
    assert_eq!(
        g(&t, "\u{e9}Task: Tqa"),
        None,
        "\\b sees a Unicode word character"
    );
    let p = Patterns::new();
    let hb = super::sources::parse_handback(
        &p,
        "Task:\u{2003}Tqb\u{2003}|\u{2003}done:\u{2003}d\u{2003}|\u{2003}est: e | remaining: r | pct: \u{669}\u{669}\u{2003}",
    );
    assert_eq!(hb, Some(("Tqb".into(), "d".into(), 99)));
}

#[test]
fn one_paragraph_cuts_by_code_point() {
    assert_eq!(one_paragraph("  a\n  b\n \t\n c", 400), "a b");
    assert_eq!(one_paragraph("x\n\u{2003}\n\u{a0}y", 400), "x");
    assert_eq!(
        one_paragraph(&"\u{e9}".repeat(401), 400),
        format!("{}…", "\u{e9}".repeat(399))
    );
    assert_eq!(
        one_paragraph(&"\u{e9}".repeat(400), 400),
        "\u{e9}".repeat(400)
    );
    assert_eq!(one_paragraph("ab  cd", 4), "ab…");
}

fn doc(id: &str, data: &str) -> PyDocOut {
    PyDocOut {
        id: id.into(),
        version: PyInt::Small(1),
        data: loads(data).unwrap_or(PyValue::Null),
    }
}

/// Ruled divergence, #12029 P2h Q5: the answer entry's text is neutral.
#[test]
fn item_entries_use_the_neutral_answer_text() {
    let docs = [doc(
        "D01",
        r#"{"kind": "question", "task": "Tqa", "answered_at": "t", "answer": "yes", "answer_note": "later"}"#,
    )];
    let got = item_entries(&docs);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, format!("{ANSWERED} D01: yes: later"));
    assert_eq!(got[0].text, "Owner answered D01: yes: later");
}

#[test]
fn pick_task_items_prefers_open_then_oldest() {
    let docs = [
        doc(
            "B01",
            r#"{"kind": "task", "task": "Tqa", "status": "done", "created": "1"}"#,
        ),
        doc(
            "B02",
            r#"{"kind": "task", "task": "ar-TQA", "created": "3"}"#,
        ),
        doc("B03", r#"{"kind": "task", "task": "Tqa", "created": "2"}"#),
        doc("B04", r#"{"kind": "task", "task": "Tqa", "created": "2"}"#),
        doc("B05", r#"{"kind": "task", "task": "Tqb"}"#),
        doc("B06", r#"{"kind": "task", "task": "Tqb", "created": "z"}"#),
    ];
    let got = pick_task_items(&docs);
    let ids: Vec<(&str, &str)> = got
        .iter()
        .map(|(t, i)| (t.as_str(), docs[*i].id.as_str()))
        .collect();
    assert_eq!(ids, [("tqa", "B03"), ("tqb", "B06")]);
}

fn seeded(fmt: &str) -> Result<(tempfile::TempDir, PyStore), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let st = PyStore::for_test(&tmp.path().join("q"), fmt);
    st.set_json(
        "items",
        "B01",
        r#"{"kind": "task", "task": "Tqa", "status": "open"}"#,
    )?;
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox)?;
    std::fs::write(
        inbox.join("relays.jsonl"),
        "{\"ts\": \"2026-11-26T09:00:00Z\", \"text\": \"Task: Tqa one\"}\n",
    )?;
    Ok((tmp, st))
}

#[test]
fn ingest_is_idempotent_and_dedups_on_at_and_text() -> R {
    for fmt in ["dir", "jsonl"] {
        let (tmp, st) = seeded(fmt)?;
        let inbox = tmp.path().join("inbox");
        let dup = "{\"ts\": \"2026-11-26T09:00:00Z\", \"text\": \"Task: Tqa one\"}\n";
        let text = std::fs::read_to_string(inbox.join("relays.jsonl"))? + dup;
        std::fs::write(inbox.join("relays.jsonl"), text)?;
        let first = ingest(&st, &inbox, NOW, 72.0)?;
        assert_eq!(first.added, [("B01".to_owned(), 1)], "{fmt}");
        let again = ingest(&st, &inbox, NOW, 72.0)?;
        assert!(
            again.added.is_empty() && again.errors.is_empty(),
            "{fmt}: {again:?}"
        );
        assert_eq!(st.get_item("B01")?.version, PyInt::Small(2), "{fmt}");
    }
    Ok(())
}

/// Fail-open parity (#12029 P2h Q6): a listing that fails is an `errors` entry, exit 0.
#[test]
fn a_list_docs_failure_is_an_errors_entry() -> R {
    let tmp = tempfile::tempdir()?;
    let items = tmp.path().join("local/owner-queue/items");
    std::fs::create_dir_all(&items)?;
    std::fs::write(items.join("B01.json"), "{not json")?;
    let out = cli(tmp.path(), &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .starts_with("{\"added\": {}, \"conflicts\": [], \"errors\": [\"unreadable document "),
        "{}",
        out.stdout
    );
    Ok(())
}

/// The store a racing writer changes between the listing and the write.
struct Racing(PyStore);

impl ItemStore for Racing {
    fn list_items(&self) -> Result<Vec<PyDocOut>, PyStoreError> {
        let docs = self.0.list_items()?;
        let body = loads(r#"{"title": "changed meanwhile"}"#)
            .map_err(|e| PyStoreError::Store(e.py_str()))?;
        self.0.update_item("B01", &body, &PyInt::Small(1), None)?;
        Ok(docs)
    }
    fn get_item(&self, id: &str) -> Result<PyDocOut, PyStoreError> {
        self.0.get_item(id)
    }
    fn update_item(
        &self,
        id: &str,
        d: &PyValue,
        v: &PyInt,
        a: Option<&str>,
    ) -> Result<PyInt, PyStoreError> {
        self.0.update_item(id, d, v, a)
    }
    fn now_iso(&self) -> String {
        self.0.now_iso()
    }
}

#[test]
fn a_conflict_is_reported_and_writes_nothing() -> R {
    let (tmp, st) = seeded("dir")?;
    let racing = Racing(st);
    let got = ingest(&racing, &tmp.path().join("inbox"), NOW, 72.0)?;
    assert_eq!(got.conflicts, ["B01"]);
    assert!(got.added.is_empty() && got.errors.is_empty(), "{got:?}");
    let after = racing.0.get_item("B01")?;
    assert_eq!(
        after.version,
        PyInt::Small(2),
        "only the racing write landed"
    );
    assert!(crate::pyval::get(&after.data, "updates").is_none());
    Ok(())
}

#[test]
fn an_unparseable_inbox_line_is_skipped() -> R {
    let tmp = tempfile::tempdir()?;
    std::fs::write(
        tmp.path().join("relays.jsonl"),
        "not json\n[1]\n\n{\"ts\": \"t\", \"text\": \"Task: Tqa\"}\n",
    )?;
    let got = candidates(&Patterns::new(), tmp.path())?;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].task, "Tqa");
    std::fs::write(tmp.path().join("relays.jsonl"), b"{}\n\xff\n")?;
    let bad = candidates(&Patterns::new(), tmp.path());
    assert!(
        matches!(&bad, Err(super::Fail::Raise(l)) if l.starts_with("UnicodeDecodeError: ")),
        "{bad:?}"
    );
    Ok(())
}

fn cli(checkout: &Path, args: &[&str]) -> Outcome {
    let env = |_: &str| None;
    let o = super::cli::Options {
        checkout: checkout.to_path_buf(),
        cwd: checkout.to_path_buf(),
        env: &env,
        clock: Arc::new(FakeClock::new(NOW)),
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    super::cli::run(&args, &o)
}

#[test]
fn note_bad_usage_exits_2_and_writes_nothing() -> R {
    let tmp = tempfile::tempdir()?;
    for args in [
        &["note", "Tqa"][..],
        &["note", "Tqa", "x", "--by"],
        &["note", "Tqa", "x", "--by", ""],
        &["note", "a", "b", "c"],
    ] {
        let out = cli(tmp.path(), args);
        assert_eq!((out.code, out.stdout.as_str()), (2, ""), "{args:?}");
        assert_eq!(out.stderr, super::cli::NOTE_USAGE);
    }
    assert!(!tmp.path().join("inbox").exists());
    assert!(!tmp.path().join("local").exists());
    Ok(())
}

/// #12029 AC2: an ingest write waits for the shared `flock`, and a store
/// claimed by the other writer class refuses it with nothing written.
#[test]
fn dashboard_updates_writes_take_the_shared_store_lock() -> R {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    let store = root.join("local/owner-queue");
    let st = PyStore::for_test(&store, "dir");
    st.set_json(
        "items",
        "B01",
        r#"{"kind": "task", "task": "Tqa", "status": "open"}"#,
    )?;
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.join(".lock"))?;
    // SAFETY: the fd stays open for `held`'s life; flock touches nothing else.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let worker = {
        let root = root.clone();
        std::thread::spawn(move || cli(&root, &["note", "Tqa", "while locked"]))
    };
    std::thread::sleep(Duration::from_millis(400));
    let b01 = store.join("items/B01.json");
    assert!(
        !std::fs::read_to_string(&b01)?.contains("while locked"),
        "written while the lock was held"
    );
    drop(held);
    let out = worker.join().map_err(|_| "the ingest panicked")?;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(std::fs::read_to_string(&b01)?.contains("while locked"));
    // The other writer class owns `items` in a second store: the ingest is refused.
    let other = root.join("other");
    let rust = crate::dashboard::store::Store::new(
        other.join("local/owner-queue"),
        crate::dashboard::store::Format::Dir,
    )
    .with_secret_scan(|_| None)
    .with_writer_claim();
    rust.set(
        "items",
        "B01",
        serde_json::json!({"kind": "task", "task": "Tqa", "status": "open"}),
        None,
    )?;
    let before = std::fs::read_to_string(other.join("local/owner-queue/items/B01.json"))?;
    let out = cli(&other, &["note", "Tqa", "refused"]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stderr.starts_with("WriterClassError: "),
        "{}",
        out.stderr
    );
    assert_eq!(
        std::fs::read_to_string(other.join("local/owner-queue/items/B01.json"))?,
        before
    );
    Ok(())
}
