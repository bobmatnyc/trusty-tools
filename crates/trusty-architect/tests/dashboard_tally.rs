//! #10063 PR 1: counts come from `tally` only, and an unreadable store is an
//! error, never a zero (DOC-82 LDG-5.7, #10063 AC 6). The expected strings
//! are `dashboard-db.py`'s output on the same documents with the same clock.

use std::fs;

use serde_json::json;
use trusty_architect::dashboard::{
    DashError, FooterLinks, Format, Store, footer, tally, tally_line,
};

type R = Result<(), Box<dyn std::error::Error>>;

/// A test scanner: nothing is secret-shaped (#10063 PR 2: a store with no
/// scan refuses writes).
fn no_secret(_: &str) -> Option<String> {
    None
}

/// 2026-10-21T10:00:00Z.
const NOW: f64 = 1_792_576_800.0;

fn links() -> FooterLinks {
    FooterLinks {
        local: "L".into(),
        tailnet: "T".into(),
    }
}

/// The prototype footer test's mixed store, plus two open questions.
fn mixed(store: &Store) -> Result<(), DashError> {
    for (id, data) in [
        ("D01", json!({"kind": "question", "status": "open"})),
        (
            "D02",
            json!({"kind": "question", "status": "open", "parked": {"until": "2999-01-01"}}),
        ),
        ("A01", json!({"kind": "action", "status": "open"})),
        (
            "T01",
            json!({"kind": "task", "status": "open", "run_start": "2000-01-01", "run_end": "2999-01-01"}),
        ),
        ("D03", json!({"kind": "question", "status": "answered"})),
        ("D04", json!({"kind": "question", "status": "done"})),
    ] {
        store.set("items", id, data, None)?;
    }
    for (id, status) in [("Q01", "open"), ("Q02", "open"), ("Q03", "cleared")] {
        store.set("questions", id, json!({"status": status}), None)?;
    }
    Ok(())
}

#[test]
fn tally_counts_kinds_parked_running_and_waiting() -> R {
    for format in [Format::Dir, Format::Jsonl] {
        let tmp = tempfile::tempdir()?;
        let store = Store::new(tmp.path(), format).with_secret_scan(no_secret);
        mixed(&store)?;
        let t = tally(&store, NOW)?;
        assert_eq!(
            serde_json::to_value(&t)?,
            json!({"open": 3, "questions": 1, "actions": 1, "tasks": 1, "answered_unrelayed": 1,
                   "parked": 1, "running": 1, "answers_waiting": 2})
        );
        let one = Store::new(tmp.path().join("one"), format).with_secret_scan(no_secret);
        one.set(
            "items",
            "A01",
            json!({"kind": "action", "status": "open"}),
            None,
        )?;
        let t1 = tally(&one, NOW)?;
        assert_eq!(
            tally_line(&t1),
            "1 open dashboard item: 0 questions, 1 action, 0 tasks"
        );
        assert!(serde_json::to_value(&t1)?.get("answers_waiting").is_none());
    }
    Ok(())
}

#[test]
fn footer_equals_tally_line() -> R {
    for format in [Format::Dir, Format::Jsonl] {
        let tmp = tempfile::tempdir()?;
        let store = Store::new(tmp.path(), format).with_secret_scan(no_secret);
        mixed(&store)?;
        let f = footer(&store, NOW, &links())?;
        assert_eq!(
            f.footer,
            "Dashboard: [localhost](L) \u{b7} [tailnet](T) \u{2014} 3 open dashboard items: 1 question, 1 action, \
             1 task (1 parked) (1 running), 2 answers waiting"
        );
        assert_eq!(f.tally, tally(&store, NOW)?);
        assert!(f.footer.ends_with(&tally_line(&f.tally)));
        assert_eq!(footer(&store, NOW, &links())?, f, "footer is deterministic");
    }
    Ok(())
}

#[test]
fn unreadable_store_is_error_not_zero() -> R {
    let tmp = tempfile::tempdir()?;
    let missing = Store::new(tmp.path().join("missing"), Format::Dir);
    assert!(matches!(tally(&missing, NOW), Err(DashError::Io { .. })));
    assert!(footer(&missing, NOW, &links()).is_err());
    let file_root = tmp.path().join("file");
    fs::write(&file_root, "not a directory")?;
    assert!(tally(&Store::new(&file_root, Format::Jsonl), NOW).is_err());

    for format in [Format::Dir, Format::Jsonl] {
        let root = tmp.path().join(format.to_string());
        let store = Store::new(&root, format).with_secret_scan(no_secret);
        mixed(&store)?;
        let path = match format {
            Format::Dir => root.join("items/D01.json"),
            Format::Jsonl => root.join("items.jsonl"),
        };
        fs::write(&path, "{not json")?;
        assert!(
            matches!(tally(&store, NOW), Err(DashError::Corrupt { .. })),
            "{format}"
        );
        assert!(footer(&store, NOW, &links()).is_err(), "{format}");
        let other = match format {
            Format::Dir => Format::Jsonl,
            Format::Jsonl => Format::Dir,
        };
        let wrong = Store::new(&root, other);
        assert!(
            matches!(tally(&wrong, NOW), Err(DashError::WrongFormat(_))),
            "{format} read as {other}"
        );
    }
    Ok(())
}
