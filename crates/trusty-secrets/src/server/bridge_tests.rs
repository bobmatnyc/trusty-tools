//! The console bridge's verdict over real sockets (#9066, S3a PR1).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the backends are in memory, and the
//! socket peer of every call is this test process. A test makes that process
//! the verified console by giving it a `trusty-console` executable in the
//! fake process table.
//!
//! The bridge's wire names and its assertion field are literals here, and
//! the audit log is read as raw JSON, so a rename or a dropped field fails
//! these tests.
//!
//! Test: itself.

use super::*;

/// The bridged `secrets.set_agents_may_use`.
const BRIDGE_FLAG: &str = "secrets.bridge.set_agents_may_use";
/// The bridged `secrets.set`.
const BRIDGE_SET: &str = "secrets.bridge.set";
/// An executable path whose file name is the console's.
const CONSOLE_EXE: &str = "/opt/trusty/bin/trusty-console";

/// `procs_with_me`, with this process running the console's executable.
fn console_procs() -> Arc<FakeProcs> {
    let procs = procs_with_me();
    procs.set_executable(me(), CONSOLE_EXE);
    procs
}

/// `procs_with_me` with the shared parent 10 marked as Claude Code.
fn agent_tree() -> Arc<FakeProcs> {
    let procs = procs_with_me();
    procs.set_agent(10);
    procs
}

/// The console's assertion about its own HTTP client.
fn assertion(agent_parent: bool, source: &str) -> Value {
    json!({"agent_parent": agent_parent, "source": source})
}

/// `params` with `assertion` beside them, in the `bridge` field.
fn bridged(mut params: Value, assertion: Value) -> Value {
    params["bridge"] = assertion;
    params
}

/// `set_agents_may_use` params for `name` in the project vault.
fn flag(fx: &Fixture, name: &str, allowed: bool) -> Value {
    json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": name, "allowed": allowed})
}

/// `set` params for `name` = `value` in the project vault.
fn set_params(fx: &Fixture, name: &str, value: &str) -> Value {
    json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": name, "value": value})
}

/// The flag as the fixture's project backend holds it.
fn flag_on(fx: &Fixture, name: &str) -> bool {
    fx.keychain
        .agents_may_use(&vault(PROJECT_VAULT), &key(name))
        .unwrap()
}

/// Whether the fixture's project backend holds `name`.
fn holds(fx: &Fixture, name: &str) -> bool {
    fx.keychain
        .get(&vault(PROJECT_VAULT), &key(name))
        .unwrap()
        .is_some()
}

/// The error's wire kind; `None` for a success or an error with no kind.
fn kind(response: &RpcResponse) -> Option<String> {
    let data = response.error.as_ref()?.data.as_ref()?;
    data["kind"].as_str().map(str::to_owned)
}

/// The error object as JSON, or `Null` for a success.
fn error_json(response: &RpcResponse) -> Value {
    serde_json::to_value(&response.error).unwrap()
}

/// Every audit line as raw JSON, so a field this build may lack still reads.
fn raw_records(fx: &Fixture) -> Vec<Value> {
    match std::fs::read_to_string(&fx.settings.audit_log) {
        Ok(text) => text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("{e}"),
    }
}

/// The part of a record a direct call and a bridge call must share.
fn common(record: &Value) -> Value {
    json!({
        "method": record["method"],
        "decision": record["decision"],
        "reason": record["reason"],
        "vault": record["vault"],
        "key": record["key"],
        "backend": record["backend"],
        "agents_allowed": record["agents_allowed"],
        "caller_pid": record["caller_pid"],
    })
}

/// A started server over `procs`, with `name` seeded unflagged.
async fn serve(fx: &Fixture, procs: Arc<FakeProcs>, name: &str) -> Running {
    seed(fx, name, SENTINEL_VALUE, false);
    let (state, _grants) = state_with(fx, procs);
    fx.start_state(state).await
}

/// Why: AC 3 — a bridge call leaves the record a direct call leaves, plus
/// the console's `agent_parent` and `source`, on the allow and on the deny.
/// From the verified console, `agent_parent: true` refuses ON with
/// `agent_use_refused`, as a direct caller under Claude Code is refused.
/// Red until the bridge method exists and its records carry both fields.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_call_is_audited_allow_and_deny() {
    let fx = fixture();
    seed(&fx, "OTHER_KEY", OPEN_VALUE, false);
    let server = serve(&fx, console_procs(), "API_KEY").await;
    let socket = &fx.settings.socket;
    let allowed = call(
        socket,
        BRIDGE_FLAG,
        bridged(flag(&fx, "API_KEY", true), assertion(false, "mapped")),
    )
    .await;
    let denied = call(
        socket,
        BRIDGE_FLAG,
        bridged(flag(&fx, "OTHER_KEY", true), assertion(true, "mapped")),
    )
    .await;
    server.stop().await;

    assert_eq!(kind(&denied).as_deref(), Some("agent_use_refused"));
    assert_eq!(ok(allowed)["allowed"], true);
    assert!(flag_on(&fx, "API_KEY"), "the allowed flag went ON");
    assert!(!flag_on(&fx, "OTHER_KEY"), "the refused flag stayed OFF");
    let all = raw_records(&fx);
    assert_eq!(all.len(), 2, "{all:?}");
    let pid = json!(me());
    for (record, decision, reason, name, agent_parent) in [
        (&all[0], "allow", Value::Null, "API_KEY", false),
        (
            &all[1],
            "deny",
            json!("agent_use_refused"),
            "OTHER_KEY",
            true,
        ),
    ] {
        assert_eq!(record["method"], method::SET_AGENTS_MAY_USE, "{record}");
        assert_eq!(record["decision"], decision, "{record}");
        assert_eq!(record["reason"], reason, "{record}");
        assert_eq!(record["key"], name, "{record}");
        assert_eq!(record["caller_pid"], pid, "{record}");
        assert_eq!(record["agents_allowed"], true, "{record}");
        assert_eq!(record["agent_parent"], agent_parent, "{record}");
        assert_eq!(record["source"], "mapped", "{record}");
    }
}

/// Why: Dd7 option B — a bridge call gets parity with the direct socket.
/// Each row runs once on the direct socket, with the caller's own tree
/// holding (or not) a Claude Code process, and once through the bridge,
/// with the console asserting the same verdict. The wire error, the result
/// and the records must match; the bridge records add `source` and
/// `agent_parent`. `unmappable` (no pid) is a non-agent.
/// Red until the bridge method exists.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_verdict_parity_with_direct_socket_table() {
    // (agent ancestor, flag value, source, expected kind)
    let rows: [(bool, bool, &str, Option<&str>); 5] = [
        (true, true, "mapped", Some("agent_use_refused")),
        (true, false, "mapped", None),
        (false, true, "mapped", None),
        (false, false, "mapped", None),
        (false, true, "unmappable", None),
    ];
    for (agent, allowed, source, expected) in rows {
        let direct_fx = fixture();
        let tree = if agent { agent_tree() } else { procs_with_me() };
        let server = serve(&direct_fx, tree, "API_KEY").await;
        let direct = call(
            &direct_fx.settings.socket,
            method::SET_AGENTS_MAY_USE,
            flag(&direct_fx, "API_KEY", allowed),
        )
        .await;
        server.stop().await;

        let bridge_fx = fixture();
        let server = serve(&bridge_fx, console_procs(), "API_KEY").await;
        let via_bridge = call(
            &bridge_fx.settings.socket,
            BRIDGE_FLAG,
            bridged(
                flag(&bridge_fx, "API_KEY", allowed),
                assertion(agent, source),
            ),
        )
        .await;
        server.stop().await;

        let row = format!("agent={agent} allowed={allowed} source={source}");
        assert_eq!(kind(&via_bridge).as_deref(), expected, "{row}");
        assert_eq!(error_json(&via_bridge), error_json(&direct), "{row}");
        assert_eq!(via_bridge.result, direct.result, "{row}");
        assert_eq!(
            flag_on(&bridge_fx, "API_KEY"),
            flag_on(&direct_fx, "API_KEY"),
            "{row}"
        );
        let direct_records = raw_records(&direct_fx);
        let bridge_records = raw_records(&bridge_fx);
        let shapes = |records: &[Value]| records.iter().map(common).collect::<Vec<_>>();
        assert_eq!(shapes(&bridge_records), shapes(&direct_records), "{row}");
        assert!(!bridge_records.is_empty(), "{row}");
        for record in &bridge_records {
            assert_eq!(record["source"], source, "{row}: {record}");
            assert_eq!(record["agent_parent"], agent, "{row}: {record}");
            assert!(direct_records.iter().all(|r| r.get("source").is_none()));
        }
    }

    // A bridged `set` answers and records as a direct `set` does.
    let direct_fx = fixture();
    let server = serve(&direct_fx, procs_with_me(), "API_KEY").await;
    let direct = call(
        &direct_fx.settings.socket,
        method::SET,
        set_params(&direct_fx, "NEW_KEY", OPEN_VALUE),
    )
    .await;
    server.stop().await;
    let bridge_fx = fixture();
    let server = serve(&bridge_fx, console_procs(), "API_KEY").await;
    let via_bridge = call(
        &bridge_fx.settings.socket,
        BRIDGE_SET,
        bridged(
            set_params(&bridge_fx, "NEW_KEY", OPEN_VALUE),
            assertion(false, "mapped"),
        ),
    )
    .await;
    server.stop().await;
    assert_eq!(via_bridge.result, direct.result);
    assert!(via_bridge.result.is_some(), "{:?}", via_bridge.error);
    let direct_records = raw_records(&direct_fx);
    let bridge_records = raw_records(&bridge_fx);
    assert_eq!(
        bridge_records.iter().map(common).collect::<Vec<_>>(),
        direct_records.iter().map(common).collect::<Vec<_>>()
    );
    assert_eq!(bridge_records[0]["source"], "mapped");
}

/// Why: the server takes the console's verdict only from the verified
/// console. A peer whose executable is not `trusty-console`, whose
/// executable cannot be read, or which itself runs under Claude Code is
/// refused with `bridge_refused`, changes nothing, and leaves one deny
/// record naming the assertion it sent.
/// Red until the bridge method checks its peer.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_assertion_from_non_console_peer_is_refused() {
    let impostor = procs_with_me();
    impostor.set_executable(me(), "/usr/bin/python3");
    let renamed = procs_with_me();
    renamed.set_executable(me(), "/opt/trusty/bin/trusty-console-shim");
    let unreadable_exe = procs_with_me();
    let under_agent = console_procs();
    under_agent.set_agent(10);
    for (case, procs) in [
        ("impostor", impostor),
        ("renamed", renamed),
        ("unreadable executable", unreadable_exe),
        ("console under an agent", under_agent),
    ] {
        let fx = fixture();
        let server = serve(&fx, procs, "API_KEY").await;
        let socket = &fx.settings.socket;
        let on = call(
            socket,
            BRIDGE_FLAG,
            bridged(flag(&fx, "API_KEY", true), assertion(false, "mapped")),
        )
        .await;
        let set = call(
            socket,
            BRIDGE_SET,
            bridged(
                set_params(&fx, "NEW_KEY", OPEN_VALUE),
                assertion(false, "unmappable"),
            ),
        )
        .await;
        server.stop().await;

        assert_eq!(kind(&on).as_deref(), Some("bridge_refused"), "{case}");
        assert_eq!(kind(&set).as_deref(), Some("bridge_refused"), "{case}");
        assert!(!flag_on(&fx, "API_KEY"), "{case}: the flag stayed OFF");
        assert!(!holds(&fx, "NEW_KEY"), "{case}: nothing was stored");
        let all = raw_records(&fx);
        assert_eq!(all.len(), 2, "{case}: {all:?}");
        for (record, method_name, source) in [
            (&all[0], method::SET_AGENTS_MAY_USE, "mapped"),
            (&all[1], method::SET, "unmappable"),
        ] {
            assert_eq!(record["method"], method_name, "{case}: {record}");
            assert_eq!(record["decision"], "deny", "{case}: {record}");
            assert_eq!(record["reason"], "bridge_refused", "{case}: {record}");
            assert_eq!(record["source"], source, "{case}: {record}");
            assert_eq!(record["agent_parent"], false, "{case}: {record}");
            assert_eq!(record["caller_pid"], json!(me()), "{case}: {record}");
        }
    }
}

/// Why: Architect 04:03Z — a mapped pid whose chain cannot be read gets what
/// the direct socket gives an unreadable chain (`set_flag_unreadable_ancestry_refuses_on`):
/// ON is `grant_refused` with one deny record, OFF is allowed. The console
/// says so with `source: unreadable`, which must carry `agent_parent: true`.
/// Red until the bridge method maps `unreadable` to the direct refusal.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_unreadable_chain_matches_direct_socket() {
    let direct_fx = fixture();
    let unreadable = procs_with_me();
    unreadable.set_unreadable();
    let server = serve(&direct_fx, unreadable, "API_KEY").await;
    let socket = &direct_fx.settings.socket;
    let direct_on = call(
        socket,
        method::SET_AGENTS_MAY_USE,
        flag(&direct_fx, "API_KEY", true),
    )
    .await;
    let direct_off = call(
        socket,
        method::SET_AGENTS_MAY_USE,
        flag(&direct_fx, "API_KEY", false),
    )
    .await;
    server.stop().await;

    let bridge_fx = fixture();
    let server = serve(&bridge_fx, console_procs(), "API_KEY").await;
    let socket = &bridge_fx.settings.socket;
    let unreadable_chain = assertion(true, "unreadable");
    let bridge_on = call(
        socket,
        BRIDGE_FLAG,
        bridged(flag(&bridge_fx, "API_KEY", true), unreadable_chain.clone()),
    )
    .await;
    let bridge_off = call(
        socket,
        BRIDGE_FLAG,
        bridged(flag(&bridge_fx, "API_KEY", false), unreadable_chain),
    )
    .await;
    server.stop().await;

    assert_eq!(kind(&bridge_on).as_deref(), Some("grant_refused"));
    assert_eq!(error_json(&bridge_on), error_json(&direct_on));
    assert_eq!(bridge_off.result, direct_off.result);
    assert_eq!(ok(bridge_off)["allowed"], false);
    assert!(!flag_on(&bridge_fx, "API_KEY"), "the flag stayed OFF");
    let direct_records = raw_records(&direct_fx);
    let bridge_records = raw_records(&bridge_fx);
    assert_eq!(
        bridge_records.iter().map(common).collect::<Vec<_>>(),
        direct_records.iter().map(common).collect::<Vec<_>>()
    );
    assert_eq!(bridge_records.len(), 2, "{bridge_records:?}");
    for record in &bridge_records {
        assert_eq!(record["source"], "unreadable", "{record}");
    }
}

/// Why: AC 3 and the Fail-Open Check — when the audit record cannot be
/// written, a bridge call that would change a backend changes nothing: no
/// flag write, no value write.
/// Red until the bridge method exists (a missing method also changes
/// nothing, so the kind is what proves it).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_audit_write_failure_refuses() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, console_procs());
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let on = call(
        socket,
        BRIDGE_FLAG,
        bridged(flag(&fx, "API_KEY", true), assertion(false, "mapped")),
    )
    .await;
    let set = call(
        socket,
        BRIDGE_SET,
        bridged(
            set_params(&fx, "NEW_KEY", OPEN_VALUE),
            assertion(false, "mapped"),
        ),
    )
    .await;
    server.stop().await;

    assert_eq!(kind(&on).as_deref(), Some("audit_unavailable"));
    assert_eq!(kind(&set).as_deref(), Some("audit_unavailable"));
    assert!(!flag_on(&fx, "API_KEY"), "the flag stayed OFF");
    assert!(!holds(&fx, "NEW_KEY"), "no value was written");
}

/// Why: the Fail-Open Check — an assertion that is missing, mistyped, names
/// an unknown source, carries an extra field (a pid is never a parameter),
/// or contradicts its source is refused with `invalid_params`, changes
/// nothing, and leaves one deny record with `source: malformed`.
/// Red until the bridge method validates its assertion.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_malformed_assertion_is_refused_and_audited() {
    let cases = [
        ("missing", None),
        ("not an object", Some(json!(true))),
        (
            "mistyped",
            Some(json!({"agent_parent": "no", "source": "mapped"})),
        ),
        ("no source", Some(json!({"agent_parent": false}))),
        ("unknown source", Some(assertion(false, "trusted"))),
        (
            "a pid",
            Some(json!({"agent_parent": false, "source": "mapped", "pid": 1})),
        ),
        ("unmappable agent", Some(assertion(true, "unmappable"))),
        ("unreadable non-agent", Some(assertion(false, "unreadable"))),
    ];
    let fx = fixture();
    let server = serve(&fx, console_procs(), "API_KEY").await;
    for (case, sent) in &cases {
        let mut params = flag(&fx, "API_KEY", true);
        if let Some(sent) = sent {
            params["bridge"] = sent.clone();
        }
        let refused = call(&fx.settings.socket, BRIDGE_FLAG, params).await;
        assert_eq!(kind(&refused).as_deref(), Some("invalid_params"), "{case}");
    }
    server.stop().await;

    assert!(!flag_on(&fx, "API_KEY"), "the flag stayed OFF");
    let all = raw_records(&fx);
    assert_eq!(all.len(), cases.len(), "{all:?}");
    for record in &all {
        assert_eq!(record["method"], method::SET_AGENTS_MAY_USE, "{record}");
        assert_eq!(record["decision"], "deny", "{record}");
        assert_eq!(record["reason"], "invalid_params", "{record}");
        assert_eq!(record["source"], "malformed", "{record}");
    }
}

/// Why: DOC-74 §12 "No value reaches the console" — a specimen value sent
/// through the bridge never comes back whole: not in the masked set
/// confirmation, not in a refusal's error, not in the audit log, not in a
/// log line.
/// Red until the bridged set answers.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_errors_and_masked_set_never_carry_the_specimen_value() {
    capture_logs();
    let fx = fixture();
    let server = serve(&fx, console_procs(), "API_KEY").await;
    let socket = &fx.settings.socket;
    let stored = call(
        socket,
        BRIDGE_SET,
        bridged(
            set_params(&fx, "NEW_KEY", SENTINEL_VALUE),
            assertion(false, "mapped"),
        ),
    )
    .await;
    let malformed = call(
        socket,
        BRIDGE_SET,
        bridged(
            set_params(&fx, "BAD_KEY", SENTINEL_VALUE),
            json!({"agent_parent": SENTINEL_VALUE, "source": SENTINEL_VALUE}),
        ),
    )
    .await;
    let out_of_scope = call(
        socket,
        BRIDGE_SET,
        bridged(
            json!({"project": fx.project(), "vault": "trusty/other/repo",
                "key": "K", "value": SENTINEL_VALUE}),
            assertion(false, "mapped"),
        ),
    )
    .await;
    server.stop().await;

    assert!(!wire(&stored).contains(SENTINEL_VALUE));
    assert_eq!(ok(stored)["masked"], mask_secret(SENTINEL_VALUE));
    for refused in [&malformed, &out_of_scope] {
        assert!(refused.result.is_none());
        assert!(kind(refused).is_some(), "{:?}", refused.error);
        assert!(!wire(refused).contains(SENTINEL_VALUE));
    }
    let log = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!log.contains(SENTINEL_VALUE));
    assert!(!captured().contains(SENTINEL_VALUE));
}

/// Why: DOC-74 §15.6 — `mask_secret` on a specimen shows its first 8
/// characters and its length, never the whole value; a short one shows its
/// length only.
/// Test: itself.
#[test]
fn mask_secret_never_shows_a_specimen_value_whole() {
    let masked = mask_secret(SENTINEL_VALUE);
    assert!(!masked.contains(SENTINEL_VALUE), "{masked}");
    assert_eq!(
        masked,
        format!("SENTINEL… [{} chars]", SENTINEL_VALUE.len())
    );
    assert_eq!(mask_secret("hunter2!"), "[8 chars]");
}
