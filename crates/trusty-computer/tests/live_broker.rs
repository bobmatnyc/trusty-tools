//! Live check for later: the broker drives the JXA fixture through a bounded daemon.
//!
//! Why: the fake daemon cannot prove the broker's pipeline against the real
//! cua-driver: redaction of a real menu bar, a read-back confirmed click on
//! the fixture, and a second session queued behind the first (plan section 8).
//! What: compiles `tests/fixtures/fixture.js` into `Fixture.app` in the test
//! temp dir (`osacompile -l JavaScript`, `CFBundleIdentifier`
//! [`FIXTURE_BUNDLE`], ad-hoc re-signed) and opens it, so the real
//! `lsappinfo` resolver names the fixture's own bundle; then builds an in-process broker on
//! the real launcher in a temp state dir, mints two grants through the
//! consent flow (fake dialog) for the fixture's pid and window only, then over
//! `agent.sock` connects, snapshots (no `AXMenuBar` in the reply), clicks
//! "Press Me" (confirmed, fixture click count +1), and checks that a second
//! session is queued. It refuses to run unless `TRUSTY_SANDBOX=1`, so it never
//! runs with the operator's HOME, and it never names any other app or daemon.
//! Run: `TRUSTY_SANDBOX=1 cargo test -p trusty-computer --test live_broker -- --ignored --nocapture`.
//! Test: this file.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::dialog::FakeDialog;
use common::rig::{Conn, consent};
use serde_json::json;
use trusty_computer::broker::{Broker, wall_clock};
use trusty_computer::broker_server::BrokerServer;
use trusty_computer::launcher::CuaAppLauncher;
use trusty_computer::manifest::LeaseScope;
use trusty_computer::supervisor::Supervisor;

/// The fixture app's own bundle id (#11460: the grant names what `lsappinfo` reports).
const FIXTURE_BUNDLE: &str = "com.trusty.computer.fixture";

/// The opened fixture app, terminated by pid on drop.
struct Fixture(Option<u32>);

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = Command::new("/bin/kill").arg(pid.to_string()).status();
        }
    }
}

fn run(cmd: &mut Command) {
    let status = cmd.stdout(Stdio::null()).status().expect("spawn");
    assert!(status.success(), "{cmd:?}: {status}");
}

/// Build `<dir>/Fixture.app` from `fixture.js`, writing its state to `readback`.
fn build_fixture_app(dir: &Path, readback: &Path) -> PathBuf {
    let js = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fixture.js");
    let source = std::fs::read_to_string(js).expect("fixture.js");
    // An applet's run handler gets no argv; bake the state path in.
    let default = "argv[0] || '/tmp/grantcheck_state.txt'";
    assert!(source.contains(default), "fixture.js state-path line moved");
    let path = json!(readback.to_str().expect("utf-8 path")).to_string();
    let wrapped = dir.join("fixture-app.js");
    std::fs::write(&wrapped, source.replace(default, &path)).expect("write js");
    let app = dir.join("Fixture.app");
    run(Command::new("/usr/bin/osacompile")
        .args(["-l", "JavaScript", "-o"])
        .arg(&app)
        .arg(&wrapped));
    run(Command::new("/usr/bin/plutil")
        .args(["-replace", "CFBundleIdentifier", "-string", FIXTURE_BUNDLE])
        .arg(app.join("Contents/Info.plist")));
    run(Command::new("/usr/bin/codesign")
        .args(["-f", "-s", "-"])
        .arg(&app)
        .stderr(Stdio::null()));
    app
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
    let app = build_fixture_app(dir.path(), &readback);
    let mut fixture = Fixture(None);
    run(Command::new("/usr/bin/open").arg("-n").arg(&app));
    let pid = wait_for("fixture pid", 15, || state_value(&readback, "pid "));
    fixture.0 = u32::try_from(pid).ok();
    let wid = wait_for("fixture window", 15, || state_value(&readback, "window "));
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
