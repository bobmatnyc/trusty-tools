//! Tests for the #4470 pre-bootstrap foreign-port guard.
//!
//! Why a sibling file: `port_guard.rs` is production source under the 500-SLOC
//! cap; test bodies belong in a `_tests.rs` sibling (the 3000-SLOC test cap),
//! mirroring `plist_bootstrap.rs` / `plist_bootstrap_tests.rs`.
//!
//! What: the full [`super::decide`] truth table, the `lsof` field parser, the
//! port-resolution precedence, and the structural invariant that every
//! launchd-managed stable-set member has a port the guard can actually check.

use super::*;
use crate::commands::stable_set::{stable_set, ManageStrategy};

/// A binary name used throughout; any stable-set daemon would do.
const MEMBER: &str = "trusty-search";

// ── decide: the proceed cases ───────────────────────────────────────────────

/// Why: the overwhelmingly common case — nothing is listening, so there is
/// nothing for the bootstrap to collide with. A guard that refused here would
/// break every ordinary install.
/// What: a `Free` port proceeds regardless of what launchd reports, including
/// the `Unavailable` state that is otherwise fail-closed.
/// Test: This is the test.
#[test]
fn decide_free_port_proceeds() {
    for owner in [
        LaunchdOwner::Running(10),
        LaunchdOwner::NotRunning,
        LaunchdOwner::Unavailable,
    ] {
        assert_eq!(
            decide(MEMBER, 7878, &PortHolder::Free, owner, false),
            PortVerdict::Proceed,
            "free port must proceed for {owner:?}"
        );
    }
}

/// Why: re-bootstrapping a daemon launchd already runs is the ordinary
/// idempotent path (`tctl install` over a live stack, `tctl restart`). The
/// guard must recognise its OWN supervised daemon holding the port and let it
/// through, or it would block every legitimate restart.
/// What: holder PID == launchd's PID → `Proceed`.
/// Test: This is the test.
#[test]
fn decide_supervised_holder_proceeds() {
    assert_eq!(
        decide(
            MEMBER,
            7878,
            &PortHolder::Held(4242),
            LaunchdOwner::Running(4242),
            false
        ),
        PortVerdict::Proceed
    );
}

// ── decide: the rejection cases ─────────────────────────────────────────────

/// Why: THE #4470 defect — an unsupervised process holding the daemon's port
/// while launchd runs nothing for the label. `launchctl bootstrap` exits 0 here
/// and the orphan keeps serving the old binary, so a signed install plus
/// behavioural verification can all pass while shipping nothing (#4230).
/// What: asserts a `Reject` whose message names the offending PID, the port,
/// the `kill` recipe, and the override — i.e. that it is actionable, not just
/// negative. Deleting the guard makes `bootstrap_one` return `Installed` here
/// instead (see `service_bootstrap_tests`).
/// Test: This is the test.
#[test]
fn decide_rejects_orphan_holder_launchd_does_not_run() {
    let verdict = decide(
        MEMBER,
        7878,
        &PortHolder::Held(9931),
        LaunchdOwner::NotRunning,
        false,
    );
    let PortVerdict::Reject(msg) = verdict else {
        panic!("expected Reject, got {verdict:?}");
    };
    assert!(msg.contains("9931"), "must name the offending pid: {msg}");
    assert!(msg.contains("7878"), "must name the port: {msg}");
    assert!(msg.contains("kill -TERM 9931"), "must be actionable: {msg}");
    assert!(
        msg.contains(ALLOW_FOREIGN_PORT_ENV),
        "must name the override: {msg}"
    );
    assert!(msg.contains(MEMBER), "must name the member: {msg}");
}

/// Why: launchd running the label as one PID while a DIFFERENT PID holds the
/// port means two processes are claiming one daemon. Whichever one answers
/// `/health` is a coin flip, which is exactly the unsound verification #4470
/// exists to stop.
/// What: asserts a `Reject` naming BOTH pids, so the operator can tell the
/// supervised one from the squatter.
/// Test: This is the test.
#[test]
fn decide_rejects_foreign_pid_while_launchd_runs_another() {
    let verdict = decide(
        MEMBER,
        7878,
        &PortHolder::Held(555),
        LaunchdOwner::Running(4242),
        false,
    );
    let PortVerdict::Reject(msg) = verdict else {
        panic!("expected Reject, got {verdict:?}");
    };
    assert!(msg.contains("555"), "must name the holder pid: {msg}");
    assert!(msg.contains("4242"), "must name launchd's pid: {msg}");
}

/// Why: `Unavailable` means the ownership question could not be ANSWERED, not
/// that the answer was "nobody". Treating an unanswerable query as clearance
/// would let the exact orphan state through on any host where `launchctl` is
/// unreachable — the fail-open shape.
/// What: a held port plus an unanswerable launchd query rejects.
/// Test: This is the test.
#[test]
fn decide_rejects_when_launchd_cannot_be_asked() {
    let verdict = decide(
        MEMBER,
        7878,
        &PortHolder::Held(4242),
        LaunchdOwner::Unavailable,
        false,
    );
    assert!(
        matches!(verdict, PortVerdict::Reject(_)),
        "an unverifiable holder must not be treated as legitimate: {verdict:?}"
    );
}

/// Why: THE fail-closed core. If the port probe itself did not work, a foreign
/// holder has not been ruled out, and "we could not check" must never advance
/// to a bootstrap. This is the branch the repository's recurring fail-open /
/// cursor-advance bug shape would silently turn into a pass.
/// What: `Unknown` rejects for EVERY launchd state, including
/// `Running(pid)` — a supervised daemon existing somewhere does not prove it is
/// the thing on this port.
/// Test: This is the test.
#[test]
fn decide_rejects_unreadable_probe() {
    for owner in [
        LaunchdOwner::Running(4242),
        LaunchdOwner::NotRunning,
        LaunchdOwner::Unavailable,
    ] {
        let verdict = decide(
            MEMBER,
            7878,
            &PortHolder::Unknown("lsof not found".to_owned()),
            owner,
            false,
        );
        let PortVerdict::Reject(msg) = verdict else {
            panic!("an unreadable probe must reject for {owner:?}, got {verdict:?}");
        };
        assert!(msg.contains("lsof not found"), "must relay why: {msg}");
    }
}

/// Why: a compact invariant over the whole table — the ONLY way a non-free port
/// may proceed is the supervised daemon holding it itself. Any future branch
/// that leaks a `Proceed` into another cell fails here even if nobody remembers
/// to add a dedicated test for it.
/// What: enumerates every holder × owner combination and asserts `Proceed`
/// occurs exactly on `Free` and on the matching-PID cell.
/// Test: This is the test.
#[test]
fn decide_never_proceeds_on_a_held_port() {
    let holders = [
        PortHolder::Free,
        PortHolder::Held(4242),
        PortHolder::Held(555),
        PortHolder::Unknown("probe failed".to_owned()),
    ];
    let owners = [
        LaunchdOwner::Running(4242),
        LaunchdOwner::NotRunning,
        LaunchdOwner::Unavailable,
    ];
    for holder in &holders {
        for owner in owners {
            let proceeded = decide(MEMBER, 7878, holder, owner, false) == PortVerdict::Proceed;
            let should_proceed = match (holder, owner) {
                (PortHolder::Free, _) => true,
                (PortHolder::Held(p), LaunchdOwner::Running(o)) => *p == o,
                _ => false,
            };
            assert_eq!(
                proceeded, should_proceed,
                "unexpected verdict for {holder:?} / {owner:?}"
            );
        }
    }
}

// ── decide: the operator override ───────────────────────────────────────────

/// Why: the guard fails closed, so a host it cannot inspect would otherwise be
/// unrecoverable. The override must convert every refusal — and only refusals —
/// into a loud proceed that still carries the reason.
/// What: asserts each rejecting cell becomes `ProceedOverridden` with the
/// reason preserved when the override is set.
/// Test: This is the test.
#[test]
fn override_downgrades_every_rejection() {
    let cases = [
        (PortHolder::Held(9931), LaunchdOwner::NotRunning),
        (PortHolder::Held(555), LaunchdOwner::Running(4242)),
        (PortHolder::Held(4242), LaunchdOwner::Unavailable),
        (
            PortHolder::Unknown("no lsof".to_owned()),
            LaunchdOwner::NotRunning,
        ),
    ];
    for (holder, owner) in &cases {
        let verdict = decide(MEMBER, 7878, holder, *owner, true);
        let PortVerdict::ProceedOverridden(reason) = verdict else {
            panic!("override must downgrade {holder:?} / {owner:?}, got {verdict:?}");
        };
        assert!(
            !reason.is_empty(),
            "the override must still explain what it bypassed"
        );
    }
}

/// Why: an override is a way THROUGH a refusal, never a source of one. If it
/// could alter a clean verdict it would be a second, hidden policy.
/// What: with the override set, the two proceeding cells still return plain
/// `Proceed` (not `ProceedOverridden`).
/// Test: This is the test.
#[test]
fn override_does_not_manufacture_a_rejection() {
    assert_eq!(
        decide(
            MEMBER,
            7878,
            &PortHolder::Free,
            LaunchdOwner::NotRunning,
            true
        ),
        PortVerdict::Proceed
    );
    assert_eq!(
        decide(
            MEMBER,
            7878,
            &PortHolder::Held(4242),
            LaunchdOwner::Running(4242),
            true
        ),
        PortVerdict::Proceed
    );
}

// ── decide_over_range: the walking-daemon fix ───────────────────────────────

/// Why (#4470 MEDIUM-3): the core of the fix. A walking daemon whose default
/// port is squatted simply takes the next one, so the guard must proceed. Before
/// this, a single unrelated listener on 7070 hard-refused a fresh trusty-memory
/// install that would have succeeded.
/// What: 7070 held by a foreign process, 7071 free → `Proceed`, and the probe
/// stops as soon as it finds the usable port.
/// Test: This is the test.
#[test]
fn decide_over_range_proceeds_when_any_candidate_is_usable() {
    let probed = std::cell::RefCell::new(Vec::new());
    let verdict = decide_over_range(
        "trusty-memory",
        &[7070, 7071, 7072],
        |port| {
            probed.borrow_mut().push(port);
            if port == 7070 {
                (PortHolder::Held(9931), LaunchdOwner::NotRunning)
            } else {
                (PortHolder::Free, LaunchdOwner::NotRunning)
            }
        },
        false,
    );
    assert_eq!(verdict, PortVerdict::Proceed);
    assert_eq!(
        probed.into_inner(),
        vec![7070, 7071],
        "must stop at the first usable port, not probe the whole range"
    );
}

/// Why: the range relaxation must not become a blanket pass. When the daemon
/// genuinely has nowhere to go, the refusal must still fire — and must name the
/// PRIMARY port, since that is the one the operator expects the daemon on.
/// What: every candidate held by a foreign process → `Reject` naming the first
/// port, its pid, and how many candidates were tried.
/// Test: This is the test.
#[test]
fn decide_over_range_refuses_only_when_every_candidate_is_taken() {
    let verdict = decide_over_range(
        "trusty-memory",
        &[7070, 7071],
        |_| (PortHolder::Held(9931), LaunchdOwner::NotRunning),
        false,
    );
    let PortVerdict::Reject(msg) = verdict else {
        panic!("expected Reject, got {verdict:?}");
    };
    assert!(msg.contains("7070"), "must name the primary port: {msg}");
    assert!(msg.contains("9931"), "must name the holder pid: {msg}");
    assert!(
        msg.contains("all 2 candidate ports"),
        "must say the whole range was tried: {msg}"
    );
}

/// Why: THE fail-closed property, carried through the range relaxation. An
/// unreadable probe must never count as a usable port — otherwise widening the
/// guard to a range would have quietly reintroduced the fail-open branch the
/// single-port version was careful to avoid.
/// What: every candidate `Unknown` → `Reject`, never `Proceed`.
/// Test: This is the test.
#[test]
fn decide_over_range_refuses_when_every_candidate_is_unreadable() {
    let verdict = decide_over_range(
        "trusty-memory",
        &[7070, 7071, 7072],
        |_| {
            (
                PortHolder::Unknown("probe failed".to_owned()),
                LaunchdOwner::Running(4242),
            )
        },
        false,
    );
    assert!(
        matches!(verdict, PortVerdict::Reject(_)),
        "an unreadable range must refuse, not proceed: {verdict:?}"
    );
}

// ── the override's value semantics ──────────────────────────────────────────

/// Why (#4470 MEDIUM-2): every refusal message says "set
/// `TCTL_ALLOW_FOREIGN_PORT=1`", which teaches value semantics. Under the
/// original presence check, an operator setting `=0` to turn the bypass OFF
/// turned it ON — disarming a safety guard by trying to arm it.
/// What: only affirmative values enable the bypass; unset, empty, `0`, and
/// `false` all leave the guard armed. Case and surrounding space are ignored.
/// Test: This is the test.
#[test]
fn override_env_value_requires_an_affirmative_value() {
    for on in ["1", "true", "TRUE", "yes", " 1 ", "Yes"] {
        assert!(
            override_from_env_value(Some(on)),
            "{on:?} should enable the bypass"
        );
    }
    for off in ["0", "", "false", "no", "off", " ", "2"] {
        assert!(
            !override_from_env_value(Some(off)),
            "{off:?} must NOT enable a safety bypass"
        );
    }
    assert!(
        !override_from_env_value(None),
        "unset must keep the guard armed"
    );
}

// ── the bind cross-check ────────────────────────────────────────────────────

/// Why (#4470 MEDIUM-1): without root, `lsof` reports only the caller's own
/// processes, so a root-owned or other-user listener produces the same empty
/// output as a free port. Trusting that emptiness classified a foreign holder
/// as `Free` and PROCEEDED — a fail-open branch inside a fail-closed function.
/// The bind cross-check is what distinguishes "nothing there" from "something
/// there I cannot see".
///
/// THE test for this fix. It hands the classifier an EMPTY `lsof` observation
/// for a port that is genuinely occupied — which is exactly what a non-root
/// `lsof` returns for a root-owned or other-user listener. Under the pre-fix
/// code that combination returned `Free` and the guard PROCEEDED past a foreign
/// holder.
///
/// This test fails if the bind cross-check is removed from
/// `probe_port_holder_from`.
///
/// Note on why the observation is injected rather than left to the real `lsof`:
/// a fixture listener the test owns is one `lsof` CAN see, so a test that just
/// binds a port and calls the real probe gets `Held` and never reaches the
/// cross-check at all — it passes identically against the fail-open version.
/// That weaker test was written first here and mutation-testing caught it.
///
/// What: binds a real listener, then asserts an empty `lsof` result classifies
/// as `Unknown` (naming the port and pointing at `sudo lsof`), never `Free`.
/// Test: This is the test.
#[test]
fn empty_lsof_output_on_an_occupied_port_is_unknown_not_free() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fixture listener");
    let port = listener.local_addr().expect("local addr").port();

    let holder = probe_port_holder_from(port, Ok(("", "exit status: 1")));
    assert_ne!(
        holder,
        PortHolder::Free,
        "an occupied port that `lsof` could not see must never classify as Free — \
         that is the non-root blindness this cross-check exists to close"
    );
    let PortHolder::Unknown(why) = holder else {
        panic!("expected Unknown, got {holder:?}");
    };
    assert!(why.contains(&port.to_string()), "must name the port: {why}");
    assert!(why.contains("sudo lsof"), "must be actionable: {why}");
    drop(listener);
}

/// Why: the cross-check must not make the guard refuse everything — a genuinely
/// free port has to stay `Free`, or no install could ever proceed. This is the
/// other half of the invariant, and it is what stops the fix above from being
/// "return Unknown always".
/// What: binds an OS-assigned port to learn a number, releases it, then asserts
/// an empty `lsof` result classifies as `Free`.
/// Test: This is the test.
#[test]
fn empty_lsof_output_on_a_free_port_is_free() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind to learn a free port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);

    assert_eq!(
        probe_port_holder_from(port, Ok(("", "exit status: 1"))),
        PortHolder::Free,
        "a released port must classify as Free or no install could ever proceed"
    );
}

/// Why: the common case — `lsof` names the holder, so the guard can report the
/// pid and the cross-check is unnecessary. Pins that a named pid short-circuits
/// (no bind attempt, no chance of a spurious `Unknown`).
/// What: a `p<pid>` observation classifies as `Held(pid)` even for a port that
/// is actually free.
/// Test: This is the test.
#[test]
fn lsof_naming_a_pid_reports_it_held() {
    assert_eq!(
        probe_port_holder_from(65000, Ok(("p4242\n", "exit status: 0"))),
        PortHolder::Held(4242)
    );
}

/// Why: `lsof` missing from `PATH` or failing to spawn is an unanswerable
/// probe, which the fail-closed contract turns into a refusal — never a pass.
/// What: an `Err` observation classifies as `Unknown` relaying the cause.
/// Test: This is the test.
#[test]
fn lsof_spawn_failure_is_unknown() {
    let holder = probe_port_holder_from(65000, Err("No such file or directory"));
    let PortHolder::Unknown(why) = holder else {
        panic!("expected Unknown, got {holder:?}");
    };
    assert!(why.contains("No such file or directory"), "why: {why}");
}

/// Why: output that exists but yields no pid means the probe ran and could not
/// be understood — indeterminate, so a refusal. Reading it as "free" would be a
/// second fail-open branch alongside the one MEDIUM-1 closed.
/// What: non-empty unparseable output classifies as `Unknown`.
/// Test: This is the test.
#[test]
fn unparseable_lsof_output_is_unknown() {
    let holder = probe_port_holder_from(65000, Ok(("garbage output\n", "exit status: 0")));
    assert!(
        matches!(holder, PortHolder::Unknown(_)),
        "expected Unknown, got {holder:?}"
    );
}

// ── the lsof field parser ───────────────────────────────────────────────────

/// Why: the PID the refusal names comes straight out of this parser; a wrong
/// PID makes the guard's remediation dangerous (it would tell an operator to
/// kill the wrong process).
/// What: parses a realistic `lsof -Fp` block and recovers the PID.
/// Test: This is the test.
#[test]
fn parse_lsof_pids_reads_process_blocks() {
    assert_eq!(parse_lsof_pids("p4242\n"), vec![4242]);
    assert_eq!(parse_lsof_pids("p4242\np555\n"), vec![4242, 555]);
}

/// Why: `lsof` prints nothing when the filter matches no listener; that empty
/// output is what `probe_port_holder` reads as a free port, so it must yield no
/// pids rather than a bogus one.
/// What: empty and whitespace-only input parse to no pids.
/// Test: This is the test.
#[test]
fn parse_lsof_pids_empty() {
    assert!(parse_lsof_pids("").is_empty());
    assert!(parse_lsof_pids("\n \n").is_empty());
}

/// Why: `-Fp` is a request, not a guarantee — `lsof` still emits other field
/// lines in some configurations, and a parser that accepted them would produce
/// garbage PIDs (an `f` descriptor number read as a process id).
/// What: only `p`-prefixed numeric lines are read; command/user/descriptor
/// fields are ignored.
/// Test: This is the test.
#[test]
fn parse_lsof_pids_ignores_other_fields() {
    let text = "p4242\ncmd-trusty-search\nu501\nf7\nn127.0.0.1:7878\n";
    assert_eq!(parse_lsof_pids(text), vec![4242]);
}

// ── port resolution ─────────────────────────────────────────────────────────

/// Why: a guard pointed at the wrong port is worse than no guard — it would
/// accuse an unrelated process. The documented table is the fallback when the
/// member has never recorded an address, so it must be the value that comes
/// back for a daemon that has not run.
/// What: asserts the fallback for a name with no recorded address, and that a
/// non-daemon resolves to `None` rather than a guessed port.
/// Test: This is the test.
#[test]
fn resolve_guard_ports_falls_back_to_the_documented_table() {
    // `tga` is not a daemon and binds nothing: never guess a port for it.
    assert!(resolve_guard_ports("tga").is_empty());
    // A name no daemon uses has no recorded address and no table entry.
    assert!(resolve_guard_ports("definitely-not-a-trusty-daemon").is_empty());
}

/// Why (#4470 MEDIUM-3): trusty-memory WALKS `7070..=7079`, so a busy 7070 on
/// a fresh machine is a condition it survives by taking the next port. The
/// guard must therefore be handed the whole range, not just the default, or it
/// hard-refuses an install that would have worked.
/// What: with no recorded address, a walking member resolves to every port in
/// its range, in order, with the documented default first.
/// Test: This is the test.
#[test]
fn resolve_guard_ports_returns_the_whole_walk_range_for_a_fresh_walker() {
    // Only meaningful when this host has no recorded trusty-memory address; if
    // one exists, the recorded-port branch is authoritative and is covered by
    // `resolve_guard_ports_prefers_a_recorded_address`.
    if trusty_common::read_daemon_addr("trusty-memory")
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let ports = resolve_guard_ports("trusty-memory");
    assert_eq!(ports.len(), 10, "ports: {ports:?}");
    assert_eq!(ports.first(), Some(&7070));
    assert_eq!(ports.last(), Some(&7079));
}

/// Why: the walk table decides whether a busy default port is survivable or
/// fatal, so a wrong entry either hard-refuses a legitimate install (false
/// walker omitted) or silently proceeds past a squatter (non-walker listed as
/// walking). Pin it against the daemons' own documented behaviour.
/// What: trusty-memory walks `7070..=7079` (its `commands/doctor/checks.rs` and
/// `commands/daemon_guard.rs` both say so); every other stable-set member is
/// pinned to one port and has no range.
/// Test: This is the test.
#[test]
fn walk_range_matches_the_daemons_documented_behaviour() {
    assert_eq!(walk_range_for("trusty-memory"), Some(7070..=7079));
    for pinned in [
        "trusty-search",
        "trusty-analyze",
        "trusty-review",
        "trusty-console",
    ] {
        assert_eq!(
            walk_range_for(pinned),
            None,
            "{pinned} is pinned to one port; listing it as a walker would let the \
             guard proceed past a squatter on its real port"
        );
    }
}

/// Why: `guard_bootstrap` treats "this member has no port" as vacuously fine.
/// That is only sound if no launchd-managed member can ever land there — else
/// the vacuous branch becomes a silent hole exactly where the guard matters.
/// This test closes the hole structurally: add a launchd member without a port
/// table entry and it fails.
/// What: every stable-set member whose manage strategy is `Launchd` resolves to
/// `Some(port)`.
/// Test: This is the test.
#[test]
fn every_launchd_member_has_a_guardable_port() {
    for m in stable_set() {
        if m.manage != ManageStrategy::Launchd {
            continue;
        }
        assert!(
            !resolve_guard_ports(&m.binary).is_empty(),
            "launchd member {} has no port the #4470 guard can check",
            m.binary
        );
    }
}
