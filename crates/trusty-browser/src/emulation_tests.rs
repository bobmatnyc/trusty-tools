//! Unit tests for `emulation.rs`: field validation and the ended-lease selection.

use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::broker::{Broker, BrokerOptions, Claim};
use crate::vault::MemoryKeyStore;

fn frame(js: Json, scheme: Json, w: Json, h: Json) -> Json {
    Json::object([
        ("op", Json::from("page_emulate")),
        ("lease", Json::from("l")),
        ("javascript", js),
        ("color_scheme", scheme),
        ("width", w),
        ("height", h),
    ])
}

#[test]
fn emulation_fields_are_validated() {
    let ok = |js, scheme, w, h| valid(&frame(js, scheme, w, h));
    let (t, f, null, i) = (Json::Bool(true), Json::Bool(false), Json::Null, Json::Int);
    assert!(ok(t.clone(), null.clone(), i(0), i(0)), "the defaults");
    assert!(ok(f.clone(), Json::from("dark"), i(1440), i(900)));
    assert!(ok(t.clone(), Json::from("light"), i(MAX_SIDE), i(1)));
    assert!(
        !ok(null.clone(), null.clone(), i(0), i(0)),
        "javascript must be a bool"
    );
    assert!(
        !ok(t.clone(), Json::from("dim"), i(0), i(0)),
        "unknown scheme"
    );
    assert!(!ok(t.clone(), null.clone(), i(1440), i(0)), "one side only");
    assert!(!ok(t.clone(), null.clone(), i(-1), i(-1)), "negative");
    assert!(
        !ok(t.clone(), null.clone(), i(MAX_SIDE + 1), i(9)),
        "too wide"
    );
    assert!(!ok(t, null, Json::Float(1.5), i(9)), "not an int");
    let mut extra = frame(f, Json::Null, i(0), i(0));
    extra.insert("request_id", Json::from("r"));
    assert_eq!(
        crate::pages::validate(&extra).map_err(|r| r.code),
        Err("InvalidRequest".to_owned()),
        "page_emulate takes no request_id"
    );
}

fn claim(token: &str, deadline: f64) -> Claim {
    Claim {
        generation: "gen".into(),
        tab: 7,
        owner: "owner".into(),
        owner_session: 1,
        token: token.into(),
        deadline,
        busy: false,
        document: None,
        origin: None,
        indicator: false,
    }
}

fn record(token: &str, tab: i64) -> Record {
    Record {
        generation: "gen".into(),
        tab,
        token: token.into(),
        grant: "g".into(),
    }
}

/// A released, an expired and a never-held lease end; a live one does not.
/// No worker is connected, so no ended record carries a reset route.
#[tokio::test]
async fn ended_records_cover_every_lease_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let options = BrokerOptions::new(dir.path(), Box::new(MemoryKeyStore::new()));
    let broker = Broker::start(options).expect("start");
    let core = broker.core();
    core.emulation.put(record("live", 7));
    core.emulation.put(record("expired", 8));
    core.emulation.put(record("released", 9));
    {
        let mut inner = core.lock();
        inner.claims.push(claim("live", 200.0));
        let mut old = claim("expired", 100.0);
        old.tab = 8;
        inner.claims.push(old);
        let gone = ended(core, &inner, 150.0);
        let tokens: Vec<&str> = gone.iter().map(|(r, _)| r.token.as_str()).collect();
        assert_eq!(tokens, ["expired", "released"]);
        assert!(gone.iter().all(|(_, route)| route.is_none()), "no worker");
        assert_eq!(core.emulation.snapshot(), [record("live", 7)]);
        // A busy lease never expires; a grant or session end drops the claim itself.
        inner.with_claim("live", |c| c.busy = true);
        assert!(ended(core, &inner, 1e9).is_empty());
        inner.claims.clear();
        assert_eq!(ended(core, &inner, 0.0).len(), 1);
    }
    assert!(core.emulation.snapshot().is_empty());
    // A newer lease's record replaces the older one on the same tab.
    core.emulation.put(record("a", 7));
    core.emulation.put(record("b", 7));
    assert_eq!(core.emulation.snapshot(), [record("b", 7)]);
    // A failed reset puts its record back, but never over a newer lease's record.
    core.emulation.restore(record("a", 7));
    assert_eq!(core.emulation.snapshot(), [record("b", 7)]);
    core.emulation.restore(record("a", 8));
    assert_eq!(core.emulation.snapshot(), [record("b", 7), record("a", 8)]);
    broker.close().await;
}
