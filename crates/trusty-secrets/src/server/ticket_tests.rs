//! Main-thread tickets: mint checks, voids, redemption and every
//! fail-closed arm (#10459 PR1, DOC-86 §2, §6, §8 PR 1).
//!
//! A child of `architect_tests`, so it shares the fake Architect `claude` 10,
//! the fake hook 15 and this process as the redeemer. Calls go straight to
//! the method bodies; the peer pid is the [`Caller`] each test names.
//!
//! Test: itself.

use super::*;
use crate::server::ticket::{TicketEvent, TicketRefusal, VoidCause};

/// `set` of the denied `API_KEY` by this process, directly.
fn set_denied(fx: &Fixture, state: &State) -> Result<Value, ErrorKind> {
    methods::set(
        state,
        Caller::new(Some(me())),
        set_to(fx, "API_KEY", OPEN_VALUE),
    )
}

/// `secrets.ticket.void` of [`SESSION`] with `cause`.
fn void(state: &State, cause: &str) -> Result<Value, ErrorKind> {
    ticket_methods::void(
        state,
        Caller::new(Some(HOOK)),
        json!({"session_id": SESSION, "cause": cause}),
    )
}

/// Whether any ticket is pending, over the wire method.
fn pending(state: &State) -> bool {
    let answer = ticket_methods::pending(state, Caller::new(Some(HOOK)), json!({}));
    answer.expect("pending answers")["pending"] == json!(true)
}

/// The agent rules held: `agent_key_denied`, the value unchanged, the set
/// record naming `refusal` and never `architect`.
fn kept_agent_rules(fx: &Fixture, set: Result<Value, ErrorKind>, refusal: ArchitectRefusal) {
    assert_eq!(set, Err(ErrorKind::AgentKeyDenied));
    assert!(stored(fx, "API_KEY", SENTINEL_VALUE), "unchanged");
    let sets = records_of(fx, AuditMethod::Set);
    assert!(sets.iter().all(|r| r.architect.is_none()), "{sets:?}");
    let refusals: Vec<_> = sets.iter().map(|r| r.architect_refusal).collect();
    assert_eq!(refusals, [Some(refusal)]);
}

/// The `reason_arm` of the last mint refusal.
fn last_refusal(fx: &Fixture) -> Option<TicketRefusal> {
    let mints = records_of(fx, AuditMethod::TicketMint);
    let last = mints.last().expect("a mint record");
    assert_eq!(last.decision, AuditDecision::Deny);
    let ticket = last.ticket.as_ref().expect("its ticket object");
    assert_eq!(ticket.event, TicketEvent::Refuse);
    ticket.reason_arm
}

/// The void causes on record, in order.
fn void_causes(fx: &Fixture) -> Vec<Option<VoidCause>> {
    records_of(fx, AuditMethod::TicketVoid)
        .iter()
        .map(|r| r.ticket.as_ref().and_then(|t| t.void_cause))
        .collect()
}

/// Why: DOC-86 §2 — minting ships off, so no ticket exists and the
/// Architect keeps today's agent rules; the refusal is audited. Red before
/// the switch existed: the mint succeeded.
/// Test: itself.
#[test]
fn mint_refused_while_mint_disabled() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let state = architect(&fx).state(&fx);
    let minted = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
    assert_eq!(minted, Err(ErrorKind::TicketRefused));
    assert_eq!(last_refusal(&fx), Some(TicketRefusal::MintDisabled));
    assert!(!pending(&state));
    kept_agent_rules(&fx, set_denied(&fx, &state), ArchitectRefusal::NoTicket);
}

/// Why: DOC-86 Q4 — P1's launch record alone no longer opens a denied key.
/// Red on P1 (`ecc179efff`): the set succeeds with the record only.
/// Test: itself.
#[test]
fn record_without_ticket_keeps_agent_rules() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).minting(&fx);
    kept_agent_rules(&fx, set_denied(&fx, &state), ArchitectRefusal::NoTicket);
}

/// Why: DOC-86 Q4 — the record is required at mint and at redemption. A
/// mint under a `claude` with no record is refused, naming the record's
/// check; a ticket whose record is gone by the set keeps the agent rules.
/// Test: itself.
#[test]
fn ticket_without_record_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    let (state, _clock) = arch.minting(&fx);
    std::fs::remove_file(arch.record_path()).unwrap();
    let minted = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
    assert_eq!(minted, Err(ErrorKind::TicketRefused));
    assert_eq!(last_refusal(&fx), Some(TicketRefusal::LaunchRecord));
    let mints = records_of(&fx, AuditMethod::TicketMint);
    assert_eq!(
        mints[0].architect_refusal,
        Some(ArchitectRefusal::NoLaunchRecord)
    );

    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    let (state, _clock) = arch.ticketed(&fx, "set", "API_KEY");
    std::fs::remove_file(arch.record_path()).unwrap();
    kept_agent_rules(
        &fx,
        set_denied(&fx, &state),
        ArchitectRefusal::NoLaunchRecord,
    );
}

/// Why: DOC-86 §2 — the peer must be the installed `tm` running the
/// registered hook argv. Each row breaks one fact; the control row mints.
/// Test: itself.
#[test]
fn mint_from_a_non_tm_exe_is_refused() {
    type Break = fn(&Fixture, &Architect);
    let cases: [(&str, Option<TicketRefusal>, Break); 5] = [
        ("control", None, |_, _| {}),
        (
            "another executable",
            Some(TicketRefusal::NotInstalledTm),
            |_, a| a.procs.set_executable(HOOK, "/usr/bin/python3"),
        ),
        (
            "a tm outside the bin dir",
            Some(TicketRefusal::NotInstalledTm),
            |fx, a| {
                let other = fx.tmp.path().join("tm");
                std::fs::copy(a.tm(), &other).unwrap();
                a.procs.set_executable(HOOK, other.to_str().unwrap());
            },
        ),
        (
            "a group-writable tm",
            Some(TicketRefusal::NotInstalledTm),
            |_, a| {
                let mode = std::fs::Permissions::from_mode(0o775);
                std::fs::set_permissions(a.tm(), mode).unwrap();
            },
        ),
        ("another argv", Some(TicketRefusal::NotHookArgv), |_, a| {
            let tm = a.tm();
            a.procs
                .set_argv(HOOK, &[tm.to_str().unwrap(), "secrets", "set"]);
        }),
    ];
    for (name, refusal, apply) in cases {
        let fx = fixture();
        let arch = architect(&fx);
        let (state, _clock) = arch.minting(&fx);
        apply(&fx, &arch);
        let minted = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
        match refusal {
            None => assert!(minted.is_ok(), "{name}: {minted:?}"),
            Some(arm) => {
                assert_eq!(minted, Err(ErrorKind::TicketRefused), "{name}");
                assert_eq!(last_refusal(&fx), Some(arm), "{name}");
                assert!(!pending(&state), "{name}");
            }
        }
    }
}

/// Why: DOC-86 §2 — exec form makes the hook a direct child of `claude`; a
/// grandchild (a shell, a script) is refused.
/// Test: itself.
#[test]
fn mint_from_a_grandchild_is_refused() {
    let fx = fixture();
    let arch = architect(&fx);
    arch.procs.add(16, ARCHITECT, 160);
    arch.procs.add(HOOK, 16, 150);
    let (state, _clock) = arch.minting(&fx);
    let minted = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
    assert_eq!(minted, Err(ErrorKind::TicketRefused));
    assert_eq!(last_refusal(&fx), Some(TicketRefusal::ParentNotClaude));
}

/// Why: DOC-86 §2 — the minting peer started at most 2 s ago; an older one
/// is a reused or long-lived process.
/// Test: itself.
#[test]
fn mint_from_an_old_process_is_refused() {
    let fx = fixture();
    let arch = architect(&fx);
    arch.procs
        .set_launch_facts(HOOK, MINT_AT - 3, &arch.project);
    let (state, _clock) = arch.minting(&fx);
    let minted = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
    assert_eq!(minted, Err(ErrorKind::TicketRefused));
    assert_eq!(last_refusal(&fx), Some(TicketRefusal::PeerTooOld));
}

/// Why: DOC-86 §2 — a ticket is one-use: the second set of the same call
/// keeps the agent rules.
/// Test: itself.
#[test]
fn ticket_replay_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    assert!(set_denied(&fx, &state).is_ok());
    let replay = methods::set(
        &state,
        Caller::new(Some(me())),
        set_to(&fx, "API_KEY", SENTINEL_VALUE),
    );
    assert_eq!(replay, Err(ErrorKind::AgentKeyDenied));
    assert!(
        stored(&fx, "API_KEY", OPEN_VALUE),
        "the replay wrote nothing"
    );
    let refusals: Vec<_> = records_of(&fx, AuditMethod::Set)
        .iter()
        .map(|r| r.architect_refusal)
        .collect();
    assert_eq!(refusals, [None, Some(ArchitectRefusal::NoTicket)]);
}

/// Why: DOC-86 §2 — the redeemer's argv must hash to the ticket's; a
/// mismatched attempt voids the ticket.
/// Test: itself.
#[test]
fn ticket_argv_mismatch_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).minting(&fx);
    let other = ["tm", "secrets", "set", "OTHER"];
    assert!(mint(&state, &fx, "set", "API_KEY", &other).is_ok());
    kept_agent_rules(
        &fx,
        set_denied(&fx, &state),
        ArchitectRefusal::TicketArgvMismatch,
    );
    assert_eq!(void_causes(&fx), [Some(VoidCause::Mismatch)]);
    assert!(!pending(&state));
}

/// Why: DOC-86 §2 — a redeemer counts only if it started after the mint;
/// the same whole second refuses, fail closed.
/// Test: itself.
#[test]
fn ticket_peer_started_before_mint_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    let (state, _clock) = arch.ticketed(&fx, "set", "API_KEY");
    arch.procs.set_launch_facts(me(), MINT_AT, &arch.project);
    kept_agent_rules(
        &fx,
        set_denied(&fx, &state),
        ArchitectRefusal::TicketPeerStartedBeforeMint,
    );
}

/// Why: DOC-86 §2 — a ticket names one op, project and key.
/// Test: itself.
#[test]
fn ticket_for_another_key_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "OTHER_KEY");
    kept_agent_rules(
        &fx,
        set_denied(&fx, &state),
        ArchitectRefusal::TicketOtherCall,
    );
}

/// Why: DOC-86 §2 — every PreToolUse in the session voids a pending
/// ticket, so the next tool call closes the window.
/// Test: itself.
#[test]
fn next_event_voids_a_pending_ticket() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    assert_eq!(void(&state, "pre_tool_use"), Ok(json!({"voided": 1})));
    assert!(!pending(&state));
    assert_eq!(void_causes(&fx), [Some(VoidCause::PreToolUse)]);
    kept_agent_rules(&fx, set_denied(&fx, &state), ArchitectRefusal::NoTicket);
    assert_eq!(void(&state, "pre_tool_use"), Ok(json!({"voided": 0})));
    assert_eq!(void_causes(&fx).len(), 1, "a no-op void writes nothing");
}

/// Why: DOC-86 §2 (HIGH if missed) — a subagent's or teammate's PreToolUse
/// voids the session's ticket too; a main-thread-only void would leave it
/// open to the subagent's call.
/// Test: itself.
#[test]
fn subagent_pretooluse_voids_pending_ticket() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    assert_eq!(
        void(&state, "subagent_pre_tool_use"),
        Ok(json!({"voided": 1}))
    );
    assert_eq!(void_causes(&fx), [Some(VoidCause::SubagentPreToolUse)]);
    kept_agent_rules(&fx, set_denied(&fx, &state), ArchitectRefusal::NoTicket);
}

/// Why: DOC-86 §2 — a declined `ask` leaves the ticket unredeemed; the
/// session's Stop voids it. A hook may not send a server-only cause.
/// Test: itself.
#[test]
fn decline_then_stop_voids() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    assert_eq!(void(&state, "ttl"), Err(ErrorKind::InvalidParams));
    assert!(pending(&state), "a refused void changes nothing");
    assert_eq!(void(&state, "stop"), Ok(json!({"voided": 1})));
    assert!(!pending(&state));
    kept_agent_rules(&fx, set_denied(&fx, &state), ArchitectRefusal::NoTicket);
}

/// Why: DOC-86 §2 — a ticket lives 60 s; an expired one is voided, with a
/// record, and redeems nothing.
/// Test: itself.
#[test]
fn ttl_expiry_voids() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    clock.set(MINT_AT + 59);
    assert!(pending(&state), "live just before the TTL");
    clock.set(MINT_AT + 60);
    assert!(!pending(&state));
    assert_eq!(void_causes(&fx), [Some(VoidCause::Ttl)]);
    kept_agent_rules(&fx, set_denied(&fx, &state), ArchitectRefusal::NoTicket);
}

/// Why: DOC-86 §8 — a second mint in the session voids the pending ticket
/// and refuses itself, so neither is live.
/// Test: itself.
#[test]
fn second_mint_voids_both() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    let again = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
    assert_eq!(again, Err(ErrorKind::TicketRefused));
    assert_eq!(last_refusal(&fx), Some(TicketRefusal::Pending));
    assert_eq!(void_causes(&fx), [Some(VoidCause::SecondMint)]);
    assert!(!pending(&state));
    kept_agent_rules(&fx, set_denied(&fx, &state), ArchitectRefusal::NoTicket);
}

/// Give the audit directory a mode the sink refuses; the guard restores it.
fn break_audit(fx: &Fixture) -> RestoreMode {
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    RestoreMode(dir)
}

/// Why: DOC-86 §6 — the mint record is written ahead; with the log
/// unwritable no ticket is stored.
/// Test: itself.
#[test]
fn audit_unwritable_at_mint_refuses() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).minting(&fx);
    let restore = break_audit(&fx);
    let minted = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
    assert_eq!(minted, Err(ErrorKind::AuditUnavailable));
    drop(restore);
    assert!(!pending(&state), "no record, no ticket");
}

/// Why: DOC-86 §6 — the redeem record precedes the write it allows; with
/// the log unwritable the set writes nothing and the ticket is spent.
/// Test: itself.
#[test]
fn audit_unwritable_at_redeem_refuses() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    let restore = break_audit(&fx);
    assert_eq!(set_denied(&fx, &state), Err(ErrorKind::AuditUnavailable));
    drop(restore);
    assert!(stored(&fx, "API_KEY", SENTINEL_VALUE), "unchanged");
    assert!(!pending(&state), "spent, fail closed");
}

/// Why: DOC-86 §5, §6 — "no ticket" is the closed state, so a void whose
/// record fails still removes the ticket and answers `audit_unavailable`.
/// That the hook then does not deny the call is PR 5's
/// `failed_void_does_not_deny_the_call`.
/// Test: itself.
#[test]
fn audit_unwritable_at_void_refuses() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    let restore = break_audit(&fx);
    assert_eq!(
        void(&state, "pre_tool_use"),
        Err(ErrorKind::AuditUnavailable)
    );
    drop(restore);
    assert!(!pending(&state), "removed though unrecorded");
    kept_agent_rules(&fx, set_denied(&fx, &state), ArchitectRefusal::NoTicket);
}

/// Why: DOC-86 §6 — a process-table error at mint refuses, never guesses.
/// Test: itself.
#[test]
fn process_table_error_at_mint_refuses() {
    let fx = fixture();
    let arch = architect(&fx);
    let (state, _clock) = arch.minting(&fx);
    arch.procs.set_unreadable();
    let minted = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
    assert_eq!(minted, Err(ErrorKind::TicketRefused));
    assert_eq!(last_refusal(&fx), Some(TicketRefusal::ProcessLookup));
}

/// Why: DOC-86 §6 — a clock error refuses at mint, and at redemption keeps
/// the agent rules.
/// Test: itself.
#[test]
fn clock_error_refuses() {
    let fx = fixture();
    let (state, clock) = architect(&fx).minting(&fx);
    clock.fail();
    let minted = mint(&state, &fx, "set", "API_KEY", &CALL_ARGV);
    assert_eq!(minted, Err(ErrorKind::TicketRefused));
    assert_eq!(last_refusal(&fx), Some(TicketRefusal::Clock));

    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    clock.fail();
    kept_agent_rules(
        &fx,
        set_denied(&fx, &state),
        ArchitectRefusal::TicketUnavailable,
    );
}

/// Why: DOC-86 §2 — tickets live in daemon memory only, so a new server
/// holds none.
/// Test: itself.
#[test]
fn restart_drops_tickets() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let arch = architect(&fx);
    let (before, _clock) = arch.ticketed(&fx, "set", "API_KEY");
    assert!(pending(&before));
    let (after, _clock) = arch.minting(&fx);
    assert!(!pending(&after));
    kept_agent_rules(&fx, set_denied(&fx, &after), ArchitectRefusal::NoTicket);
}

/// Why: the argv hash must not let `["ab"]` stand for `["a", "b"]`.
/// Test: itself.
#[test]
fn argv_hash_separates_arguments() {
    let joined = argv_sha256(&["ab".to_owned()]);
    let split = argv_sha256(&["a".to_owned(), "b".to_owned()]);
    assert_ne!(joined, split);
    assert_eq!(joined.len(), 64);
}

/// Why: DOC-86 §6 — a ticket record holds ids, the argv hash and the key
/// name, never a value or the argv, and reads back.
/// Test: itself.
#[test]
fn ticket_records_carry_no_value_and_round_trip() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _clock) = architect(&fx).ticketed(&fx, "set", "API_KEY");
    assert!(set_denied(&fx, &state).is_ok());
    let raw = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!raw.contains(OPEN_VALUE) && !raw.contains(SENTINEL_VALUE));
    assert!(!raw.contains("secrets set"), "no argv: {raw}");
    let redeem = records_of(&fx, AuditMethod::TicketRedeem);
    let ticket = redeem[0].ticket.as_ref().expect("ticket object");
    assert_eq!(ticket.event, TicketEvent::Redeem);
    assert_eq!(ticket.session_id.as_deref(), Some(SESSION));
    assert_eq!(ticket.claude_pid, Some(ARCHITECT));
    assert_eq!(
        redeem[0].key.as_ref().map(SecretKey::as_str),
        Some("API_KEY")
    );
    let line = serde_json::to_string(&redeem[0]).unwrap();
    let back: AuditRecord = serde_json::from_str(&line).unwrap();
    assert_eq!(back, redeem[0]);
}
