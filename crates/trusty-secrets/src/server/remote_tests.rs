//! `secrets.gate` and `remote-gate` over real sockets (#11464, D3a).
//!
//! A child of `wire_tests`, so it shares that module's fixture: every path
//! is under a `TempDir` and the backends are in memory. The socket peer of
//! every call is this test process; a test makes it the verified gate by
//! giving it a `trusty-secrets` executable below a root `sshd-session`
//! (its parent 10) in the fake process table. The fixture's machine config
//! is also the account config the server re-checks, so each test writes
//! its `remote_hosts` there.
//!
//! Test: itself.

use std::io::Cursor;
use std::os::unix::net::UnixListener;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use super::*;
use crate::server::audit::RemoteAgent;
use crate::server::methods::Caller;
use crate::server::remote_gate::{
    EXIT_REFUSED, EXIT_UNAVAILABLE, GATE_DEADLINE, GateConfig, SERVER_ABSENT, gate_reply,
    run_remote_gate,
};
use crate::server::{exec, remote};

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

/// One host entry mapping `project` to `dir` with `keys`.
fn host_entry(host: &str, project: &str, dir: &str, keys: &str) -> String {
    format!("    {host}:\n      projects:\n        \"{project}\": \"{dir}\"\n{keys}")
}

/// The key list both seeded keys are on.
const BOTH_KEYS: &str = "      keys: [OPEN_KEY, API_KEY]\n";

/// `HOST` mapping `PROJECT` to the fixture's checkout, both keys listed.
fn listed(fx: &Fixture) {
    account(fx, &host_entry(HOST, PROJECT, &fx.project(), BOTH_KEYS));
}

/// `procs_with_me`, this process running the gate's executable below a
/// root `sshd-session` at its parent 10.
fn gate_procs() -> Arc<FakeProcs> {
    let procs = gate_procs_without_sshd();
    procs.set_comm(10, "sshd-session", 0);
    procs
}

/// `procs_with_me` running the gate's executable below a user shell.
fn gate_procs_without_sshd() -> Arc<FakeProcs> {
    let procs = procs_with_me();
    procs.set_executable(me(), GATE_EXE);
    procs.set_comm(10, "zsh", 501);
    procs
}

/// `secrets.gate` `resolve` params for `name` under `token`.
fn gate_params(token: &str, name: &str) -> Value {
    json!({"verb": "resolve", "token": token, "key": name, "host": HOST,
        "project": PROJECT, "agent": "session-7"})
}

/// A grant of `keys` to this process, minted straight on `state`.
fn token_on(fx: &Fixture, state: &State, keys: &[&str]) -> String {
    let answer = exec::grant(state, Caller::new(Some(me())), grant_params(fx, me(), keys));
    answer.unwrap()["token"].as_str().unwrap().to_owned()
}

/// `secrets.gate` from this process, straight on `state`.
fn gate_call(state: &State, params: Value) -> Result<Value, ErrorKind> {
    remote::gate(state, Caller::new(Some(me())), params)
}

/// A started server over `procs`, `OPEN_KEY` open and `API_KEY` denied.
async fn serve(fx: &Fixture, procs: Arc<FakeProcs>) -> Running {
    seed(fx, "OPEN_KEY", OPEN_VALUE, true);
    seed(fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(fx, procs);
    fx.start_state(state).await
}

/// `secrets.resolve` records in the fixture's audit log.
fn resolve_records(fx: &Fixture) -> Vec<AuditRecord> {
    records(fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Resolve)
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

/// Run the gate on `input` as stdin; its exit status and its reply line.
async fn gate_raw(config: &GateConfig, input: String) -> (u8, Value) {
    let mut out = Vec::new();
    let status = run_remote_gate(config, Cursor::new(input), &mut out).await;
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    (status, serde_json::from_str(&text).unwrap())
}

/// Run the gate on one `frame` line.
async fn gate(config: &GateConfig, frame: &str) -> (u8, Value) {
    gate_raw(config, format!("{frame}\n")).await
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

/// Why: AC5, AC6 — through the verified gate, with a grant token, an open
/// key resolves, and its write-ahead allow record carries `host`, the label
/// marked unverified, the project, `agent_parent: true` and
/// `default_policy: true`.
/// Red when `host` or `remote_agent` is not recorded.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_resolve_of_an_open_key_returns_it_with_a_remote_record() {
    let fx = fixture();
    listed(&fx);
    let server = serve(&fx, gate_procs()).await;
    let token = granted(&fx, me(), &["OPEN_KEY"]).await;
    let response = call(
        &fx.settings.socket,
        method::GATE,
        gate_params(&token, "OPEN_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(ok(response)["value"], OPEN_VALUE);
    let rows: Vec<_> = resolve_records(&fx)
        .into_iter()
        .filter(|r| r.host.is_some())
        .collect();
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

/// Why: design S1 (a), critic M-C (iii) — the gate peer has no agent
/// ancestor, so plain `secrets.resolve` hands it a denied key; through
/// `secrets.gate` the forced `agent_parent` refuses it: no value, an allow
/// then a deny record, both naming the host.
/// Red when the gate entry does not force `agent_parent`.
/// Test: itself.
#[test]
fn gate_resolve_of_a_denied_key_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let procs = gate_procs();
    assert_eq!(
        crate::ancestry::has_agent_ancestor(procs.as_ref(), me()),
        Ok(false)
    );
    let (state, _grants) = state_with(&fx, procs);
    // Control: the same peer and grant through the local path reads it.
    let plain_token = token_on(&fx, &state, &["API_KEY"]);
    let plain = exec::resolve(
        &state,
        Caller::new(Some(me())),
        resolve_params(&plain_token, "API_KEY"),
    );
    assert_eq!(plain.unwrap()["value"], SENTINEL_VALUE);
    let token = token_on(&fx, &state, &["API_KEY"]);
    let refused = gate_call(&state, gate_params(&token, "API_KEY"));
    assert_eq!(refused, Err(ErrorKind::AgentKeyDenied));
    let rows: Vec<_> = resolve_records(&fx)
        .into_iter()
        .filter(|r| r.host.is_some())
        .collect();
    let decisions: Vec<_> = rows.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [AuditDecision::Allow, AuditDecision::Deny]);
    assert!(rows.iter().all(|r| r.agent_parent == Some(true)));
}

/// Why: design S1 (b), Fail-Open Check — a peer running another
/// executable, or the gate's executable under Claude Code, is refused with
/// `GateRefused` before anything decodes; it leaves one deny record
/// (critic LOW, as `bridged`) naming the key and no host.
/// Red when `verify_gate` is skipped or accepts an agent ancestor.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_from_a_non_gate_peer_is_refused() {
    let fx = fixture();
    listed(&fx);
    let other = gate_procs();
    other.set_executable(me(), "/usr/bin/python3");
    let server = serve(&fx, other).await;
    let token = granted(&fx, me(), &["OPEN_KEY"]).await;
    let response = call(
        &fx.settings.socket,
        method::GATE,
        gate_params(&token, "OPEN_KEY"),
    )
    .await;
    server.stop().await;
    assert_eq!(fixed_error(&response, method::GATE), ErrorKind::GateRefused);
    assert_no_value(&response);
    let rows = resolve_records(&fx);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        shape(&rows[0]),
        (
            AuditMethod::Resolve,
            AuditDecision::Deny,
            Some("gate_refused"),
            Some("OPEN_KEY")
        )
    );
    assert_eq!(rows[0].host, None);

    let agent = gate_procs();
    agent.set_agent(10);
    let (state, _grants) = state_with(&fx, agent);
    let refused = gate_call(&state, gate_params("t", "OPEN_KEY"));
    assert_eq!(refused, Err(ErrorKind::GateRefused));
}

/// Why: critic M-C (i), M1 — a detached local process running the gate's
/// executable has no Claude Code ancestor but never came in over ssh: with
/// no sshd above it, or an sshd not owned by root, it is refused. Both
/// `sshd` and OpenSSH 9.8+'s `sshd-session` pass (M-B).
/// Red when `verify_gate` does not require a root sshd ancestor.
/// Test: itself.
#[test]
fn gate_from_a_peer_with_no_sshd_ancestor_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let (state, _grants) = state_with(&fx, gate_procs_without_sshd());
    let refused = gate_call(&state, gate_params("t", "OPEN_KEY"));
    assert_eq!(refused, Err(ErrorKind::GateRefused));
    let user_sshd = gate_procs_without_sshd();
    user_sshd.set_comm(10, "sshd", 501);
    let (state, _grants) = state_with(&fx, user_sshd);
    let refused = gate_call(&state, gate_params("t", "OPEN_KEY"));
    assert_eq!(refused, Err(ErrorKind::GateRefused));
    for name in ["sshd", "sshd-session"] {
        let procs = gate_procs();
        procs.set_comm(10, name, 0);
        let (state, _grants) = state_with(&fx, procs);
        let token = token_on(&fx, &state, &["OPEN_KEY"]);
        let allowed = gate_call(&state, gate_params(&token, "OPEN_KEY"));
        assert_eq!(allowed.unwrap()["value"], OPEN_VALUE, "{name}");
    }
}

/// Why: critic M-C (ii) — an unreadable table, an ancestor whose short name
/// cannot be read, an agent walk that reaches an unreadable pid above a
/// valid sshd, and a parent loop (`ChainTooDeep`) each refuse; no read
/// failure falls back to the local-caller path.
/// Red when an unreadable step answers "verified".
/// Test: itself.
#[test]
fn gate_with_unreadable_ancestry_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let unreadable = gate_procs();
    unreadable.set_unreadable();
    let no_comm = procs_with_me();
    no_comm.set_executable(me(), GATE_EXE);
    // The sshd check passes; only the agent walk reaches the unreadable 77.
    let agent_walk = gate_procs();
    agent_walk.add(10, 77, 100);
    let looped = gate_procs();
    looped.add(me(), 60, 4242).add(60, 61, 1).add(61, 60, 1);
    looped.set_comm(60, "zsh", 501);
    looped.set_comm(61, "zsh", 501);
    for procs in [unreadable, no_comm, agent_walk, looped] {
        let (state, _grants) = state_with(&fx, procs);
        let refused = gate_call(&state, gate_params("t", "OPEN_KEY"));
        assert_eq!(refused, Err(ErrorKind::GateRefused));
    }
}

/// Why: Fail-Open Check — with no peer pid there is no gate to verify.
/// Red when a missing pid skips the check.
/// Test: itself.
#[test]
fn gate_with_no_peer_pid_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let (state, _grants) = state_with(&fx, gate_procs());
    let token = token_on(&fx, &state, &["OPEN_KEY"]);
    let refused = remote::gate(&state, Caller::new(None), gate_params(&token, "OPEN_KEY"));
    assert_eq!(refused, Err(ErrorKind::GateRefused));
}

/// Why: design S1 (c) — the inner verbs are a closed enum: any other verb,
/// odd casing, or none, through a verified gate peer, is refused and
/// resolves nothing.
/// Red when the entry dispatches an unknown verb.
/// Test: itself.
#[test]
fn gate_unknown_inner_verb_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let (state, _grants) = state_with(&fx, gate_procs());
    let token = token_on(&fx, &state, &["OPEN_KEY"]);
    for verb in [
        json!("set"),
        json!("delete"),
        json!("grant"),
        json!("list"),
        json!("Resolve"),
        Value::Null,
    ] {
        let mut params = gate_params(&token, "OPEN_KEY");
        params["verb"] = verb.clone();
        let refused = gate_call(&state, params);
        assert_eq!(refused, Err(ErrorKind::InvalidParams), "{verb}");
    }
    let mut params = gate_params(&token, "OPEN_KEY");
    params.as_object_mut().unwrap().remove("verb");
    assert_eq!(gate_call(&state, params), Err(ErrorKind::InvalidParams));
    // The grant was never spent: nothing resolved.
    let allowed = gate_call(&state, gate_params(&token, "OPEN_KEY"));
    assert_eq!(allowed.unwrap()["value"], OPEN_VALUE);
}

/// Why: critic M-C (iv) — `agent_parent` is forced by the server, never read
/// from the request: a body carrying it, either way, is refused, as is any
/// other unknown field on either verb.
/// Red when the request type accepts unknown fields.
/// Test: itself.
#[test]
fn gate_request_carrying_agent_parent_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let (state, _grants) = state_with(&fx, gate_procs());
    let token = token_on(&fx, &state, &["OPEN_KEY"]);
    for flag in [false, true] {
        let mut params = gate_params(&token, "OPEN_KEY");
        params["agent_parent"] = json!(flag);
        assert_eq!(gate_call(&state, params), Err(ErrorKind::InvalidParams));
    }
    let doctor = json!({"verb": "doctor", "agent_parent": false});
    assert_eq!(gate_call(&state, doctor), Err(ErrorKind::InvalidParams));
    assert!(gate_call(&state, json!({"verb": "doctor"})).is_ok());
}

/// Why: critic M-A, M-C (v) — the entry wraps `secrets.resolve` unchanged,
/// so through a verified gate peer a missing token does not decode and a
/// wrong one is `GrantRefused`, with a deny record and no value.
/// Red when the gate entry skips `check_scoped`.
/// Test: itself.
#[test]
fn gate_with_a_missing_or_wrong_token_is_refused() {
    let fx = fixture();
    listed(&fx);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let (state, _grants) = state_with(&fx, gate_procs());
    let mut missing = gate_params("t", "OPEN_KEY");
    missing.as_object_mut().unwrap().remove("token");
    assert_eq!(gate_call(&state, missing), Err(ErrorKind::InvalidParams));
    let wrong = gate_call(&state, gate_params("not-a-token", "OPEN_KEY"));
    assert_eq!(wrong, Err(ErrorKind::GrantRefused));
    let rows = resolve_records(&fx);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].decision, AuditDecision::Deny);
    assert_eq!(rows[0].host.as_deref(), Some(HOST));
    assert_eq!(
        rows[0].reason.as_ref().map(|r| r.as_str()),
        Some("grant_refused")
    );
}

/// Why: AC2 on the server — the allowlist is re-checked in the account's
/// file: an unknown host, an unmapped project, a disabled host, an unlisted
/// key, no key list, a corrupt or absent file all refuse, even with a valid
/// grant.
/// Red when the server trusts the gate's pre-check.
/// Test: itself.
#[test]
fn gate_resolve_off_the_server_allowlist_is_refused() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let dir = fx.project();
    let cases = [
        host_entry("other-host", PROJECT, &dir, BOTH_KEYS),
        host_entry(HOST, "acme/other", &dir, BOTH_KEYS),
        format!(
            "{}      enabled: false\n",
            host_entry(HOST, PROJECT, &dir, BOTH_KEYS)
        ),
        host_entry(HOST, PROJECT, &dir, "      keys: [API_KEY]\n"),
        host_entry(HOST, PROJECT, &dir, ""),
    ];
    for hosts in cases {
        account(&fx, &hosts);
        let (state, _grants) = state_with(&fx, gate_procs());
        let token = token_on(&fx, &state, &["OPEN_KEY"]);
        let refused = gate_call(&state, gate_params(&token, "OPEN_KEY"));
        assert_eq!(refused, Err(ErrorKind::RemoteRefused), "{hosts}");
    }
    listed(&fx);
    let (mut state, _grants) = state_with(&fx, gate_procs());
    let token = token_on(&fx, &state, &["OPEN_KEY"]);
    state.file_consent_config = None;
    let absent = gate_call(&state, gate_params(&token, "OPEN_KEY"));
    assert_eq!(absent, Err(ErrorKind::RemoteRefused));
}

/// Why: AC1 — a verb other than `resolve` or `doctor` (odd casing
/// included) exits 13 and sends nothing; so does a frame that is not one
/// typed value on one line: an unknown field, a duplicate key, a second
/// value, a second frame, over-size or not JSON. The socket counts every
/// connection.
/// Red when the gate relays first, or checks verbs by a deny list.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_gate_refuses_an_unlisted_verb_and_sends_nothing() {
    let fx = fixture();
    listed(&fx);
    let silent = silent_socket(&fx.settings.socket);
    let config = gate_config(&fx);
    let resolve = r#"{"method":"resolve","project":"acme/web","key":"OPEN_KEY"}"#;
    let mut inputs: Vec<(String, &str)> = [
        "set",
        "delete",
        "set_agents_may_use",
        "grant",
        "secrets.gate",
        "Resolve",
        "DOCTOR",
    ]
    .iter()
    .map(|verb| {
        let frame = format!(r#"{{"method":"{verb}","project":"acme/web","key":"OPEN_KEY"}}"#);
        (format!("{frame}\n"), "method_refused")
    })
    .collect();
    for bad in [
        r#"{"method":"resolve","project":"acme/web","key":"OPEN_KEY","host":"x"}"#.to_owned(),
        r#"{"method":"resolve","method":"doctor","project":"acme/web","key":"OPEN_KEY"}"#
            .to_owned(),
        format!("{resolve} {{}}"),
        format!("{resolve}\n{resolve}"),
        format!(
            r#"{{"method":"resolve","project":"acme/web","key":"{}"}}"#,
            "K".repeat(70_000)
        ),
        "not json".to_owned(),
    ] {
        inputs.push((format!("{bad}\n"), "invalid_frame"));
    }
    for (input, kind) in inputs {
        let (status, line) = gate_raw(&config, input.clone()).await;
        assert_eq!(status, EXIT_REFUSED, "{input:.80}");
        assert_eq!(line, json!({"ok": false, "error": kind}), "{input:.80}");
    }
    assert_eq!(
        silent.accepted.load(Ordering::SeqCst),
        0,
        "a refused frame was relayed"
    );
}

/// Why: critic M3 — config failure is closed: a missing or unparsable
/// file, an unknown host, a host with no `keys` or an empty list (for a
/// `doctor` too), and a key not on the list each exit 13 with nothing
/// relayed.
/// Red when any of them reads as "allowed".
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_gate_config_failures_refuse_and_send_nothing() {
    let fx = fixture();
    let silent = silent_socket(&fx.settings.socket);
    let config = gate_config(&fx);
    let dir = fx.project();
    let frame = r#"{"method":"resolve","project":"acme/web","key":"OPEN_KEY"}"#;
    let refused = json!({"ok": false, "error": "remote_refused"});
    std::fs::remove_file(&fx.settings.machine_config).unwrap();
    assert_eq!(gate(&config, frame).await, (EXIT_REFUSED, refused.clone()));
    std::fs::write(&fx.settings.machine_config, "secrets: [not, a, map\n").unwrap();
    assert_eq!(gate(&config, frame).await, (EXIT_REFUSED, refused.clone()));
    for hosts in [
        host_entry("other-host", PROJECT, &dir, BOTH_KEYS),
        host_entry(HOST, PROJECT, &dir, ""),
        host_entry(HOST, PROJECT, &dir, "      keys: []\n"),
        host_entry(HOST, PROJECT, &dir, "      keys: [API_KEY]\n"),
    ] {
        account(&fx, &hosts);
        assert_eq!(
            gate(&config, frame).await,
            (EXIT_REFUSED, refused.clone()),
            "{hosts}"
        );
    }
    // A doctor names no key, so only the key-list check refuses it.
    let doctor = r#"{"method":"doctor","project":"acme/web"}"#;
    for keys in ["", "      keys: []\n"] {
        account(&fx, &host_entry(HOST, PROJECT, &dir, keys));
        assert_eq!(gate(&config, doctor).await, (EXIT_REFUSED, refused.clone()));
    }
    assert_eq!(silent.accepted.load(Ordering::SeqCst), 0);
}

/// Why: critic H2 — no checkout on this machine fails closed: no map entry,
/// a mapped dir that is missing, one that is no checkout, and one whose
/// `origin` names another repository each exit 13 and issue no grant.
/// Red when the gate grants against a path it did not verify.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_gate_without_a_matching_checkout_refuses_and_sends_nothing() {
    let fx = fixture();
    let silent = silent_socket(&fx.settings.socket);
    let config = gate_config(&fx);
    let plain = fx.tmp.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let missing = fx.tmp.path().join("missing").display().to_string();
    let frame =
        |project: &str| format!(r#"{{"method":"resolve","project":"{project}","key":"OPEN_KEY"}}"#);
    for (hosts, project) in [
        (
            host_entry(HOST, "acme/other", &fx.project(), BOTH_KEYS),
            PROJECT,
        ),
        (host_entry(HOST, PROJECT, &missing, BOTH_KEYS), PROJECT),
        (
            host_entry(HOST, PROJECT, &plain.display().to_string(), BOTH_KEYS),
            PROJECT,
        ),
        (
            host_entry(HOST, "acme/other", &fx.project(), BOTH_KEYS),
            "acme/other",
        ),
    ] {
        account(&fx, &hosts);
        let (status, line) = gate(&config, &frame(project)).await;
        assert_eq!(status, EXIT_REFUSED, "{hosts}");
        assert_eq!(line, json!({"ok": false, "error": "remote_refused"}));
    }
    assert_eq!(silent.accepted.load(Ordering::SeqCst), 0);
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
        r#"{"method":"resolve","project":"acme/web","key":"OPEN_KEY"}"#,
    )
    .await;
    assert_eq!(status, EXIT_UNAVAILABLE);
    assert_eq!(line, json!({"ok": false, "error": SERVER_ABSENT}));
    assert!(!fx.settings.socket.parent().unwrap().exists());
}

/// Why: AC4, critic M5 — a server that never answers, or a frame that never
/// ends, answers 13 (a timeout is never 69) at the deadline. The server's
/// own gate deadline plus its body grace stays inside the gate's 10 s.
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
        r#"{"method":"resolve","project":"acme/web","key":"OPEN_KEY"}"#,
    )
    .await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(status, EXIT_REFUSED);
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
    assert_eq!(reply.status, EXIT_REFUSED);
}

/// Why: end to end — the gate grants, then resolves through `secrets.gate`:
/// an open key resolves (exit 0, value on stdout only), a denied key exits
/// 13 with a deny record, doctor answers, and no value or token reaches the
/// logs, the audit file or the reply.
/// Red when the gate drops the forced `agent_parent` or relays the frame's
/// host.
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
    assert!(open_line["result"].get("token").is_none());
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
    let rows: Vec<_> = resolve_records(&fx);
    assert!(
        rows.iter().all(|r| r.host.as_deref() == Some(HOST)),
        "{rows:?}"
    );
    let denies = rows.iter().filter(|r| r.decision == AuditDecision::Deny);
    assert_eq!(denies.count(), 1, "{rows:?}");
}

/// Why: critic M5 — every server error exits 13 with its fixed kind, one
/// without a kind (an older server) too; only a result is exit 0.
/// Test: itself.
#[test]
fn remote_gate_answer_table() {
    use crate::server::remote_gate::answer;
    let failing = |kind: ErrorKind| RpcResponse {
        jsonrpc: "2.0".to_owned(),
        id: json!(1),
        result: None,
        error: Some(kind.to_rpc(method::GATE)),
    };
    for kind in [
        ErrorKind::AgentKeyDenied,
        ErrorKind::RemoteRefused,
        ErrorKind::GateRefused,
        ErrorKind::GrantRefused,
        ErrorKind::BackendTimeout,
        ErrorKind::BackendLocked,
        ErrorKind::AuditUnavailable,
        ErrorKind::DeadlineExceeded,
    ] {
        let reply = answer(failing(kind));
        assert_eq!(reply.status, EXIT_REFUSED, "{kind:?}");
        assert_eq!(reply.line, json!({"ok": false, "error": kind.as_str()}));
    }
    let mut unknown = failing(ErrorKind::RemoteRefused);
    unknown.error.as_mut().unwrap().data = None;
    assert_eq!(answer(unknown).status, EXIT_REFUSED);
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
/// key compares exactly, a disabled host, an absent entry or an empty key
/// list allows nothing.
/// Test: itself.
#[test]
fn remote_allowlist_table() {
    let machine: crate::store::config::MachineSecretsConfig = serde_yaml::from_str(
        "remote_hosts:\n  evo-linux:\n    projects: {Acme/Web: /w}\n    keys: [K]\n  off:\n    enabled: false\n    projects: {acme/web: /w}\n    keys: [K]\n  nokeys:\n    projects: {acme/web: /w}\n",
    )
    .unwrap();
    let (owner, repo) = remote::parse_project("acme/web").unwrap();
    let k = key("K");
    assert!(remote::host_allows(
        &machine,
        "EVO-linux",
        &owner,
        &repo,
        &k
    ));
    assert!(!remote::host_allows(
        &machine,
        "evo-linux",
        &owner,
        &repo,
        &key("k")
    ));
    for host in ["off", "nobody", "nokeys"] {
        assert!(
            !remote::host_allows(&machine, host, &owner, &repo, &k),
            "{host}"
        );
    }
    let (owner, repo) = remote::parse_project("acme/other").unwrap();
    assert!(!remote::host_allows(
        &machine,
        "evo-linux",
        &owner,
        &repo,
        &k
    ));
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
