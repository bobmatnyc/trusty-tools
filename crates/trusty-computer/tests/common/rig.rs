//! A broker on the fake daemon, served on real sockets in a short temp dir
//! (port of the spike's `test_broker.py` `Rig`).
//!
//! Why: the broker tests must run every byte through the real sockets, peer
//! check and pipeline, without the real cua-driver (issue #11460).
//! What: [`Rig::new`] builds a [`Supervisor`] on [`FakeLauncher`] with a
//! fixture, a [`Broker`] on a manual clock and a [`FakeDialog`], binds
//! [`BrokerServer`], mints one grant through the consent flow on
//! `operator.sock` for this process's pid and window 1, and connects one
//! agent session that redeems its code and holds the lease.
//! Test: `tests/broker_spike.rs`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Value, json};
use trusty_computer::broker::{Broker, Clock, Inner};
use trusty_computer::broker_server::BrokerServer;
use trusty_computer::grants::Grant;
use trusty_computer::supervisor::Supervisor;

use super::dialog::{FakeDialog, FakeResolver};
use super::{FakeDaemon, FakeLauncher, FixtureState, short_tmp};

/// The bundle id every rig grant names.
pub const BUNDLE: &str = "dev.trusty.computer.fixture";

/// The fixture window every rig grant names.
pub const WINDOW: u64 = 1;
/// The rig's clock starts here (unix seconds).
pub const T0: f64 = 1_800_000_000.0;

/// One client connection.
pub struct Conn {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Conn {
    /// Connect to `socket`.
    pub fn open(socket: &Path) -> Conn {
        let writer = UnixStream::connect(socket).expect("connect broker");
        writer
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("timeout");
        let reader = BufReader::new(writer.try_clone().expect("clone"));
        Conn { writer, reader }
    }

    /// Send one raw line, read one reply line.
    pub fn send_raw(&mut self, line: &str) -> Value {
        writeln!(self.writer, "{line}").expect("send");
        let mut reply = String::new();
        self.reader.read_line(&mut reply).expect("read reply");
        serde_json::from_str(&reply).expect("reply json")
    }

    /// Send one request.
    pub fn send(&mut self, request: &Value) -> Value {
        self.send_raw(&request.to_string())
    }
}

/// The broker under test.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub broker: Arc<Broker<FakeLauncher>>,
    pub server: BrokerServer,
    pub millis: Arc<AtomicU64>,
    pub grant: Grant,
    pub conn: Conn,
    pub readback: PathBuf,
    pub dialog: Arc<FakeDialog>,
    pub resolver: Arc<FakeResolver>,
    codes: Mutex<HashMap<String, String>>,
}

/// A call request.
pub fn call(grant: &str, tool: &str, rid: &str, args: Value) -> Value {
    json!({"op": "call", "grant_id": grant, "tool": tool, "request_id": rid, "args": args})
}

/// This process's pid, the scope of every rig grant.
pub fn pid() -> u32 {
    std::process::id()
}

impl Rig {
    /// A rig with one granted, connected session (extra tools `type_text`, `set_value`).
    pub fn new() -> Rig {
        Rig::with(&["type_text", "set_value"], WINDOW)
    }

    /// A rig whose grant names `window` and adds `extra` tools.
    pub fn with(extra: &[&str], window: u64) -> Rig {
        let dir = short_tmp();
        let state = dir.path().join("s");
        let readback = dir.path().join("fixture.json");
        let fixture = FixtureState::new(&readback);
        fixture.flush();
        let mut launcher = FakeLauncher::new("0.23.2", None);
        launcher.fixture = Some(fixture);
        let sup = Supervisor::new(launcher, &state)
            .expect("supervisor")
            .with_ready_timeout(Duration::from_secs(2));
        let millis = Arc::new(AtomicU64::new((T0 * 1000.0) as u64));
        let m = Arc::clone(&millis);
        let clock: Clock = Arc::new(move || m.load(Ordering::SeqCst) as f64 / 1000.0);
        let dialog = FakeDialog::confirming();
        let resolver = Arc::new(FakeResolver::default());
        let broker = Broker::new(sup, &state, clock, dialog.clone()).expect("broker");
        let broker = broker
            .with_dialog_timeout(Duration::from_millis(500))
            .with_app_resolver(resolver.clone());
        let broker = Arc::new(broker);
        let mut server = BrokerServer::bind(&state).expect("bind");
        server.spawn(Arc::clone(&broker));
        let minted = consent(&server.operator, &dialog, &grant_req(extra, window));
        let id = minted["result"]["grant"]["id"].as_str().expect("grant id");
        let grant = broker.lock().grant(id).cloned().expect("grant");
        let mut conn = Conn::open(&server.agent);
        let code = &minted["result"]["code"];
        let reply = conn.send(&json!({"op": "connect", "code": code}));
        assert_eq!(reply["result"]["lease"], "held", "{reply}");
        Rig {
            dir,
            broker,
            server,
            millis,
            grant,
            conn,
            readback,
            dialog,
            resolver,
            codes: Mutex::new(HashMap::new()),
        }
    }

    /// The state dir.
    pub fn state(&self) -> PathBuf {
        self.dir.path().join("s")
    }

    /// The audit log path.
    pub fn audit_path(&self) -> PathBuf {
        self.state().join("audit.jsonl")
    }

    /// Every audit line.
    pub fn audit(&self) -> Vec<Value> {
        std::fs::read_to_string(self.audit_path())
            .expect("audit")
            .lines()
            .map(|l| serde_json::from_str(l).expect("audit json"))
            .collect()
    }

    /// A call on the rig's session with the rig's grant.
    pub fn call(&mut self, tool: &str, rid: &str, args: Value) -> Value {
        let req = call(&self.grant.id, tool, rid, args);
        self.conn.send(&req)
    }

    /// `get_window_state` of the fixture window; panics unless ok.
    pub fn snap(&mut self, rid: &str) -> Value {
        let reply = self.call(
            "get_window_state",
            rid,
            json!({"pid": pid(), "window_id": WINDOW}),
        );
        assert_eq!(reply["ok"], true, "{reply}");
        reply["result"].clone()
    }

    /// A click on `token` in the fixture window.
    pub fn click(&mut self, rid: &str, token: &str) -> Value {
        self.call(
            "click",
            rid,
            json!({"pid": pid(), "window_id": WINDOW, "element_token": token}),
        )
    }

    /// The broker state, locked.
    pub fn inner(&self) -> MutexGuard<'_, Inner<FakeLauncher>> {
        self.broker.lock()
    }

    /// Run `f` on the live fake daemon.
    pub fn daemon<R>(&self, f: impl FnOnce(&FakeDaemon) -> R) -> R {
        let inner = self.inner();
        f(inner
            .supervisor
            .launcher()
            .daemon
            .as_ref()
            .expect("daemon running"))
    }

    /// Tool names the live daemon received, without the supervisor's `start_session`.
    pub fn agent_calls(&self) -> Vec<String> {
        self.daemon(|d| {
            d.calls()
                .into_iter()
                .map(|(name, _)| name)
                .filter(|n| n != "start_session")
                .collect()
        })
    }

    /// The fixture readback.
    pub fn fixture(&self) -> Value {
        super::read_state(&self.readback)
    }

    /// Advance the broker clock.
    pub fn advance(&self, secs: f64) {
        self.millis
            .fetch_add((secs * 1000.0) as u64, Ordering::SeqCst);
    }

    /// One operator request on a fresh `operator.sock` connection.
    pub fn op(&self, request: &Value) -> Value {
        Conn::open(&self.server.operator).send(request)
    }

    /// Mint another grant for this pid and `window`; its code is kept for [`Rig::code`].
    pub fn mint(&self, window: u64) -> Grant {
        let minted = consent(
            &self.server.operator,
            &self.dialog,
            &grant_req(&["type_text"], window),
        );
        let id = minted["result"]["grant"]["id"]
            .as_str()
            .expect("grant id")
            .to_owned();
        let code = minted["result"]["code"].as_str().expect("code").to_owned();
        self.codes.lock().expect("codes").insert(id.clone(), code);
        self.broker.lock().grant(&id).cloned().expect("grant")
    }

    /// The one-use code of a grant from [`Rig::mint`].
    pub fn code(&self, grant: &str) -> String {
        self.codes
            .lock()
            .expect("codes")
            .remove(grant)
            .expect("a code for this grant")
    }
}

/// A `grant_request` for this pid and `window`, adding `extra` tools.
pub fn grant_req(extra: &[&str], window: u64) -> Value {
    json!({"op": "grant_request", "bundle_id": BUNDLE, "pid": pid(), "window_id": window,
        "tools": extra})
}

/// The whole consent flow on `operator`: request, read the fake dialog's code, confirm.
/// Answers the first refusal, or the `grant_confirm` reply.
pub fn consent(operator: &Path, dialog: &FakeDialog, request: &Value) -> Value {
    let mut conn = Conn::open(operator);
    let reply = conn.send(request);
    let Some(challenge) = reply["result"]["challenge"].as_str() else {
        return reply;
    };
    let code = dialog.last_code();
    conn.send(&json!({"op": "grant_confirm", "challenge": challenge, "code": code}))
}
