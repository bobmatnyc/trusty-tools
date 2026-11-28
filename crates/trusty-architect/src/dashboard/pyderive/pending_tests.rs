//! The answer-note rule of `pending` (#12029 P2g).

use super::PySnap;
use super::pending::{pending, with_note};
use crate::pyjson_load::{PyValue, loads};
use crate::pyval::get;
use crate::tasks::store_read::PyDoc;

fn snap(items: &[(&str, &str)]) -> PySnap {
    let docs = items
        .iter()
        .map(|(id, body)| PyDoc {
            id: (*id).to_owned(),
            version: 1,
            data: loads(body).unwrap_or(PyValue::Null),
        })
        .collect();
    PySnap::from([("items".to_owned(), docs)])
}

fn texts(s: &PySnap) -> Vec<String> {
    pending(s, 1_795_784_400.0)
        .iter()
        .map(|e| match get(e, "text") {
            Some(PyValue::Str(t)) => t.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// An answered item and an Architect Do item both carry `: <note>`, trimmed;
/// a blank or non-string note adds nothing.
#[test]
fn answered_text_carries_the_trimmed_answer_note() {
    let s = snap(&[
        (
            "D01",
            r#"{"status": "answered", "answer": "Other", "answer_note": "  ship the small one "}"#,
        ),
        (
            "D02",
            r#"{"status": "answered", "answer": "Architect Do", "answer_note": "via the PM", "task": "il-1", "session": "s"}"#,
        ),
        (
            "D03",
            r#"{"status": "answered", "answer": "Yes", "answer_note": " \t "}"#,
        ),
        (
            "D04",
            r#"{"status": "answered", "answer": "No", "answer_note": 7}"#,
        ),
        ("D05", r#"{"status": "answered", "answer": "Architect Do"}"#),
    ]);
    assert_eq!(
        texts(&s),
        [
            "Other: ship the small one",
            "Architect Do: via the PM",
            "Yes",
            "No",
            "Architect Do (needs target resolution)",
        ]
    );
    assert_eq!(with_note("A".into(), None), "A");
}
