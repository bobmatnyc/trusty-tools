//! Live check for later: the broker drives the SignedCheck fixture through a bounded daemon.
//!
//! Why: the fake daemon cannot prove the broker's pipeline against the real
//! cua-driver: redaction of a real menu bar, a read-back confirmed click on
//! the fixture, and a second session queued behind the first (plan section 8).
//! What: builds and opens `tests/fixtures/signedapp` (bundle [`FIXTURE_BUNDLE`];
//! #11699: cua-driver 0.23.2 sees no AXWindow in the osascript `fixture.js`),
//! so the real `lsappinfo` resolver names the fixture's own bundle; then builds
//! an in-process broker on the real launcher in a temp state dir, mints two
//! grants through the consent flow (fake dialog) for the fixture's pid and
//! window only, then over
//! `agent.sock` connects, snapshots (no `AXMenuBar` in the reply), clicks
//! "Press Me" (confirmed, fixture click count +1), and checks that a second
//! session is queued. It refuses to run unless `TRUSTY_SANDBOX=1`, so it never
//! runs with the operator's HOME, and it never names any other app or daemon.
//! Run: `TRUSTY_SANDBOX=1 cargo test -p trusty-computer --test live_broker -- --ignored --nocapture`.
//! Test: this file.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::dialog::FakeDialog;
use common::live::Live;
use common::rig::{Conn, consent};
use serde_json::json;
use trusty_computer::broker::{Broker, wall_clock};
use trusty_computer::broker_server::BrokerServer;
use trusty_computer::launcher::CuaAppLauncher;
use trusty_computer::manifest::LeaseScope;
use trusty_computer::supervisor::Supervisor;

/// The fixture app's own bundle id (#11460: the grant names what `lsappinfo` reports).
const FIXTURE_BUNDLE: &str = "com.trusty.spike.signedcheck";

#[test]
#[ignore = "live, Phase 7: drives CuaDriver.app and the SignedCheck fixture; needs TRUSTY_SANDBOX=1 and an unlocked screen"]
fn live_broker_drives_fixture_and_queues_second_session() {
    assert_eq!(
        std::env::var("TRUSTY_SANDBOX").as_deref(),
        Ok("1"),
        "run only inside scripts/sandbox_computer_daemon.sh's environment"
    );
    let mut fixture = Live::new("tcb");
    let state: PathBuf = fixture.dir.join("state");
    let (pid, wid, _) = fixture.launch("fixture", &[]);
    let scope = LeaseScope {
        pid: u32::try_from(pid).expect("pid"),
        window_id: wid,
    };

    let sup = Supervisor::new(CuaAppLauncher, &state).expect("supervisor");
    let dialog = FakeDialog::confirming();
    let broker = Broker::new(sup, &state, wall_clock(), dialog.clone()).expect("broker");
    let broker = Arc::new(broker);
    let mut server = BrokerServer::bind(&state).expect("bind");
    server.spawn(Arc::clone(&broker));
    let req = json!({"op": "grant_request", "bundle_id": FIXTURE_BUNDLE, "pid": scope.pid,
        "window_id": scope.window_id});
    let mint = |what: &str| {
        let reply = consent(&server.operator, &dialog, &req);
        let field = |v: &serde_json::Value| {
            v.as_str()
                .unwrap_or_else(|| panic!("{what}: broker replied {reply}"))
                .to_owned()
        };
        let id = field(&reply["result"]["grant"]["id"]);
        let code = field(&reply["result"]["code"]);
        (broker.lock().grant(&id).cloned().expect(what), code)
    };
    let (grant, code) = mint("first grant");
    let (_, second_code) = mint("second grant");
    assert_eq!(
        (grant.bundle_id.as_str(), grant.scope),
        (FIXTURE_BUNDLE, scope)
    );

    let mut agent = Conn::open(&server.agent);
    let held = agent.send(&json!({"op": "connect", "code": code}));
    assert_eq!(held["result"]["lease"], "held", "{held}");
    let mut second = Conn::open(&server.agent);
    let queued = second.send(&json!({"op": "connect", "code": second_code}));
    assert_eq!(queued["result"]["lease"], "queued", "{queued}");

    let args = json!({"pid": pid, "window_id": wid});
    let call = |rid: &str, tool: &str, args: serde_json::Value| json!({"op": "call", "grant_id": grant.id, "request_id": rid, "tool": tool, "args": args});
    let snap = agent.send(&call("s1", "get_window_state", args));
    assert_eq!(snap["ok"], true, "{snap}");
    assert!(!snap.to_string().contains("AXMenuBar"));
    let token = snap["result"]["structuredContent"]["elements"]
        .as_array()
        .and_then(|els| els.iter().find(|e| e["label"] == "Press Me"))
        .and_then(|e| e["element_token"].as_str())
        .map(str::to_owned)
        .expect("Press Me in the fixture snapshot");
    let click = agent.send(&call(
        "c1",
        "click",
        json!({"pid": pid, "window_id": wid, "element_token": token}),
    ));
    println!("click: {click}");
    assert_eq!(click["confirmation"]["confirmed"], true, "{click}");
    let _ = broker.lock().supervisor.stop();
    drop(fixture);
}
