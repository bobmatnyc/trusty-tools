//! The Architect exemption for `set` and a local `copy` (#10459 P1), and
//! the main-thread ticket it needs (#10459 PR1, DOC-86 §2).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers.
//! Every record directory, project directory and cargo bin directory is
//! under the fixture's `TempDir`; the account's real `~/.trusty-mpm` and
//! `~/.cargo/bin` are never read. The socket peer is this test process,
//! placed under the fake `claude` 10, whose launch facts and record each
//! test controls; the minting hook is the fake process 15 beside it.
//!
//! Test: itself.

use std::os::unix::fs::PermissionsExt as _;

use super::sync_target_tests::{push_params, with_fake_target};
use super::*;
use crate::server::agents_flag::set_agents_may_use;
use crate::server::architect::{ArchitectRefusal, check_architect};
use crate::server::bridge::BridgeVerdict;
use crate::server::grant::Clock;
use crate::server::methods::{self, Caller};
use crate::server::ticket::TicketStore;
use crate::server::ticket_checks::argv_sha256;
use crate::server::ticket_methods;

// #10459 PR1: mint, void, redeem and every fail-closed arm.
#[path = "ticket_tests.rs"]
mod ticket_tests;

/// The fake Architect `claude`: the shared parent of this test process.
const ARCHITECT: u32 = 10;
/// Its start time in whole Unix seconds, as tm records it.
const LAUNCHED: u64 = 1_790_000_000;
/// The fake `tm hook --pm-guard` process, a fresh child of `claude` 10.
const HOOK: u32 = 15;
/// The fake clock's time at mint, in Unix seconds.
const MINT_AT: u64 = LAUNCHED + 100;
/// The session the fake hook mints in.
const SESSION: &str = "5c1f0e0e-session-10459";
/// This process's argv in the fake table: the call the hook hashed.
const CALL_ARGV: [&str; 4] = ["tm", "secrets", "set", "API_KEY"];

/// A launched Architect: its record directory, its project directory, the
/// cargo bin directory holding the fake `tm`, and a process table where it
/// is `claude` 10 with matching launch facts.
struct Architect {
    records: PathBuf,
    project: PathBuf,
    bin: PathBuf,
    procs: Arc<FakeProcs>,
}

impl Architect {
    /// tm's record body for `pid`, `start` and `project`.
    fn body(pid: u32, start: u64, project: &Path) -> Value {
        json!({"pid": pid, "start_time": start, "project_dir": project,
            "armed_at": "2026-10-29T22:58:00+00:00"})
    }

    /// The record file of `claude` 10.
    fn record_path(&self) -> PathBuf {
        self.records.join(format!("{ARCHITECT}.architect"))
    }

    /// Write `body` as the record of `claude` 10, mode 0600.
    fn write(&self, body: &Value) {
        write_owner_only(
            &self.record_path(),
            &serde_json::to_vec_pretty(body).unwrap(),
        );
    }

    /// The fixture's state over this table, reading these records. Minting
    /// stays off, its shipped default (#10459 PR1).
    fn state(&self, fx: &Fixture) -> State {
        let (mut state, _grants) = state_with(fx, Arc::clone(&self.procs));
        state.architect_records = Some(self.records.clone());
        state.tm_bin_dir = Some(self.bin.clone());
        state
    }

    /// [`Self::state`] with minting on and a ticket store on a fake clock
    /// at [`MINT_AT`].
    fn minting(&self, fx: &Fixture) -> (State, Arc<FakeClock>) {
        let mut state = self.state(fx);
        state.settings.mint_enabled = true;
        let clock = Arc::new(FakeClock::at(MINT_AT));
        state.tickets = Arc::new(TicketStore::new(Arc::clone(&clock) as Arc<dyn Clock>));
        (state, clock)
    }

    /// [`Self::minting`] holding a ticket for `op` on `name`.
    fn ticketed(&self, fx: &Fixture, op: &str, name: &str) -> (State, Arc<FakeClock>) {
        let (state, clock) = self.minting(fx);
        let minted = mint(&state, fx, op, name, &CALL_ARGV);
        assert!(minted.is_ok(), "{minted:?}");
        (state, clock)
    }

    /// The fake `tm` the hook runs.
    fn tm(&self) -> PathBuf {
        self.bin.join("tm")
    }
}

/// `secrets.ticket.mint` params for `op` on `name`, hashing `argv`.
fn mint_params(fx: &Fixture, op: &str, name: &str, argv: &[&str]) -> Value {
    let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
    json!({"project": fx.project(), "session_id": SESSION, "prompt_id": "prompt-7",
        "tool_use_id": "toolu_01AbC", "kind": "secrets_op",
        "argv_sha256": argv_sha256(&argv), "op": op, "key": name})
}

/// Mint from the fake hook, directly through the method body.
fn mint(
    state: &State,
    fx: &Fixture,
    op: &str,
    name: &str,
    argv: &[&str],
) -> Result<Value, ErrorKind> {
    ticket_methods::mint(
        state,
        Caller::new(Some(HOOK)),
        mint_params(fx, op, name, argv),
    )
}

/// Every record of `method`.
fn records_of(fx: &Fixture, method: AuditMethod) -> Vec<AuditRecord> {
    records(fx)
        .into_iter()
        .filter(|r| r.method == method)
        .collect()
}

fn write_owner_only(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// A valid Architect: `1 -> 10 (claude, recorded) -> me`.
fn architect(fx: &Fixture) -> Architect {
    let records = fx.tmp.path().join("home/.trusty-mpm/architect-launch");
    std::fs::create_dir_all(&records).unwrap();
    let project = fx.tmp.path().join("architect-project");
    std::fs::create_dir_all(&project).unwrap();
    let bin = fx.tmp.path().join("home/.cargo/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(bin.join("tm"), b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(bin.join("tm"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let procs = procs_with_me();
    procs.set_agent(ARCHITECT);
    procs.set_launch_facts(ARCHITECT, LAUNCHED, &project);
    // #10459 PR1: the hook, fresh at mint; this process, started after it.
    let tm = bin.join("tm");
    procs.add(HOOK, ARCHITECT, 150);
    procs.set_executable(HOOK, tm.to_str().unwrap());
    procs.set_argv(HOOK, &[tm.to_str().unwrap(), "hook", "--pm-guard"]);
    procs.set_launch_facts(HOOK, MINT_AT, &project);
    procs.set_launch_facts(me(), MINT_AT + 1, &project);
    procs.set_argv(me(), &CALL_ARGV);
    let architect = Architect {
        records,
        project,
        bin,
        procs,
    };
    architect.write(&Architect::body(ARCHITECT, LAUNCHED, &architect.project));
    architect
}

fn set_to(fx: &Fixture, name: &str, value: &str) -> Value {
    json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": name, "value": value})
}

/// The stored value of `name` equals `value`.
fn stored(fx: &Fixture, name: &str, value: &str) -> bool {
    fx.keychain
        .get(&vault(PROJECT_VAULT), &key(name))
        .unwrap()
        .is_some_and(|v| v.expose() == value)
}

/// Whether `backend` holds an agent deny on `name`.
fn denied_in(backend: &MemoryBackend, name: &str) -> bool {
    backend
        .control_item(
            ControlItem::AgentDeny,
            &vault(PROJECT_VAULT),
            Some(&key(name)),
        )
        .unwrap()
}

/// Why: #10459 P1 regression — the launched Architect stores a new value
/// for a key the operator denied to agents. The deny stays, the reply shows
/// the length only, and the one allow record says `architect: true` and
/// holds no value. Red on c1c8e9a36a: `agent_key_denied`. #10459 PR1: only
/// with a ticket minted for this call; its mint and redeem records precede
/// the write.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn architect_with_a_valid_record_sets_a_denied_key() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    let tickets = Arc::clone(&state.tickets);
    let server = fx.start_state(state).await;
    let answer = call(
        &fx.settings.socket,
        method::SET,
        set_to(&fx, "API_KEY", OPEN_VALUE),
    )
    .await;
    server.stop().await;

    let answer = ok(answer);
    assert_eq!(answer["outcome"], "updated");
    let masked = format!("[{} chars]", OPEN_VALUE.chars().count());
    assert_eq!(answer["masked"], masked.as_str());
    assert!(
        stored(&fx, "API_KEY", OPEN_VALUE),
        "the new value was stored"
    );
    assert!(denied_in(&fx.keychain, "API_KEY"), "the deny stays");
    let order: Vec<_> = records(&fx).iter().map(|r| r.method).collect();
    assert_eq!(
        order,
        [
            AuditMethod::TicketMint,
            AuditMethod::TicketRedeem,
            AuditMethod::Set
        ]
    );
    assert!(!tickets.locked(|t| t.pending(None)).unwrap(), "spent");
    let all = records_of(&fx, AuditMethod::Set);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::Set,
            AuditDecision::Allow,
            None,
            Some("API_KEY")
        )
    );
    assert_eq!(all[0].agent_parent, Some(true));
    assert_eq!(all[0].architect, Some(true));
    assert_eq!(all[0].default_policy, None);
    assert_eq!(all[0].caller_pid, Some(me()));
    let raw = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(raw.contains("\"architect\":true"), "{raw}");
    assert!(!raw.contains(OPEN_VALUE) && !raw.contains(SENTINEL_VALUE));
}

/// Why: #10459 P1 — the Architect's local copy moves a denied key, and its
/// deny travels with it; the copy record and the carried deny's record both
/// say `architect: true`. Red on c1c8e9a36a: the key lands in `failed`.
/// #10459 PR1: with a ticket minted for this one-key copy.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn architect_copies_a_denied_key_locally() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "copy", "API_KEY");
    let server = fx.start_state(state).await;
    let params = json!({"project": fx.project(), "from_backend": "keychain",
        "to_backend": "spare", "keys": ["API_KEY"]});
    let answer = call(&fx.settings.socket, method::COPY, params).await;
    server.stop().await;

    assert_eq!(ok(answer), json!({"copied": ["API_KEY"], "failed": []}));
    assert!(denied_in(&fx.spare, "API_KEY"), "the deny was carried");
    let marks: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.ticket.is_none())
        .map(|r| (r.method, r.decision, r.architect))
        .collect();
    assert_eq!(
        marks,
        [
            (
                AuditMethod::SetAgentsMayUse,
                AuditDecision::Allow,
                Some(true)
            ),
            (AuditMethod::Copy, AuditDecision::Allow, Some(true)),
        ]
    );
    let raw = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!raw.contains(SENTINEL_VALUE));
}

/// One way the Architect check can fail, applied to a valid Architect.
type Break = fn(&Fixture, &mut Architect, &mut State);

/// Why: #10459 P1 Fail-Open Check — each failed check keeps the agent
/// rules: the denied key is `agent_key_denied`, its value unchanged, and no
/// record says `architect`. The control row (no break) proves each refusal
/// comes from the broken fact: it sets the key, so the test is red on
/// c1c8e9a36a, which has no exemption. #10459 PR1: every row holds a
/// ticket, so the record check alone decides.
/// Test: itself.
#[test]
fn architect_refusals_each_keep_the_agent_rules() {
    let cases: [(&str, Option<ArchitectRefusal>, Break); 11] = [
        ("control", None, |_, _, _| {}),
        (
            "no record dir",
            Some(ArchitectRefusal::NoRecordDir),
            |_, _, s| {
                s.architect_records = None;
            },
        ),
        (
            "missing record",
            Some(ArchitectRefusal::NoLaunchRecord),
            |_, a, _| {
                std::fs::remove_file(a.record_path()).unwrap();
            },
        ),
        (
            "malformed",
            Some(ArchitectRefusal::UnreadableRecord),
            |_, a, _| {
                write_owner_only(&a.record_path(), b"{not json");
            },
        ),
        (
            "unknown field",
            Some(ArchitectRefusal::UnreadableRecord),
            |_, a, _| {
                let mut body = Architect::body(ARCHITECT, LAUNCHED, &a.project);
                body["profile"] = json!("supervisor");
                a.write(&body);
            },
        ),
        (
            "other-writable",
            Some(ArchitectRefusal::UnreadableRecord),
            |_, a, _| {
                let mode = std::fs::Permissions::from_mode(0o622);
                std::fs::set_permissions(a.record_path(), mode).unwrap();
            },
        ),
        (
            "symlink",
            Some(ArchitectRefusal::UnreadableRecord),
            |fx, a, _| {
                let real = fx.tmp.path().join("planted.architect");
                std::fs::rename(a.record_path(), &real).unwrap();
                std::os::unix::fs::symlink(&real, a.record_path()).unwrap();
            },
        ),
        (
            "pid mismatch",
            Some(ArchitectRefusal::RecordPidMismatch),
            |_, a, _| {
                a.write(&Architect::body(ARCHITECT + 1, LAUNCHED, &a.project));
            },
        ),
        (
            "stale start",
            Some(ArchitectRefusal::StartTimeMismatch),
            |_, a, _| {
                // #10459: the pid was reused by a process started later.
                a.procs
                    .set_launch_facts(ARCHITECT, LAUNCHED + 7, &a.project);
            },
        ),
        (
            "other project",
            Some(ArchitectRefusal::OtherProject),
            |fx, a, _| {
                let other = fx.tmp.path().join("other-project");
                std::fs::create_dir_all(&other).unwrap();
                a.write(&Architect::body(ARCHITECT, LAUNCHED, &other));
            },
        ),
        (
            "unreadable launch facts",
            Some(ArchitectRefusal::ProcessLookup),
            |_, a, _| {
                let procs = procs_with_me();
                procs.set_agent(ARCHITECT);
                a.procs = procs;
            },
        ),
    ];
    for (name, refusal, apply) in cases {
        let fx = fixture();
        seed(&fx, "API_KEY", SENTINEL_VALUE, false);
        let mut arch = architect(&fx);
        let (mut state, _clock) = arch.ticketed(&fx, "set", "API_KEY");
        apply(&fx, &mut arch, &mut state);
        // A swapped table must reach the state too.
        state.grants = state_with(&fx, Arc::clone(&arch.procs)).1;
        let verdict = check_architect(
            arch.procs.as_ref(),
            state.architect_records.as_deref(),
            me(),
        );
        assert_eq!(verdict.err(), refusal, "{name}");
        let set = methods::set(
            &state,
            Caller::new(Some(me())),
            set_to(&fx, "API_KEY", OPEN_VALUE),
        );
        let all = records_of(&fx, AuditMethod::Set);
        match refusal {
            None => {
                assert!(set.is_ok(), "{name}: {set:?}");
                assert_eq!(all[0].architect, Some(true), "{name}");
                assert_eq!(all[0].architect_refusal, None, "{name}");
            }
            Some(refused) => {
                assert_eq!(set, Err(ErrorKind::AgentKeyDenied), "{name}");
                assert!(stored(&fx, "API_KEY", SENTINEL_VALUE), "{name}: unchanged");
                assert!(all.iter().all(|r| r.architect.is_none()), "{name}: {all:?}");
                // #10459: the reason is audited once a record directory is known.
                let audited = state.architect_records.as_ref().map(|_| refused);
                assert!(
                    all.iter().all(|r| r.architect_refusal == audited),
                    "{name}: {all:?}"
                );
            }
        }
        if name == "stale start" {
            let raw = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
            assert!(
                raw.contains("\"architect_refusal\":\"start_time_mismatch\""),
                "{raw}"
            );
            assert!(!raw.contains(&arch.records.display().to_string()), "{raw}");
        }
    }
}

/// Why: #10459 P1 Fail-Open Check — with a valid record but an unreadable
/// process chain the caller is not judged at all: `grant_refused`, nothing
/// written, today's agent behaviour.
/// Test: itself.
#[test]
fn architect_with_an_unreadable_process_chain_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    let state = arch.state(&fx);
    arch.procs.set_unreadable();
    let set = methods::set(
        &state,
        Caller::new(Some(me())),
        set_to(&fx, "API_KEY", OPEN_VALUE),
    );
    assert_eq!(set, Err(ErrorKind::GrantRefused));
    assert!(stored(&fx, "API_KEY", SENTINEL_VALUE), "unchanged");
    assert!(records(&fx).iter().all(|r| r.architect.is_none()));
}

/// Why: #10459 P1 — a PM or subagent session started under the Architect's
/// `claude` is its own nearest `claude` 20, which has no record, so it keeps
/// the agent rules even though `claude` 10 above it is the Architect.
/// Test: itself.
#[test]
fn nested_claude_under_the_architect_is_not_the_architect() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    let procs = FakeProcs::default();
    procs
        .add(1, 0, 1)
        .add(ARCHITECT, 1, 100)
        .add(20, ARCHITECT, 200)
        .add(me(), 20, 4242);
    procs.set_agent(ARCHITECT);
    procs.set_agent(20);
    procs.set_launch_facts(ARCHITECT, LAUNCHED, &arch.project);
    procs.set_launch_facts(20, LAUNCHED, &arch.project);
    let procs = Arc::new(procs);
    let (mut state, _grants) = state_with(&fx, Arc::clone(&procs));
    state.architect_records = Some(arch.records.clone());

    let verdict = check_architect(procs.as_ref(), Some(&arch.records), me());
    assert_eq!(verdict, Err(ArchitectRefusal::NoLaunchRecord));
    let set = methods::set(
        &state,
        Caller::new(Some(me())),
        set_to(&fx, "API_KEY", OPEN_VALUE),
    );
    assert_eq!(set, Err(ErrorKind::AgentKeyDenied));
    assert!(stored(&fx, "API_KEY", SENTINEL_VALUE), "unchanged");
    let refusals: Vec<_> = records(&fx).iter().map(|r| r.architect_refusal).collect();
    assert_eq!(refusals, [Some(ArchitectRefusal::NoLaunchRecord)]);
}

/// Why: #10459 critic LOW — the `writer_for` bridge arm itself. A verdict
/// as the verified console states it (`mapped`, agent) over a peer that
/// passes `check_architect` keeps the agent rules: `set` of a denied key is
/// `agent_key_denied`. Red when the bridge arm is removed (the peer then
/// passes as the Architect and the key is written). Over the wire a console
/// under Claude Code is `bridge_refused` first, so the verdict is built here.
/// #10459 PR1: a ticket is pending, so only the bridge arm refuses.
/// Test: itself.
#[test]
fn verified_bridge_call_over_an_architect_peer_keeps_the_agent_rules() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    let (state, _clock) = arch.ticketed(&fx, "set", "API_KEY");
    assert_eq!(
        check_architect(arch.procs.as_ref(), Some(&arch.records), me()),
        Ok(ARCHITECT)
    );
    let caller = Caller::new(Some(me())).with_bridge(BridgeVerdict::mapped(true));
    let set = methods::set(&state, caller, set_to(&fx, "API_KEY", OPEN_VALUE));
    assert_eq!(set, Err(ErrorKind::AgentKeyDenied));
    assert!(stored(&fx, "API_KEY", SENTINEL_VALUE), "unchanged");
    assert!(records(&fx).iter().all(|r| r.architect.is_none()));
}

/// Why: #10459 P1 scope — `delete`, deny removal and a sync-target push stay
/// operator-only for the Architect too (a separate follow-up covers them),
/// and none of their records says `architect`. #10459 PR1: a pending
/// ticket changes none of them.
/// Test: itself.
#[test]
fn architect_delete_deny_removal_and_sync_push_stay_operator_only() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    let (state, _target, opens) = with_fake_target(arch.ticketed(&fx, "set", "API_KEY").0);
    let caller = Caller::new(Some(me()));
    let target = json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": "API_KEY"});
    let deleted = methods::delete(&state, caller, target.clone());
    let mut lift = target;
    lift["allowed"] = json!(true);
    let lifted = set_agents_may_use(&state, caller, lift);
    let pushed = methods::copy(
        &state,
        caller,
        push_params(&fx, "github-actions", &["API_KEY"]),
    );

    assert_eq!(deleted, Err(ErrorKind::AgentUseRefused));
    assert_eq!(lifted, Err(ErrorKind::AgentUseRefused));
    assert_eq!(pushed, Err(ErrorKind::AgentUseRefused));
    assert_eq!(opens.load(Ordering::SeqCst), 0, "no target opened");
    assert!(stored(&fx, "API_KEY", SENTINEL_VALUE), "the value stays");
    assert!(denied_in(&fx.keychain, "API_KEY"), "the deny stays");
    let all: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.ticket.is_none())
        .collect();
    assert_eq!(all.len(), 3, "{all:?}");
    assert!(all.iter().all(|r| r.architect.is_none()), "{all:?}");
}

/// Why: #10459 P1 — a console bridge call's peer is the console, never the
/// client it serves. A console running under the Architect's `claude` is not
/// a verified console at all (#9066), so the bridged `set` of a denied key is
/// `bridge_refused` and nothing is written, though the direct check on the
/// same peer passes.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_call_from_under_the_architect_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    arch.procs
        .set_executable(me(), "/opt/trusty/bin/trusty-console");
    assert_eq!(
        check_architect(arch.procs.as_ref(), Some(&arch.records), me()),
        Ok(ARCHITECT)
    );
    let server = fx.start_state(arch.ticketed(&fx, "set", "API_KEY").0).await;
    let mut params = set_to(&fx, "API_KEY", OPEN_VALUE);
    params["bridge"] = json!({"agent_parent": true, "source": "mapped"});
    let refused = call(&fx.settings.socket, "secrets.bridge.set", params).await;
    server.stop().await;

    assert_eq!(fixed_error(&refused, method::SET), ErrorKind::BridgeRefused);
    assert!(stored(&fx, "API_KEY", SENTINEL_VALUE), "unchanged");
    assert!(records(&fx).iter().all(|r| r.architect.is_none()));
}
