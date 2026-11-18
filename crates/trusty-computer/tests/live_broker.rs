//! Live check for later: the broker drives the JXA fixture through a bounded daemon.
//!
//! Why: the fake daemon cannot prove the broker's pipeline against the real
//! cua-driver: redaction of a real menu bar, a read-back confirmed click on
//! the fixture, and a second session queued behind the first (plan section 8).
//! What: launches `tests/fixtures/fixture.js`, builds an in-process broker on
//! the real launcher in a temp state dir, mints one grant through the
//! operator stub for the fixture's pid and window only, then over
//! `agent.sock` connects, snapshots (no `AXMenuBar` in the reply), clicks
//! "Press Me" (confirmed, fixture click count +1), and checks that a second
//! session is queued. It refuses to run unless `TRUSTY_SANDBOX=1`, so it never
//! runs with the operator's HOME, and it never names any other app or daemon.
//! Status: NOT RUN in Phase 3 (screen locked, AX returns 0 windows). Run it at
//! Phase 7 under the sandbox script:
//! `cargo test -p trusty-computer --test live_broker -- --ignored --nocapture`.
//! Test: this file.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::rig::Conn;
use serde_json::json;
use trusty_computer::broker::{Broker, wall_clock};
use trusty_computer::broker_server::BrokerServer;
use trusty_computer::grants::OperatorMint;
use trusty_computer::launcher::CuaAppLauncher;
use trusty_computer::manifest::LeaseScope;
use trusty_computer::supervisor::Supervisor;

/// Label for the fixture grant (an osascript JXA app; `launch_app` is not used).
const FIXTURE_BUNDLE: &str = "com.apple.ScriptEditor.id.trusty-computer-fixture";

struct Fixture(Child);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn state_value(path: &Path, key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.trim().parse().ok())
}

fn wait_for<T>(what: &str, secs: u64, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "live, Phase 7: drives CuaDriver.app and the JXA fixture; needs TRUSTY_SANDBOX=1 and an unlocked screen"]
fn live_broker_drives_fixture_and_queues_second_session() {
    assert_eq!(
        std::env::var("TRUSTY_SANDBOX").as_deref(),
        Ok("1"),
        "run only inside scripts/sandbox_computer_daemon.sh's environment"
    );
    let dir = tempfile::Builder::new()
        .prefix("tcb-")
        .tempdir_in("/tmp")
        .expect("tmp");
    let state: PathBuf = dir.path().join("state");
    let readback = dir.path().join("fixture-state.txt");
    let js = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fixture.js");
    let child = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript"])
        .arg(&js)
        .arg(&readback)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("launch fixture");
    let _fixture = Fixture(child);
    let pid = wait_for("fixture pid", 15, || state_value(&readback, "pid "));
    let wid = wait_for("fixture window", 15, || state_value(&readback, "window "));
    let scope = LeaseScope {
        pid: u32::try_from(pid).expect("pid"),
        window_id: wid,
    };

    let sup = Supervisor::new(CuaAppLauncher, &state).expect("supervisor");
    let broker = Arc::new(Broker::new(sup, &state, wall_clock()).expect("broker"));
    let mut server = BrokerServer::bind(&state).expect("bind");
    server.spawn(Arc::clone(&broker));
    let grant = broker
        .operator_mint(OperatorMint::stub(FIXTURE_BUNDLE, scope))
        .expect("mint");
    assert_eq!(
        (grant.bundle_id.as_str(), grant.scope),
        (FIXTURE_BUNDLE, scope)
    );

    let mut agent = Conn::open(&server.agent);
    let held = agent.send(&json!({"op": "connect", "grant_id": grant.id}));
    assert_eq!(held["result"]["lease"], "held", "{held}");
    let mut second = Conn::open(&server.agent);
    let queued = second.send(&json!({"op": "connect", "grant_id": grant.id}));
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
}
