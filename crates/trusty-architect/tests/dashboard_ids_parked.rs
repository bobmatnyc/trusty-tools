//! #12029 P2d: the two live defects the slice fixes, through the public API.
//!
//! `derive::is_parked` and the `tally` footer count a bare `parked` flag and
//! an `indefinite` choice as parked, as `dashboard_derive.is_parked` does.
//! The `ids` verb issues base-34 ids, keeps every hex-era id on disk counted
//! as used, never issues one of them again, and retires by the
//! `dashboard_derive.retire_candidates` rules. Every store is synthetic.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use trusty_architect::FakeClock;
use trusty_architect::dashboard::derive::is_parked;
use trusty_architect::dashboard::rpc::{IdsRequest, StoreSel};
use trusty_architect::dashboard::verbs::{self, Env};
use trusty_architect::dashboard::{Format, Op, Store, tally};

type R = Result<(), Box<dyn std::error::Error>>;

/// 2026-10-21T10:00:00Z.
const NOW: f64 = 1_792_576_800.0;

fn env() -> Env {
    Env {
        scan: |_| None,
        clock: Arc::new(FakeClock::new(NOW)),
    }
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn put(store: &Store, docs: &[(String, Value)]) -> R {
    let ops: Vec<Op> = docs
        .iter()
        .map(|(id, data)| Op::Set {
            collection: "items".into(),
            id: id.clone(),
            data: data.clone(),
            if_version: None,
        })
        .collect();
    for chunk in ops.chunks(200) {
        store.batch(chunk)?;
    }
    Ok(())
}

fn ids(
    root: &std::path::Path,
) -> Result<trusty_architect::dashboard::rpc::IdsResponse, Box<dyn std::error::Error>> {
    let store: StoreSel = serde_json::from_value(json!({"root": root}))?;
    Ok(verbs::ids(
        &env(),
        &IdsRequest {
            store,
            prefix: "D".into(),
        },
    )?)
}

/// Base-34 digits for `n` (`0-9a-z` without `l` and `o`).
fn b34(n: usize) -> String {
    const D: &[u8] = b"0123456789abcdefghijkmnpqrstuvwxyz";
    format!("D{}{}", char::from(D[n / 34]), char::from(D[n % 34]))
}

#[test]
fn a_bare_parked_flag_and_an_indefinite_choice_count_as_parked() -> R {
    let open = |pk: Value| obj(json!({"status": "open", "kind": "question", "parked": pk}));
    assert!(is_parked(&open(json!(true))), "a bare flag parks");
    assert!(
        is_parked(&open(json!("waiting on the vendor"))),
        "a note parks"
    );
    assert!(is_parked(&open(json!({"choice": "indefinite"}))));
    assert!(!is_parked(&open(json!({"choice": "later"}))));
    assert!(!is_parked(&obj(json!({"status": "done", "parked": true}))));
    let dir = tempfile::tempdir()?;
    let store = Store::new(dir.path(), Format::Dir).with_secret_scan(|_| None);
    put(
        &store,
        &[
            (
                "D01".into(),
                json!({"kind": "question", "status": "open", "parked": true}),
            ),
            (
                "D02".into(),
                json!({"kind": "question", "status": "open", "parked": {"choice": "indefinite"}}),
            ),
            ("D03".into(), json!({"kind": "question", "status": "open"})),
        ],
    )?;
    let t = tally(&store, NOW)?;
    assert_eq!((t.parked, t.open, t.questions), (2, 1, 1), "{t:?}");
    Ok(())
}

#[test]
fn ids_verb_issues_base34_and_never_collides_with_old_hex_ids() -> R {
    let dir = tempfile::tempdir()?;
    let store = Store::new(dir.path(), Format::Dir).with_secret_scan(|_| None);
    // Hex-era ids already on disk (`D9c` is 0x9c in hex and 318 in base 34)
    // and base-34 ids side by side.
    let old_hex = ["D01", "D09", "D0a", "D0f", "D10", "D9c", "Dff"];
    let new_b34 = ["D0g", "D0z", "Dfg"];
    let mut docs: Vec<(String, Value)> = old_hex
        .iter()
        .chain(new_b34.iter())
        .map(|id| ((*id).to_owned(), json!({"status": "open"})))
        .collect();
    put(&store, &docs)?;
    let first = ids(dir.path())?;
    assert_eq!(first.free, 1155 - 10, "every old and new id is counted");
    // The highest used is `Dfg` (543); the next is the one after it.
    assert_eq!(first.next.as_deref(), Some("Dfh"));
    // Issue 40 more through the verb: none is an id already on disk.
    let mut seen: BTreeSet<String> = docs.iter().map(|d| d.0.clone()).collect();
    for _ in 0..40 {
        let next = ids(dir.path())?.next.ok_or("space full")?;
        assert!(seen.insert(next.clone()), "{next} collides");
        let digits = next.get(1..).ok_or("short id")?;
        assert!(
            digits
                .chars()
                .all(|c| c.is_ascii_alphanumeric() && c != 'l' && c != 'o')
        );
        docs = vec![(next, json!({"status": "open"}))];
        put(&store, &docs)?;
    }
    assert_eq!(ids(dir.path())?.free, 1155 - 50);
    Ok(())
}

#[test]
fn ids_verb_retires_by_the_derive_rules() -> R {
    let dir = tempfile::tempdir()?;
    let store = Store::new(dir.path(), Format::Dir).with_secret_scan(|_| None);
    let old = "2026-10-01T00:00:00Z";
    let mut docs: Vec<(String, Value)> = (1..=880)
        .map(|n| (b34(n), json!({"status": "open"})))
        .collect();
    docs[0].1 = json!({"status": "done", "done_at": old});
    docs[1].1 = json!({"status": "answered", "relayed": "x", "relayed_at": "2026-10-02T00:00:00Z"});
    docs[2].1 = json!({"status": "done", "done_at": old, "keep": true});
    docs[3].1 = json!({"status": "done", "done_at": "2026-10-21T09:00:00Z"});
    docs[4].1 = json!({"status": "wont_do", "wont_do_at": "2026-10-03T00:00:00Z"});
    put(&store, &docs)?;
    let got = ids(dir.path())?;
    assert_eq!(got.free, 275);
    // Answered-and-relayed and won't-do retire; `keep` and a done item
    // touched within 24 h do not.
    assert_eq!(got.retire, ["D01", "D02", "D05"]);
    Ok(())
}
