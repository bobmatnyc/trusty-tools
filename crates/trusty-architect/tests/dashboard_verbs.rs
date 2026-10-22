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
