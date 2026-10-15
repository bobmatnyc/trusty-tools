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
use crate::server::methods::Caller;

/// The bridged `secrets.set_agents_may_use`.
const BRIDGE_FLAG: &str = "secrets.bridge.set_agents_may_use";
/// The bridged `secrets.set`.
const BRIDGE_SET: &str = "secrets.bridge.set";
/// The bridged `secrets.list`.
const BRIDGE_LIST: &str = "secrets.bridge.list";
/// The bridged `secrets.delete`.
const BRIDGE_DELETE: &str = "secrets.bridge.delete";
/// An executable path whose file name is the console's.
const CONSOLE_EXE: &str = "/opt/trusty/bin/trusty-console";
/// #10394: `SENTINEL_VALUE` under the ruled mask — first 4, last 5, length.
const BRIDGE_MASK: &str = "SENT…w2Pn6 [32 chars]";

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

/// Whether agents may use `name`: no deny in the project backend (#10386).
fn flag_on(fx: &Fixture, name: &str) -> bool {
    !fx.keychain
        .control_item(
            ControlItem::AgentDeny,
            &vault(PROJECT_VAULT),
            Some(&key(name)),
        )
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

/// A started server over `procs`, with `name` seeded and denied to agents.
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
/// `agent_parent`. #9067 Architect Q5: `unmappable` (no pid) is an agent, so
/// removing a deny (`allowed: true`) is `agent_use_refused` and writing one
/// is allowed, as for an agent tree (#10386).
/// Red until the bridge method exists.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_verdict_parity_with_direct_socket_table() {
    // (agent ancestor, flag value, source, expected kind)
    let rows: [(bool, bool, &str, Option<&str>); 6] = [
        (true, true, "mapped", Some("agent_use_refused")),
        (true, false, "mapped", None),
        (false, true, "mapped", None),
        (false, false, "mapped", None),
        (true, true, "unmappable", Some("agent_use_refused")),
        (true, false, "unmappable", None),
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

/// Why: #10386 Q2 — `set` and `delete` judge the caller as a deny removal
/// does, so a bridge call must get the direct socket's answer for each
/// verdict: an agent (mapped or unmappable) is `agent_use_refused`, a person
/// is allowed. The wire reply, the stored state and the shared record fields
/// must match. Red when either method skips the bridge verdict.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_set_and_delete_parity_with_direct_socket_table() {
    // (agent ancestor, source, expected kind)
    let rows: [(bool, &str, Option<&str>); 3] = [
        (true, "mapped", Some("agent_use_refused")),
        (false, "mapped", None),
        (true, "unmappable", Some("agent_use_refused")),
    ];
    let target =
        |fx: &Fixture| json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": "API_KEY"});
    for (agent, source, expected) in rows {
        for (direct_method, bridge_method) in
            [(method::SET, BRIDGE_SET), (method::DELETE, BRIDGE_DELETE)]
        {
            let params = |fx: &Fixture| match direct_method {
                method::SET => set_params(fx, "NEW_KEY", OPEN_VALUE),
                _ => target(fx),
            };
            let direct_fx = fixture();
            let tree = if agent { agent_tree() } else { procs_with_me() };
            let server = serve(&direct_fx, tree, "API_KEY").await;
            let direct = call(
                &direct_fx.settings.socket,
                direct_method,
                params(&direct_fx),
            )
            .await;
            server.stop().await;

            let bridge_fx = fixture();
            let server = serve(&bridge_fx, console_procs(), "API_KEY").await;
            let via_bridge = call(
                &bridge_fx.settings.socket,
                bridge_method,
                bridged(params(&bridge_fx), assertion(agent, source)),
            )
            .await;
            server.stop().await;

            let row = format!("{direct_method} agent={agent} source={source}");
            assert_eq!(kind(&via_bridge).as_deref(), expected, "{row}");
            assert_eq!(error_json(&via_bridge), error_json(&direct), "{row}");
            assert_eq!(via_bridge.result, direct.result, "{row}");
            for name in ["API_KEY", "NEW_KEY"] {
                assert_eq!(
                    holds(&bridge_fx, name),
                    holds(&direct_fx, name),
                    "{row} {name}"
                );
            }
            let shapes = |fx: &Fixture| raw_records(fx).iter().map(common).collect::<Vec<_>>();
            assert_eq!(shapes(&bridge_fx), shapes(&direct_fx), "{row}");
        }
    }
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
                assertion(true, "unmappable"),
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
        for (record, method_name, source, name, agent_parent) in [
            (
                &all[0],
                method::SET_AGENTS_MAY_USE,
                "mapped",
                "API_KEY",
                false,
            ),
            (&all[1], method::SET, "unmappable", "NEW_KEY", true),
        ] {
            assert_eq!(record["method"], method_name, "{case}: {record}");
            // #9066 LOW1: the deny names the key the call attempted.
            assert_eq!(record["vault"], PROJECT_VAULT, "{case}: {record}");
            assert_eq!(record["key"], name, "{case}: {record}");
            assert_eq!(record["decision"], "deny", "{case}: {record}");
            assert_eq!(record["reason"], "bridge_refused", "{case}: {record}");
            assert_eq!(record["source"], source, "{case}: {record}");
            assert_eq!(record["agent_parent"], agent_parent, "{case}: {record}");
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

/// `list` params for the project vault.
fn list_params(fx: &Fixture) -> Value {
    json!({"project": fx.project(), "vault": PROJECT_VAULT})
}

/// Why: every bridge call is audited (#9066 review). An allowed bridged list
/// leaves one allow record naming the vault and the console's verdict, where
/// a direct list leaves none.
/// Red while the bridged list runs the direct list's deny-only recording.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_list_allow_is_audited() {
    let fx = fixture();
    let server = serve(&fx, console_procs(), "API_KEY").await;
    let socket = &fx.settings.socket;
    let listed = call(
        socket,
        BRIDGE_LIST,
        bridged(list_params(&fx), assertion(false, "mapped")),
    )
    .await;
    server.stop().await;

    let listed = ok(listed);
    assert_eq!(listed["vault"], PROJECT_VAULT);
    assert_eq!(listed["keys"].as_array().map(Vec::len), Some(1), "{listed}");
    let all = raw_records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    let record = &all[0];
    assert_eq!(record["method"], method::LIST, "{record}");
    assert_eq!(record["decision"], "allow", "{record}");
    assert_eq!(record["reason"], Value::Null, "{record}");
    assert_eq!(record["vault"], PROJECT_VAULT, "{record}");
    assert_eq!(record["caller_pid"], json!(me()), "{record}");
    assert_eq!(record["source"], "mapped", "{record}");
    assert_eq!(record["agent_parent"], false, "{record}");
}

/// Why: a bridged list is admitted fail-closed like a bridged change: when
/// the audit log cannot be written it is refused with `audit_unavailable`
/// and lists nothing. A direct list on the same server still answers; its
/// allow leaves no record, so it needs none.
/// Red while the bridged list runs the direct list's deny-only recording.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_list_refuses_when_audit_unwritable() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, console_procs());
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let bridge = call(
        socket,
        BRIDGE_LIST,
        bridged(list_params(&fx), assertion(false, "mapped")),
    )
    .await;
    let direct = call(socket, method::LIST, list_params(&fx)).await;
    server.stop().await;

    assert_eq!(kind(&bridge).as_deref(), Some("audit_unavailable"));
    assert!(bridge.result.is_none(), "{:?}", bridge.result);
    assert_eq!(ok(direct)["vault"], PROJECT_VAULT);
}

/// Why: owner ruling Ddf (2026-10-11) — the bridge honours the machine
/// config's `secrets.audit: false` as the direct socket does. A bridged
/// list, a bridged set and a `bridge_refused` deny answer as they do with
/// the audit on, and write no record. With the log then unwritable, the
/// list and the set still succeed: nothing is written, so nothing refuses.
/// Red when a bridge call skips the machine suppression.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_respects_audit_disabled() {
    let fx = fixture();
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: keychain\n  audit: false\n",
    )
    .unwrap();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, console_procs());
    let (refusing, _grants) = state_with(&fx, console_procs());
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let mapped = || assertion(false, "mapped");
    let listed = call(socket, BRIDGE_LIST, bridged(list_params(&fx), mapped())).await;
    let set = call(
        socket,
        BRIDGE_SET,
        bridged(set_params(&fx, "NEW_KEY", OPEN_VALUE), mapped()),
    )
    .await;
    // No peer pid: the console cannot be verified.
    let (_, entry) = crate::server::bridge::METHODS
        .into_iter()
        .find(|(name, _)| *name == BRIDGE_SET)
        .expect("the bridged set method");
    let refused = entry(
        &refusing,
        Caller::new(None),
        bridged(set_params(&fx, "BAD_KEY", OPEN_VALUE), mapped()),
    );
    assert_eq!(ok(listed)["keys"].as_array().map(Vec::len), Some(1));
    assert!(ok(set)["masked"].is_string());
    assert_eq!(refused, Err(ErrorKind::BridgeRefused));
    assert!(holds(&fx, "NEW_KEY") && !holds(&fx, "BAD_KEY"));
    let all = raw_records(&fx);
    assert!(all.is_empty(), "audit off, yet records: {all:?}");

    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let listed = call(socket, BRIDGE_LIST, bridged(list_params(&fx), mapped())).await;
    let set = call(
        socket,
        BRIDGE_SET,
        bridged(set_params(&fx, "LATE_KEY", OPEN_VALUE), mapped()),
    )
    .await;
    server.stop().await;
    assert_eq!(ok(listed)["keys"].as_array().map(Vec::len), Some(2));
    assert!(ok(set)["masked"].is_string());
    assert!(holds(&fx, "LATE_KEY"));
    assert!(raw_records(&fx).is_empty());
}

/// Why: `verify_console`'s last check — a peer that runs the console's
/// executable but whose own ancestry cannot be read is refused with
/// `bridge_refused`, as a console under Claude Code is, and the deny record
/// names the key the call attempted.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_console_with_unreadable_chain_is_refused() {
    let procs = console_procs();
    // The shared parent 10 vanishes: the executable reads, the chain does not.
    procs.remove(10);
    let fx = fixture();
    let server = serve(&fx, procs, "API_KEY").await;
    let on = call(
        &fx.settings.socket,
        BRIDGE_FLAG,
        bridged(flag(&fx, "API_KEY", true), assertion(false, "mapped")),
    )
    .await;
    server.stop().await;

    assert_eq!(kind(&on).as_deref(), Some("bridge_refused"));
    assert!(!flag_on(&fx, "API_KEY"), "the flag stayed OFF");
    let all = raw_records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0]["reason"], "bridge_refused", "{}", all[0]);
    assert_eq!(all[0]["vault"], PROJECT_VAULT, "{}", all[0]);
    assert_eq!(all[0]["key"], "API_KEY", "{}", all[0]);
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
        // #9067 Architect Q5: unmappable must now carry `true`.
        ("unmappable non-agent", Some(assertion(false, "unmappable"))),
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
        assert_eq!(record["vault"], PROJECT_VAULT, "{record}");
        assert_eq!(record["key"], "API_KEY", "{record}");
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
    // #10394: the bridge returns the ruled mask, pinned as a literal.
    assert_eq!(ok(stored)["masked"], BRIDGE_MASK);
    for refused in [&malformed, &out_of_scope] {
        assert!(refused.result.is_none());
        assert!(kind(refused).is_some(), "{:?}", refused.error);
        assert!(!wire(refused).contains(SENTINEL_VALUE));
    }
    let log = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!log.contains(SENTINEL_VALUE));
    assert!(!captured().contains(SENTINEL_VALUE));
}

/// Why: DOC-74 §15.6, #10394 — `mask_secret` on a specimen shows its first
/// 4 and last 5 characters and its length, never the middle; a value under
/// 10 characters shows its length only.
/// Test: itself.
#[test]
fn mask_secret_never_shows_a_specimen_value_whole() {
    // #9066: the `api` path the console bridge links.
    use crate::api::mask_secret;
    let masked = mask_secret(SENTINEL_VALUE);
    assert!(!masked.contains("INEL-VALUE-9070-zq4Rk8L"), "{masked}");
    assert_eq!(masked, BRIDGE_MASK);
    assert_eq!(mask_secret("hunter2!"), "[8 chars]");
    assert_eq!(mask_secret("hunter22!"), "[9 chars]");
}

/// Why: the Fail-Open Check — a bridge call whose peer pid the kernel did
/// not report cannot be verified, so even OFF is refused, with one deny
/// record that names no pid.
/// Red when a missing peer pid skips the console check.
/// Test: itself.
#[test]
fn bridge_with_no_peer_pid_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, console_procs());
    let (_, entry) = crate::server::bridge::METHODS
        .into_iter()
        .find(|(name, _)| *name == BRIDGE_FLAG)
        .expect("the bridged flag method");
    for allowed in [true, false] {
        let params = bridged(flag(&fx, "API_KEY", allowed), assertion(false, "mapped"));
        assert_eq!(
            entry(&state, Caller::new(None), params),
            Err(ErrorKind::BridgeRefused)
        );
    }
    assert!(!flag_on(&fx, "API_KEY"));
    let all = raw_records(&fx);
    assert_eq!(all.len(), 2, "{all:?}");
    for record in &all {
        assert_eq!(record["reason"], "bridge_refused", "{record}");
        assert_eq!(record["caller_pid"], Value::Null, "{record}");
        assert_eq!(record["source"], "mapped", "{record}");
    }
}

/// Why: a bridged call answers under its direct method's name and gets its
/// direct method's deadline, so the console's error text and wait match a
/// direct caller's. Against the host's real process table this test binary
/// is not `trusty-console`, so every bridged method refuses it with fixed
/// text that echoes nothing it sent.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_methods_answer_as_their_direct_method() {
    use crate::server::deadline::{client_wait, request_deadline};
    let fx = fixture();
    let server = fx.start().await;
    for (name, _) in crate::server::bridge::METHODS {
        // #9067 P1: `backends` has no direct method; it answers under its own
        // name with a read's deadline.
        let direct = match name {
            BRIDGE_BACKENDS => {
                assert_eq!(crate::api::bridge::direct_method(name), None);
                assert_eq!(request_deadline(name), request_deadline(method::LIST));
                name
            }
            _ => crate::api::bridge::direct_method(name).expect("a direct method"),
        };
        assert_eq!(request_deadline(name), request_deadline(direct), "{name}");
        assert_eq!(client_wait(name), client_wait(direct), "{name}");
        let response = call(&fx.settings.socket, name, json!(SENTINEL_VALUE)).await;
        assert_eq!(fixed_error(&response, direct), ErrorKind::BridgeRefused);
        assert!(!wire(&response).contains(SENTINEL_VALUE), "{name}");
    }
    server.stop().await;
}

/// Why: owner ruling Ddf (2026-10-11), every arm — with `secrets.audit:
/// false` no bridged method writes a record: `list`, `scopes`, `set`,
/// `delete`, `copy` and `set_agents_may_use` allowed, a `set_agents_may_use` ON
/// refused for an agent client, and an `invalid_params` refusal of a
/// malformed assertion. Each still answers as it does with the audit on.
/// Red when any bridged arm skips the machine suppression.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_audit_disabled_writes_nothing_for_every_method() {
    let fx = fixture();
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: keychain\n  audit: false\n",
    )
    .unwrap();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    seed(&fx, "OLD_KEY", OPEN_VALUE, false);
    let server = serve(&fx, console_procs(), "API_KEY").await;
    let socket = &fx.settings.socket;
    let mapped = || assertion(false, "mapped");
    let project = fx.project();
    let delete = json!({"project": project, "vault": PROJECT_VAULT, "key": "OLD_KEY"});
    let copy = json!({"project": project, "from_backend": "keychain", "to_backend": "spare",
                      "keys": ["API_KEY"]});
    let allowed = [
        ("secrets.bridge.list", list_params(&fx)),
        (BRIDGE_SCOPES, json!({"project": project})),
        (BRIDGE_SET, set_params(&fx, "NEW_KEY", OPEN_VALUE)),
        ("secrets.bridge.delete", delete),
        ("secrets.bridge.copy", copy),
        (BRIDGE_FLAG, flag(&fx, "API_KEY", false)),
    ];
    for (name, params) in allowed {
        let response = call(socket, name, bridged(params, mapped())).await;
        assert!(
            response.error.is_none(),
            "{name}: {:?}",
            error_json(&response)
        );
    }
    let agent = call(
        socket,
        BRIDGE_FLAG,
        bridged(flag(&fx, "API_KEY", true), assertion(true, "mapped")),
    )
    .await;
    assert_eq!(kind(&agent).as_deref(), Some("agent_use_refused"));
    let malformed = call(
        socket,
        BRIDGE_FLAG,
        bridged(flag(&fx, "API_KEY", true), json!({"agent_parent": "no"})),
    )
    .await;
    assert_eq!(kind(&malformed).as_deref(), Some("invalid_params"));
    server.stop().await;

    assert!(holds(&fx, "NEW_KEY") && !holds(&fx, "OLD_KEY"));
    assert!(!flag_on(&fx, "API_KEY"), "the flag stayed OFF");
    let all = raw_records(&fx);
    assert!(all.is_empty(), "audit off, yet records: {all:?}");
}

/// The bridged `secrets.scopes` (#9067).
const BRIDGE_SCOPES: &str = "secrets.bridge.scopes";

/// Why: #9067 — the scope picker's call is refused, like every bridged
/// method, when the socket peer is not the verified console, whatever it
/// asserted; the refusal is `bridge_refused` (-32084) with one deny record.
/// Red until `secrets.bridge.scopes` exists.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_scopes_from_non_console_peer_is_refused() {
    let impostor = procs_with_me();
    impostor.set_executable(me(), "/usr/bin/python3");
    let under_agent = console_procs();
    under_agent.set_agent(10);
    for (case, procs) in [
        ("impostor", impostor),
        ("unreadable executable", procs_with_me()),
        ("console under an agent", under_agent),
    ] {
        let fx = fixture();
        let server = serve(&fx, procs, "API_KEY").await;
        let refused = call(
            &fx.settings.socket,
            BRIDGE_SCOPES,
            bridged(json!({"project": fx.project()}), assertion(false, "mapped")),
        )
        .await;
        server.stop().await;

        assert_eq!(kind(&refused).as_deref(), Some("bridge_refused"), "{case}");
        assert_eq!(
            refused.error.as_ref().map(|e| e.code),
            Some(-32084),
            "{case}"
        );
        assert!(refused.result.is_none(), "{case}");
        let all = raw_records(&fx);
        assert_eq!(all.len(), 1, "{case}: {all:?}");
        assert_eq!(all[0]["method"], method::SCOPES, "{case}");
        assert_eq!(all[0]["decision"], "deny", "{case}");
        assert_eq!(all[0]["reason"], "bridge_refused", "{case}");
        assert_eq!(all[0]["source"], "mapped", "{case}");
    }
}

/// Why: #9067 — the bridged scope picker answers what a direct
/// `secrets.scopes` answers: the project scope, then the owner scope from
/// the origin remote, and never a machine scope. Every bridge call is
/// audited (Ddf), so it leaves one allow record where the direct call
/// leaves none.
/// Red until `secrets.bridge.scopes` exists.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_scopes_returns_only_project_and_owner_kinds() {
    let fx = fixture();
    let server = serve(&fx, console_procs(), "API_KEY").await;
    let socket = &fx.settings.socket;
    let direct = ok(call(socket, method::SCOPES, json!({"project": fx.project()})).await);
    assert!(
        raw_records(&fx).is_empty(),
        "a direct scopes leaves no record"
    );
    let via_bridge = call(
        socket,
        BRIDGE_SCOPES,
        bridged(
            json!({"project": fx.project()}),
            assertion(true, "unmappable"),
        ),
    )
    .await;
    server.stop().await;

    let via_bridge = ok(via_bridge);
    assert_eq!(via_bridge, direct);
    assert_eq!(
        via_bridge,
        json!({"scopes": [
            {"kind": "project", "vault": PROJECT_VAULT},
            {"kind": "owner", "vault": "trusty/acme"},
        ]})
    );
    let kinds: Vec<&str> = via_bridge["scopes"]
        .as_array()
        .expect("scopes")
        .iter()
        .filter_map(|scope| scope["kind"].as_str())
        .collect();
    assert!(
        kinds.iter().all(|k| *k == "project" || *k == "owner"),
        "{kinds:?}"
    );
    assert!(!via_bridge.to_string().contains("machine"), "{via_bridge}");
    let all = raw_records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0]["method"], method::SCOPES);
    assert_eq!(all[0]["decision"], "allow");
    assert_eq!(all[0]["source"], "unmappable");
    assert_eq!(all[0]["agent_parent"], true);
}

/// Why: #9067 review — a bridged scopes call is admitted fail-closed like a
/// bridged list: when the audit log cannot be written it is refused with
/// `audit_unavailable` and returns no scopes. A direct scopes on the same
/// server still answers; it leaves no record, so it needs none.
/// Red when the bridged scopes skips `Gate::admit`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_scopes_refuses_when_audit_unwritable() {
    let fx = fixture();
    let (state, _grants) = state_with(&fx, console_procs());
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let bridge = call(
        socket,
        BRIDGE_SCOPES,
        bridged(json!({"project": fx.project()}), assertion(false, "mapped")),
    )
    .await;
    let direct = call(socket, method::SCOPES, json!({"project": fx.project()})).await;
    server.stop().await;

    assert_eq!(kind(&bridge).as_deref(), Some("audit_unavailable"));
    assert!(bridge.result.is_none(), "{:?}", bridge.result);
    assert!(
        !wire(&bridge).contains(r#""kind":"project""#),
        "{}",
        wire(&bridge)
    );
    assert_eq!(ok(direct)["scopes"][0]["vault"], PROJECT_VAULT);
}

/// The bridged backend list (#9067 P1).
const BRIDGE_BACKENDS: &str = "secrets.bridge.backends";

/// The audit wire name of a bridged backend list (#9067 P1).
const BACKENDS_RECORD: &str = "secrets.backends";

/// Why: #9067 P1 — the copy dialog offers the backends the server reports,
/// never a fixed list. The bridged list is the doctor's own judgement for
/// the project, reduced to id and availability: no path, detail or reason
/// reaches the page. It is read-only and audited like a bridged scopes:
/// one allow record. Red until `secrets.bridge.backends` exists.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_backends_match_doctor_and_are_audited_once() {
    let fx = fixture();
    let server = serve(&fx, console_procs(), "API_KEY").await;
    let socket = &fx.settings.socket;
    let doctor = ok(call(socket, "secrets.doctor", json!({"project": fx.project()})).await);
    let via_bridge = call(
        socket,
        BRIDGE_BACKENDS,
        bridged(
            json!({"project": fx.project()}),
            assertion(true, "unmappable"),
        ),
    )
    .await;
    server.stop().await;

    let via_bridge = ok(via_bridge);
    let expected: Vec<Value> = doctor["backends"]
        .as_array()
        .expect("doctor rows")
        .iter()
        // #9069: D15 — doctor lists sync targets; the copy dialog does not.
        .filter(|row| row["id"] != "github-actions" && row["id"] != "vercel")
        .map(|row| json!({"id": row["id"], "available": row["available"]}))
        .collect();
    assert_eq!(
        via_bridge,
        json!({"selected": doctor["selected_backend"], "backends": expected})
    );
    let wire = via_bridge.to_string();
    for leak in ["detail", "reason", "socket", "/"] {
        assert!(!wire.contains(leak), "{leak} in {wire}");
    }
    assert!(holds(&fx, "API_KEY"), "the list changed nothing");
    let all = raw_records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0]["method"], BACKENDS_RECORD);
    assert_eq!(all[0]["decision"], "allow");
    assert_eq!(all[0]["source"], "unmappable");
    assert_eq!(all[0]["agent_parent"], true);
}

/// Why: #9067 P1 — like every bridged method, the backend list is refused
/// with `bridge_refused` (-32084) and one deny record when the socket peer
/// is not the verified console. Red until `secrets.bridge.backends` exists.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_backends_from_non_console_peer_is_refused() {
    let impostor = procs_with_me();
    impostor.set_executable(me(), "/usr/bin/python3");
    for (case, procs) in [("impostor", impostor), ("unreadable", procs_with_me())] {
        let fx = fixture();
        let server = serve(&fx, procs, "API_KEY").await;
        let refused = call(
            &fx.settings.socket,
            BRIDGE_BACKENDS,
            bridged(json!({"project": fx.project()}), assertion(false, "mapped")),
        )
        .await;
        server.stop().await;

        assert_eq!(kind(&refused).as_deref(), Some("bridge_refused"), "{case}");
        assert_eq!(refused.error.as_ref().map(|e| e.code), Some(-32084));
        assert!(refused.result.is_none(), "{case}");
        let all = raw_records(&fx);
        assert_eq!(all.len(), 1, "{case}: {all:?}");
        assert_eq!(all[0]["method"], BACKENDS_RECORD, "{case}");
        assert_eq!(all[0]["decision"], "deny", "{case}");
    }
}

/// Why: #9067 P1 — a bridged backend list is admitted fail-closed, like a
/// bridged scopes: with the audit log unwritable it is `audit_unavailable`
/// and lists nothing; and a project the server cannot resolve fails with the
/// kind a bridged scopes gets, each with one deny record.
/// Red until `secrets.bridge.backends` exists.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_backends_errors_map_as_bridged_scopes_does() {
    let fx = fixture();
    let (state, _grants) = state_with(&fx, console_procs());
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let restore = RestoreMode(dir);
    let server = fx.start_state(state).await;
    let blocked = call(
        &fx.settings.socket,
        BRIDGE_BACKENDS,
        bridged(json!({"project": fx.project()}), assertion(false, "mapped")),
    )
    .await;
    server.stop().await;
    drop(restore);
    assert_eq!(kind(&blocked).as_deref(), Some("audit_unavailable"));
    assert!(blocked.result.is_none(), "{:?}", blocked.result);

    let fx = fixture();
    let server = serve(&fx, console_procs(), "API_KEY").await;
    let missing = fx.settings.socket.with_file_name("no-such-checkout-9067");
    let mut kinds = Vec::new();
    for name in [BRIDGE_SCOPES, BRIDGE_BACKENDS] {
        let refused = call(
            &fx.settings.socket,
            name,
            bridged(json!({"project": missing}), assertion(false, "mapped")),
        )
        .await;
        assert!(refused.result.is_none(), "{name}");
        kinds.push(kind(&refused));
    }
    server.stop().await;
    assert!(kinds[0].is_some(), "{kinds:?}");
    assert_eq!(kinds[0], kinds[1], "backends maps its error as scopes does");
    let all = raw_records(&fx);
    let decisions = |name: &str| -> Vec<Value> {
        all.iter()
            .filter(|r| r["method"] == name)
            .map(|r| r["decision"].clone())
            .collect()
    };
    assert_eq!(
        decisions(BACKENDS_RECORD),
        decisions(method::SCOPES),
        "{all:?}"
    );
    assert!(all.iter().all(|r| r["decision"] == "deny"), "{all:?}");
}
