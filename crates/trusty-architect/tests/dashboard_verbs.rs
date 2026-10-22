//! #10063 PR 3: the `mpm.architect.dashboard.*` bodies, their exit classes
//! and the CLI's stdout shapes (`dashboard-db.py`'s `emit`).

use std::sync::Arc;

use serde_json::json;
use trusty_architect::FakeClock;
use trusty_architect::dashboard::DashError;
use trusty_architect::dashboard::rpc::{
    BatchRequest, GetRequest, IdsRequest, METHODS, StoreRequest, StoreSel, WriteRequest,
};
use trusty_architect::dashboard::verbs::{self, Env, Reply, render};
use trusty_architect::ledger::rpc::RpcErrorCode;

type R = Result<(), Box<dyn std::error::Error>>;

fn env() -> Env {
    Env {
        scan: |_| None,
        clock: Arc::new(FakeClock::new(1_792_576_800.0)),
    }
}

fn sel(root: &std::path::Path) -> StoreSel {
    serde_json::from_value(json!({"root": root})).expect("store selection")
}

#[test]
fn dashboard_rpc_classes_fix_the_exit_codes() -> R {
    assert_eq!(METHODS.len(), 12);
    assert!(
        METHODS
            .iter()
            .all(|m| m.starts_with("mpm.architect.dashboard."))
    );
    let tmp = tempfile::tempdir()?;
    let store = sel(&tmp.path().join("store"));
    let w = |data, if_version| WriteRequest {
        store: store.clone(),
        collection: "items".into(),
        id: "D01".into(),
        data,
        if_version,
    };
    verbs::set(&env(), &w(json!({"t": 1}), None))?;
    let blind = verbs::set(&env(), &w(json!({"t": 2}), None))
        .err()
        .ok_or("replaced")?;
    let stale = verbs::set(&env(), &w(json!({"t": 2}), Some(9)))
        .err()
        .ok_or("stale")?;
    for e in [&blind, &stale] {
        assert!(matches!(e, DashError::Conflict(_)), "{e:?}");
        assert_eq!(e.code().exit_code(), 1);
    }
    let bad = verbs::set(&env(), &w(json!({"version": 3}), Some(1)))
        .err()
        .ok_or("version")?;
    assert_eq!(bad.code(), RpcErrorCode::Invalid);
    let relative = StoreSel {
        root: "rel/store".into(),
        ..store.clone()
    };
    let got = verbs::fsck(&env(), &StoreRequest { store: relative });
    assert!(matches!(got, Err(DashError::Invalid(_))), "{got:?}");
    let missing = GetRequest {
        store: store.clone(),
        collection: "items".into(),
        id: "D09".into(),
    };
    assert_eq!(
        verbs::get(&env(), &missing)
            .err()
            .ok_or("found")?
            .code()
            .exit_code(),
        2
    );
    let unknown = serde_json::from_value::<StoreRequest>(json!({"store": store, "x": 1}));
    assert!(unknown.is_err(), "unknown key accepted");
    Ok(())
}

/// Every file under `root` with its bytes.
fn snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

// #10063: AC7 re-scoped to store lock (Arch df-Db7)
#[test]
fn dashboard_verb_store_refuses_a_second_writer_while_the_lock_is_held() -> R {
    use std::os::fd::AsRawFd;
    let tmp = tempfile::tempdir()?;
    let store = sel(&tmp.path().join("store"));
    let seed = WriteRequest {
        store: store.clone(),
        collection: "items".into(),
        id: "D01".into(),
        data: json!({"t": 1}),
        if_version: None,
    };
    verbs::set(&env(), &seed)?;
    let before = snapshot(tmp.path());
    // The first writer: an exclusive flock on `<root>/.lock`, as `dashboard_store.py` takes.
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(store.root.join(".lock"))?;
    // SAFETY: the fd stays open for `held`'s life; flock touches nothing else.
    let rc = unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "the test could not take the lock");
    // The second writer: the store the daemon and `tm architect dashboard`
    // open for a write; a zero wait makes its flock one non-blocking attempt.
    let writer = verbs::open(&store, &env(), true)?.with_lock_wait(std::time::Duration::ZERO);
    let got = writer
        .set("items", "D02", json!({}), Some(0))
        .err()
        .ok_or("written while the lock was held")?;
    assert!(matches!(got, DashError::Unavailable(_)), "{got:?}");
    assert_eq!(got.code(), RpcErrorCode::Unavailable);
    assert_eq!(got.code().exit_code(), 3);
    assert_eq!(
        snapshot(tmp.path()),
        before,
        "a refused write changed bytes"
    );
    drop(held);
    writer.set("items", "D02", json!({}), Some(0))?;
    Ok(())
}

#[test]
fn dashboard_verbs_render_the_prototype_shapes() -> R {
    let tmp = tempfile::tempdir()?;
    let store = sel(&tmp.path().join("store"));
    let ops = vec![
        json!({"op": "set", "collection": "items", "id": "D01",
            "data": {"kind": "question", "status": "open", "title": "é", "a": [1]}}),
        json!({"op": "set", "collection": "items", "id": "T01",
            "data": {"kind": "task", "status": "open", "title": "t"}}),
    ];
    let done = verbs::batch(
        &env(),
        &BatchRequest {
            store: store.clone(),
            ops,
        },
    )?;
    assert_eq!(
        render(&Reply::Ops(done)),
        "[\n  {\n    \"collection\": \"items\",\n    \"id\": \"D01\",\n    \"version\": 1\n  },\n  \
         {\n    \"collection\": \"items\",\n    \"id\": \"T01\",\n    \"version\": 1\n  }\n]\n"
    );
    let doc = verbs::get(
        &env(),
        &GetRequest {
            store: store.clone(),
            collection: "items".into(),
            id: "D01".into(),
        },
    )?;
    assert_eq!(
        render(&Reply::Doc(doc)),
        "{\n  \"id\": \"D01\",\n  \"version\": 1,\n  \"data\": {\n    \"a\": [\n      1\n    ],\n    \
         \"kind\": \"question\",\n    \"status\": \"open\",\n    \"title\": \"é\"\n  }\n}\n"
    );
    let t = verbs::tally_verb(
        &env(),
        &StoreRequest {
            store: store.clone(),
        },
    )?;
    assert_eq!(
        render(&Reply::Tally(t.clone(), false)),
        "2 open dashboard items: 1 question, 0 actions, 1 task\n"
    );
    assert!(render(&Reply::Tally(t, true)).starts_with("{\n  \"open\": 2,\n  \"questions\": 1,"));
    let ids = verbs::ids(
        &env(),
        &IdsRequest {
            store: store.clone(),
            prefix: "D".into(),
        },
    )?;
    assert_eq!(ids.next.as_deref(), Some("D02"));
    assert_eq!(ids.free, 255);
    let bad = verbs::ids(
        &env(),
        &IdsRequest {
            store,
            prefix: "d".into(),
        },
    );
    assert!(matches!(bad, Err(DashError::Invalid(_))), "{bad:?}");
    Ok(())
}
