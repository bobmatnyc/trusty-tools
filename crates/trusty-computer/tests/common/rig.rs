//! A broker on the fake daemon, served on real sockets in a short temp dir
//! (port of the spike's `test_broker.py` `Rig`).
//!
//! Why: the broker tests must run every byte through the real sockets, peer
//! check and pipeline, without the real cua-driver (issue #11460).
//! What: [`Rig::new`] builds a [`Supervisor`] on [`FakeLauncher`] with a
//! fixture, a [`Broker`] on a manual clock, binds [`BrokerServer`], mints one
//! grant through the operator stub for this process's pid and window 1, and
//! connects one agent session that holds the lease.
//! Test: `tests/broker_spike.rs`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, MutexGuard};
use std::time::Duration;

use serde_json::{Value, json};
use trusty_computer::broker::{Broker, Clock, Inner};
use trusty_computer::broker_server::BrokerServer;
use trusty_computer::grants::{Grant, OperatorMint};
use trusty_computer::manifest::LeaseScope;
use trusty_computer::supervisor::Supervisor;

use super::{FakeDaemon, FakeLauncher, FixtureState, short_tmp};

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
        let broker = Arc::new(Broker::new(sup, &state, clock).expect("broker"));
        let mut server = BrokerServer::bind(&state).expect("bind");
        server.spawn(Arc::clone(&broker));
        let grant = broker.operator_mint(mint(extra, window)).expect("mint");
        let mut conn = Conn::open(&server.agent);
        let reply = conn.send(&json!({"op": "connect", "grant_id": grant.id}));
        assert_eq!(reply["result"]["lease"], "held", "{reply}");
        Rig {
            dir,
            broker,
            server,
            millis,
            grant,
            conn,
            readback,
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

    /// Mint another grant for this pid and `window`.
    pub fn mint(&self, window: u64) -> Grant {
        self.broker
            .operator_mint(mint(&["type_text"], window))
            .expect("mint")
    }
}

/// An operator stub mint for this pid and `window`, adding `extra` tools.
pub fn mint(extra: &[&str], window: u64) -> OperatorMint {
    let mut req = OperatorMint::stub(
        "dev.trusty.computer.fixture",
        LeaseScope {
            pid: pid(),
            window_id: window,
        },
    );
    req.extra_tools = extra.iter().map(|t| (*t).to_owned()).collect();
    req
}
