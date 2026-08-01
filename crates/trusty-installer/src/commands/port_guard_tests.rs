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
fn resolve_guard_port_falls_back_to_the_documented_table() {
    // `tga` is not a daemon and binds nothing: never guess a port for it.
    assert_eq!(resolve_guard_port("tga"), None);
    // A name no daemon uses has no recorded address and no table entry.
    assert_eq!(resolve_guard_port("definitely-not-a-trusty-daemon"), None);
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
            resolve_guard_port(&m.binary).is_some(),
            "launchd member {} has no port the #4470 guard can check",
            m.binary
        );
    }
}
