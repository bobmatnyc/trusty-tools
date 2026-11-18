//! `secrets.remote.*` and `remote-gate` over real sockets (#11464).
//!
//! A child of `wire_tests`, so it shares that module's fixture: every path
//! is under a `TempDir` and the backends are in memory. The socket peer of
//! every call is this test process; a test makes it the verified gate by
//! giving it a `trusty-secrets` executable in the fake process table.
//! The fixture's machine config is also the account config the server
//! re-checks, so each test writes its `remote_hosts` there.
//!
//! Test: itself.

use std::io::Cursor;
use std::os::unix::net::UnixListener;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use super::*;
use crate::server::audit::RemoteAgent;
use crate::server::methods::Caller;
use crate::server::remote;
use crate::server::remote_gate::{
    EXIT_REFUSED, EXIT_UNAVAILABLE, GATE_DEADLINE, GateConfig, gate_reply, run_remote_gate,
};

/// An executable path whose file name is the gate's.
const GATE_EXE: &str = "/opt/trusty/bin/trusty-secrets";
/// The host every test's account config lists.
const HOST: &str = "evo-linux";
/// The fixture's project, as a remote frame names it.
const PROJECT: &str = "acme/web";

/// Write the fixture's account config: `keychain`, plus `hosts`.
fn account(fx: &Fixture, hosts: &str) {
    let yaml = format!("secrets:\n  default_backend: keychain\n  remote_hosts:\n{hosts}");
    std::fs::write(&fx.settings.machine_config, yaml).unwrap();
}

/// `HOST` listing `PROJECT`.
fn listed(fx: &Fixture) {
    account(
        fx,
        &format!("    {HOST}:\n      projects: [\"{PROJECT}\"]\n"),
    );
}

/// An executable path whose file name is sshd's.
const SSHD_EXE: &str = "/usr/sbin/sshd";

/// `procs_with_me`, with this process running the gate's executable below
/// an sshd at its parent 10.
fn gate_procs() -> Arc<FakeProcs> {
    let procs = gate_procs_without_sshd();
    procs.set_executable(10, SSHD_EXE);
    procs
}

/// `procs_with_me` running the gate's executable, with no sshd above it.
fn gate_procs_without_sshd() -> Arc<FakeProcs> {
    let procs = procs_with_me();
    procs.set_executable(me(), GATE_EXE);
    procs
}

fn remote_params(name: &str) -> Value {
    json!({"host": HOST, "project": PROJECT, "key": name, "agent": "session-7"})
}

/// A started server over `procs`, `OPEN_KEY` open and `API_KEY` denied.
async fn serve(fx: &Fixture, procs: Arc<FakeProcs>) -> Running {
    seed(fx, "OPEN_KEY", OPEN_VALUE, true);
    seed(fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(fx, procs);
    fx.start_state(state).await
}

/// Records of `method` in the fixture's audit log.
fn remote_records(fx: &Fixture, method: AuditMethod) -> Vec<AuditRecord> {
    records(fx)
        .into_iter()
        .filter(|r| r.method == method)
        .collect()
}

/// A gate config dialling the fixture's socket with its account config.
fn gate_config(fx: &Fixture) -> GateConfig {
    GateConfig {
        host: HOST.to_owned(),
        socket: fx.settings.socket.clone(),
        machine_config: fx.settings.machine_config.clone(),
        deadline: GATE_DEADLINE,
    }
}

/// Run the gate on `frame`; its exit status and its one reply line.
async fn gate(config: &GateConfig, frame: &str) -> (u8, Value) {
    let mut out = Vec::new();
    let status = run_remote_gate(config, Cursor::new(format!("{frame}\n")), &mut out).await;
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    (status, serde_json::from_str(&text).unwrap())
}

/// A socket that counts connections and never answers.
struct Silent {
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

fn silent_socket(path: &Path) -> Silent {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // The client's hardened dial wants a 0700 directory and a 0600 socket.
    set_mode(path.parent().unwrap(), 0o700);
    let listener = UnixListener::bind(path).unwrap();
    set_mode(path, 0o600);
    let accepted = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (count, done) = (Arc::clone(&accepted), Arc::clone(&stop));
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            if done.load(Ordering::SeqCst) {
                break;
            }
            count.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    Silent { accepted, stop }
}

impl Drop for Silent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Why: AC5, AC6 — through the verified gate an open key resolves, and its
/// write-ahead allow record carries `host`, the label marked unverified,
/// the project, `agent_parent: true` and `default_policy: true`.
/// Red when `host` or `remote_agent` is not recorded.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_resolve_of_an_open_key_returns_it_with_a_remote_record() {
    let fx = fixture();
    listed(&fx);
    let server = serve(&fx, gate_procs()).await;
    let response = call(
        &fx.settings.socket,
        method::REMOTE_RESOLVE,
        remote_params("OPEN_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(ok(response)["value"], OPEN_VALUE);
    let rows = remote_records(&fx, AuditMethod::RemoteResolve);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row.decision, AuditDecision::Allow);
    assert_eq!(row.host.as_deref(), Some(HOST));
    assert_eq!(row.remote_agent, Some(RemoteAgent::unverified("session-7")));
    assert_eq!(row.remote_agent.as_ref().map(|a| a.verified), Some(false));
    assert_eq!(row.remote_project.as_deref(), Some(PROJECT));
    assert_eq!(row.agent_parent, Some(true));
    assert_eq!(row.default_policy, Some(true));
    assert_eq!(row.key.as_ref().map(SecretKey::as_str), Some("OPEN_KEY"));
}

/// Why: AC5 — a key the project denies to agents is refused through the
/// gate as it is to a local agent: no value, an allow then a deny record.
/// Red when the remote path resolves with `agent_parent` false.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_resolve_of_a_denied_key_is_refused() {
    let fx = fixture();
    listed(&fx);
    let server = serve(&fx, gate_procs()).await;
    let response = call(
        &fx.settings.socket,
        method::REMOTE_RESOLVE,
        remote_params("API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::REMOTE_RESOLVE),
        ErrorKind::AgentKeyDenied
    );
    assert_no_value(&response);
    let rows = remote_records(&fx, AuditMethod::RemoteResolve);
    let decisions: Vec<_> = rows.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [AuditDecision::Allow, AuditDecision::Deny]);
    assert!(rows.iter().all(|r| r.host.as_deref() == Some(HOST)));
}

/// Why: Fail-Open Check — a peer whose executable is not the gate's is
/// refused before the allowlist, and its record names no host, since the
/// host is trusted only from the verified gate.
/// Red when `verify_gate` is skipped.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_resolve_from_a_non_gate_peer_is_refused() {
    let fx = fixture();
    listed(&fx);
    let procs = procs_with_me();
    procs.set_executable(me(), "/usr/bin/python3");
    let server = serve(&fx, procs).await;
    let response = call(
        &fx.settings.socket,
        method::REMOTE_RESOLVE,
        remote_params("OPEN_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::REMOTE_RESOLVE),
        ErrorKind::RemoteRefused
    );
    assert!(!wire(&response).contains(OPEN_VALUE));
    let rows = remote_records(&fx, AuditMethod::RemoteResolve);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].decision, AuditDecision::Deny);
    assert_eq!(rows[0].host, None);
}

/// Why: Fail-Open Check — a gate with Claude Code above it is a local
/// agent, which must use a grant; an unreadable chain refuses too.
/// Red when an agent ancestor or a read error passes `verify_gate`.
/// Test: itself.
#[test]
fn remote_resolve_from_a_gate_under_an_agent_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let agent = gate_procs();
    agent.set_agent(10);
    let unreadable = gate_procs();
    unreadable.set_unreadable();
    for procs in [agent, unreadable] {
        let (state, _grants) = state_with(&fx, procs);
        let refused = remote::resolve(&state, Caller::new(Some(me())), remote_params("OPEN_KEY"));
        assert_eq!(refused, Err(ErrorKind::RemoteRefused));
    }
}

/// Why: #11464 critic M1 — a detached local process running the gate's
/// executable has no Claude Code ancestor but never came in over ssh; with
/// no sshd above it, or an sshd not owned by root, it is refused.
/// Red when `verify_gate` does not require a root-owned sshd ancestor.
/// Test: itself.
#[test]
fn remote_resolve_from_a_gate_with_no_sshd_ancestor_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let (state, _grants) = state_with(&fx, gate_procs_without_sshd());
    let refused = remote::resolve(&state, Caller::new(Some(me())), remote_params("OPEN_KEY"));
    assert_eq!(refused, Err(ErrorKind::RemoteRefused));
    let renamed = gate_procs_without_sshd();
    renamed.set_executable(10, "/tmp/evil/sshd");
    let (mut state, _grants) = state_with(&fx, renamed);
    state.exe_owner = |_| Some(501);
    let refused = remote::resolve(&state, Caller::new(Some(me())), remote_params("OPEN_KEY"));
    assert_eq!(refused, Err(ErrorKind::RemoteRefused));
    let (state, _grants) = state_with(&fx, gate_procs());
    let allowed = remote::resolve(&state, Caller::new(Some(me())), remote_params("OPEN_KEY"));
    assert!(allowed.is_ok(), "{allowed:?}");
}

/// Why: #11464 critic M2 — the preflight writes its record before it opens
/// the backend, so an unwritable log opens nothing.
/// Red when `state.open` runs before the write-ahead record.
/// Test: itself.
#[test]
fn remote_doctor_with_unwritable_audit_never_opens_the_backend() {
    static OPENS: AtomicUsize = AtomicUsize::new(0);
    let fx = fixture();
    listed(&fx);
    let base = fx.backends();
    let counting: BackendFactory = Arc::new(move |id: &BackendId| {
        OPENS.fetch_add(1, Ordering::SeqCst);
        base(id)
    });
    let mut state = fx.state(counting);
    state.grants = Arc::new(GrantRegistry::new(
        gate_procs(),
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    let params = json!({"host": HOST, "project": PROJECT});
    // Create the log's directory, then lock it.
    remote::doctor(&state, Caller::new(Some(me())), params.clone()).unwrap();
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    set_mode(&dir, 0o755);
    let restore = RestoreMode(dir);
    let before = OPENS.load(Ordering::SeqCst);
    let refused = remote::doctor(&state, Caller::new(Some(me())), params);
    let opens = OPENS.load(Ordering::SeqCst) - before;
    drop(restore);
    assert_eq!(refused, Err(ErrorKind::AuditUnavailable));
    assert_eq!(opens, 0, "the backend was opened with no audit record");
}

/// Why: #11464 critic L1 — the allowlist compares the canonical names the
/// scopes are built from, so the frame's spelling cannot reach the scopes
/// or the record, and an entry that does not parse matches nothing.
/// Test: itself.
#[test]
fn remote_project_spelling_never_reaches_the_scopes() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    account(
        &fx,
        &format!("    {HOST}:\n      projects: [\"ACME/Web\"]\n"),
    );
    let (state, _grants) = state_with(&fx, gate_procs());
    let params = json!({"host": HOST, "project": "acme/WEB"});
    let answer = remote::doctor(&state, Caller::new(Some(me())), params).unwrap();
    assert_eq!(answer["project_vault"], PROJECT_VAULT);
    let rows = remote_records(&fx, AuditMethod::RemoteDoctor);
    assert_eq!(rows[0].remote_project.as_deref(), Some(PROJECT));
    account(
        &fx,
        &format!("    {HOST}:\n      projects: [\"acme/web/x\"]\n"),
    );
    let (state, _grants) = state_with(&fx, gate_procs());
    let refused = remote::doctor(
        &state,
        Caller::new(Some(me())),
        json!({"host": HOST, "project": PROJECT}),
    );
    assert_eq!(refused, Err(ErrorKind::RemoteRefused));
}

/// Why: Fail-Open Check — with no peer pid there is no gate to verify.
/// Red when a missing pid skips the check.
/// Test: itself.
#[test]
fn remote_resolve_with_no_peer_pid_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let (state, _grants) = state_with(&fx, gate_procs());
    let refused = remote::resolve(&state, Caller::new(None), remote_params("OPEN_KEY"));
    assert_eq!(refused, Err(ErrorKind::RemoteRefused));
}

/// Why: AC2 on the server — the allowlist is re-checked in the account's
/// file, so a client that skips the gate's pre-check gains nothing: an
/// unknown host, an unlisted project and a disabled host are refused.
/// Red when the server trusts the gate's pre-check.
/// Test: itself.
#[test]
fn remote_resolve_of_an_unlisted_project_is_refused_by_the_server() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let cases = [
        format!("    other-host:\n      projects: [\"{PROJECT}\"]\n"),
        format!("    {HOST}:\n      projects: [\"acme/other\"]\n"),
        format!("    {HOST}:\n      enabled: false\n      projects: [\"{PROJECT}\"]\n"),
    ];
    for hosts in cases {
        account(&fx, &hosts);
        let (state, _grants) = state_with(&fx, gate_procs());
        let refused = remote::resolve(&state, Caller::new(Some(me())), remote_params("OPEN_KEY"));
        assert_eq!(refused, Err(ErrorKind::RemoteRefused), "{hosts}");
    }
}

/// Why: Fail-Open Check — no account config, or one that does not parse,
/// is no allowlist, never an open one.
/// Red when a missing or corrupt file reads as "allowed".
/// Test: itself.
#[test]
fn remote_resolve_with_an_unreadable_account_config_is_refused() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    std::fs::write(&fx.settings.machine_config, "secrets: [not, a, map\n").unwrap();
    let (state, _grants) = state_with(&fx, gate_procs());
    let corrupt = remote::resolve(&state, Caller::new(Some(me())), remote_params("OPEN_KEY"));
    assert_eq!(corrupt, Err(ErrorKind::RemoteRefused));
    let (mut state, _grants) = state_with(&fx, gate_procs());
    state.file_consent_config = None;
    let absent = remote::resolve(&state, Caller::new(Some(me())), remote_params("OPEN_KEY"));
    assert_eq!(absent, Err(ErrorKind::RemoteRefused));
}

/// Why: AC6, audit fail-closed — an unwritable log refuses the remote
/// resolve before the backend is read.
/// Red when the read runs before the write-ahead record.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_resolve_with_unwritable_audit_never_reads_the_backend() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "API_KEY", SENTINEL_VALUE, true);
    let (mut state, racing, _grants) = racing_state(&fx);
    state.grants = Arc::new(GrantRegistry::new(
        gate_procs(),
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    let server = fx.start_state(state).await;
    // Open the log once, so its directory exists to lock.
    let warm = call(
        &fx.settings.socket,
        method::REMOTE_DOCTOR,
        json!({"host": HOST, "project": PROJECT}),
    )
    .await;
    ok(warm);
    let before = racing.reads.load(Ordering::SeqCst);
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    set_mode(&dir, 0o755);
    let restore = RestoreMode(dir);
    let refused = call(
        &fx.settings.socket,
        method::REMOTE_RESOLVE,
        remote_params("API_KEY"),
    )
    .await;
    let reads = racing.reads.load(Ordering::SeqCst) - before;
    drop(restore);
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::REMOTE_RESOLVE),
        ErrorKind::AuditUnavailable
    );
    assert_no_value(&refused);
    assert_eq!(reads, 0, "the backend was read with no audit record");
}

/// Why: the preflight names the backend and the project vault, reads no
/// value, and leaves one record with the host.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_doctor_names_the_backend_and_leaves_one_record() {
    let fx = fixture();
    listed(&fx);
    let server = serve(&fx, gate_procs()).await;
    let response = call(
        &fx.settings.socket,
        method::REMOTE_DOCTOR,
        json!({"host": HOST, "project": PROJECT}),
    )
    .await;
    server.stop().await;

    let answer = ok(response);
    assert_eq!(answer["backend"], "keychain");
    assert_eq!(answer["project_vault"], PROJECT_VAULT);
    let rows = remote_records(&fx, AuditMethod::RemoteDoctor);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].host.as_deref(), Some(HOST));
}

/// Why: AC1 — a verb other than `resolve` or `doctor` exits 13 and sends
/// nothing; AC2 — an unlisted project or unknown host exits 13 before any
/// relay. The socket counts every connection.
/// Red when the gate relays first, or checks verbs by a deny list.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_gate_refuses_an_unlisted_verb_and_sends_nothing() {
    let fx = fixture();
    listed(&fx);
    let silent = silent_socket(&fx.settings.socket);
    let config = gate_config(&fx);
    let frames = [
        (
            r#"{"method":"set","project":"acme/web","key":"K"}"#,
            "method_refused",
        ),
        (
            r#"{"method":"grant","project":"acme/web","key":"K"}"#,
            "method_refused",
        ),
        (
            r#"{"method":"secrets.remote.resolve","project":"acme/web","key":"K"}"#,
            "method_refused",
        ),
        (
            r#"{"method":"resolve","project":"acme/other","key":"K"}"#,
            "remote_refused",
        ),
        (
            r#"{"method":"resolve","project":"acme/web","key":"K","host":"x"}"#,
            "invalid_frame",
        ),
        ("not json", "invalid_frame"),
    ];
    for (frame, kind) in frames {
        let (status, line) = gate(&config, frame).await;
        assert_eq!(status, EXIT_REFUSED, "{frame}");
        assert_eq!(line, json!({"ok": false, "error": kind}), "{frame}");
    }
    let unknown = GateConfig {
        host: "other-host".to_owned(),
        ..config
    };
    let (status, _) = gate(
        &unknown,
        r#"{"method":"resolve","project":"acme/web","key":"K"}"#,
    )
    .await;
    assert_eq!(status, EXIT_REFUSED);
    assert_eq!(
        silent.accepted.load(Ordering::SeqCst),
        0,
        "a refused frame was relayed"
    );
}

/// Why: AC3 — with no server socket the gate exits 69 with a fixed error
/// and creates nothing where a spawned server would bind.
/// Red when the gate spawns or waits for a server.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_gate_with_no_server_exits_unavailable() {
    let fx = fixture();
    listed(&fx);
    let (status, line) = gate(
        &gate_config(&fx),
        r#"{"method":"resolve","project":"acme/web","key":"K"}"#,
    )
    .await;
    assert_eq!(status, EXIT_UNAVAILABLE);
    assert_eq!(line, json!({"ok": false, "error": "server_unavailable"}));
    assert!(!fx.settings.socket.parent().unwrap().exists());
}

/// Why: AC4 — a server that never answers, or a frame that never ends,
/// answers 69 at the deadline. The server's own remote deadline plus its
/// body grace stays inside the gate's 10 s, so a reply is never cut off.
/// Red when the relay or the frame read has no bound.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_gate_with_a_silent_server_answers_within_the_deadline() {
    use crate::server::deadline::{BODY_GRACE, REMOTE_DEADLINE};
    assert_eq!(GATE_DEADLINE, Duration::from_secs(10));
    assert!(REMOTE_DEADLINE + BODY_GRACE < GATE_DEADLINE);
    let fx = fixture();
    listed(&fx);
    let silent = silent_socket(&fx.settings.socket);
    let config = GateConfig {
        deadline: Duration::from_millis(400),
        ..gate_config(&fx)
    };
    let started = Instant::now();
    let (status, line) = gate(
        &config,
        r#"{"method":"resolve","project":"acme/web","key":"K"}"#,
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(status, EXIT_UNAVAILABLE);
    assert_eq!(line, json!({"ok": false, "error": "deadline_exceeded"}));
    assert_eq!(silent.accepted.load(Ordering::SeqCst), 1);

    // A reader that never returns a byte.
    struct Stalled;
    impl std::io::Read for Stalled {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_secs(30));
            Ok(0)
        }
    }
    let started = Instant::now();
    let reply = gate_reply(&config, Stalled).await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(reply.status, EXIT_UNAVAILABLE);
}

/// Why: end to end — the gate relays to the running server: an open key
/// resolves (exit 0, value on stdout only), a denied key exits 13, and no
/// value reaches the logs or the audit file.
/// Red when the gate drops `agent_parent` or relays the frame's host.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_gate_relays_to_the_running_server() {
    capture_logs();
    let fx = fixture();
    listed(&fx);
    let server = serve(&fx, gate_procs()).await;
    let config = gate_config(&fx);
    let (open, open_line) = gate(
        &config,
        r#"{"method":"resolve","project":"Acme/Web","key":"OPEN_KEY","agent":"s1"}"#,
    )
    .await;
    let (denied, denied_line) = gate(
        &config,
        r#"{"method":"resolve","project":"acme/web","key":"API_KEY"}"#,
    )
    .await;
    let (doctor, _) = gate(&config, r#"{"method":"doctor","project":"acme/web"}"#).await;
    server.stop().await;

    assert_eq!(open, 0);
    assert_eq!(open_line["result"]["value"], OPEN_VALUE);
    assert_eq!(denied, EXIT_REFUSED);
    assert_eq!(
        denied_line,
        json!({"ok": false, "error": "agent_key_denied"})
    );
    assert_eq!(doctor, 0);
    let audit = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!audit.contains(SENTINEL_VALUE) && !audit.contains(OPEN_VALUE));
    let logs = captured();
    assert!(!logs.contains(SENTINEL_VALUE) && !logs.contains(OPEN_VALUE));
    let rows = remote_records(&fx, AuditMethod::RemoteResolve);
    assert!(
        rows.iter().all(|r| r.host.as_deref() == Some(HOST)),
        "{rows:?}"
    );
}

/// Why: a server refusal of policy exits 13 and one of availability 69,
/// an older server without the method (no kind) is 69, and only a result
/// is exit 0; every failure line is the fixed kind alone.
/// Test: itself.
#[test]
fn remote_gate_answer_table() {
    use crate::server::remote_gate::answer;
    let failing = |kind: ErrorKind| RpcResponse {
        jsonrpc: "2.0".to_owned(),
        id: json!(1),
        result: None,
        error: Some(kind.to_rpc(method::REMOTE_RESOLVE)),
    };
    for (kind, status) in [
        (ErrorKind::AgentKeyDenied, EXIT_REFUSED),
        (ErrorKind::RemoteRefused, EXIT_REFUSED),
        (ErrorKind::NotFound, EXIT_REFUSED),
        (ErrorKind::BackendTimeout, EXIT_UNAVAILABLE),
        (ErrorKind::BackendLocked, EXIT_UNAVAILABLE),
        (ErrorKind::AuditUnavailable, EXIT_UNAVAILABLE),
        (ErrorKind::DeadlineExceeded, EXIT_UNAVAILABLE),
    ] {
        let reply = answer(failing(kind));
        assert_eq!(reply.status, status, "{kind:?}");
        assert_eq!(reply.line, json!({"ok": false, "error": kind.as_str()}));
    }
    let mut unknown = failing(ErrorKind::RemoteRefused);
    unknown.error.as_mut().unwrap().data = None;
    assert_eq!(answer(unknown).status, EXIT_UNAVAILABLE);
    let success = RpcResponse {
        jsonrpc: "2.0".to_owned(),
        id: json!(1),
        result: Some(json!({"key": "K"})),
        error: None,
    };
    assert_eq!(answer(success).status, 0);
}

/// Why: host names and labels reach the audit log, which holds no free
/// text; a project is exactly `<owner>/<repo>`.
/// Test: itself.
#[test]
fn remote_label_validation_table() {
    for good in ["evo-linux", "s1", "a.b_c-9", &"x".repeat(64)] {
        assert!(remote::valid_label(good), "{good}");
    }
    for bad in ["", "-x", ".x", "a b", "a/b", "a\nb", &"x".repeat(65), "ä"] {
        assert!(!remote::valid_label(bad), "{bad}");
    }
    assert!(remote::parse_project("acme/web").is_some());
    for bad in ["acme", "acme/web/x", "/web", "acme/", "a b/web"] {
        assert!(remote::parse_project(bad).is_none(), "{bad}");
    }
}

/// Why: the allowlist is exact: host and project compare without case, a
/// disabled host or an absent entry allows nothing.
/// Test: itself.
#[test]
fn remote_allowlist_table() {
    let machine: crate::store::config::MachineSecretsConfig = serde_yaml::from_str(
        "remote_hosts:\n  evo-linux:\n    projects: [Acme/Web]\n  off:\n    enabled: false\n    projects: [acme/web]\n",
    )
    .unwrap();
    let (owner, repo) = remote::parse_project("acme/web").unwrap();
    assert!(remote::host_allows(&machine, "EVO-linux", &owner, &repo));
    assert!(!remote::host_allows(&machine, "off", &owner, &repo));
    assert!(!remote::host_allows(&machine, "nobody", &owner, &repo));
    let (owner, repo) = remote::parse_project("acme/other").unwrap();
    assert!(!remote::host_allows(&machine, "evo-linux", &owner, &repo));
}

/// Why: the command line pins `--host`; paths default under the account
/// home, and a bad line is refused.
/// Test: itself.
#[test]
fn remote_gate_args_table() {
    let args = |list: &[&str]| {
        list.iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
    };
    let home = Some(PathBuf::from("/home/a"));
    let config =
        GateConfig::from_args(args(&["remote-gate", "--host", HOST]), home.clone()).unwrap();
    assert_eq!(config.host, HOST);
    assert_eq!(
        config.socket,
        PathBuf::from("/home/a").join(crate::server::SOCKET_SUBPATH)
    );
    assert_eq!(config.deadline, GATE_DEADLINE);
    for bad in [
        &["remote-gate"][..],
        &["remote-gate", "--host"],
        &["remote-gate", "--host", "a b"],
        &["remote-gate", "--host", HOST, "--other", "x"],
        &["serve", "--host", HOST],
    ] {
        assert!(
            GateConfig::from_args(args(bad), home.clone()).is_err(),
            "{bad:?}"
        );
    }
    assert!(GateConfig::from_args(args(&["remote-gate", "--host", HOST]), None).is_err());
}
