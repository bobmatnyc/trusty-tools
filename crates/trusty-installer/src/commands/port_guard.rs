//! Pre-bootstrap foreign-port guard for launchd-managed daemons (#4470).
//!
//! Why: `launchctl bootstrap` reports success even when a FOREIGN process — one
//! launchd does not supervise, typically an older-binary orphan — already holds
//! the daemon's TCP port. The bootstrapped job then fails to bind and dies (or
//! never binds at all) while the orphan keeps answering `/health` on that port,
//! so the documented `bootout → install → bootstrap` restart convention
//! (`docs/reference/release-workflow.md`) can complete "successfully" while
//! shipping nothing. #4230 records that this nearly happened on the 1.2.3
//! deploy. #4466 addressed the AFTERMATH — `tm doctor`'s `daemon_orphan` check
//! detects the state once you are already in it — and documented an operator
//! pre-check, but nothing intercepted the `bootstrap` itself. This module is
//! that interception, on the installer side where the bootstrap is actually
//! issued.
//!
//! Composition with #4466, not duplication: that PR's guard lives in
//! trusty-mpm and answers "should `tm start` SPAWN a daemon?" from plist
//! presence. This one answers the distinct question "is the port this
//! `launchctl bootstrap` targets already taken by someone launchd does not
//! own?", and reuses the installer's existing shared helpers rather than
//! growing new copies: [`super::probe_http::fixed_port_for`] (the documented
//! port table), [`super::port::parse_port_from_addr`] (the `host:port`
//! splitter), [`super::plist_label::plist_label_for`] (the label table), and
//! [`super::verify_launchd_state::launchd_owner`] (the ONE `launchctl list`
//! primitive, widened by this issue to carry the running PID).
//!
//! What: [`decide`] is the whole policy as a pure function over an observed
//! [`PortHolder`] and a [`LaunchdOwner`]; [`guard_bootstrap`] gathers those two
//! observations for a member and turns the verdict into a `Result`.
//!
//! FAIL CLOSED. Every state in which the guard cannot PROVE the port is either
//! free or held by the launchd-supervised daemon is a refusal — including an
//! unreadable port probe and an unanswerable `launchctl` query. "We could not
//! check" is never "it is fine to proceed", because that is precisely the
//! fail-open shape (a failed check whose failure branch advances state anyway)
//! this repository has been bitten by repeatedly. The single escape hatch is
//! the explicit [`ALLOW_FOREIGN_PORT_ENV`] operator override, which downgrades
//! a refusal to a loud warning and is named in every refusal message.
//!
//! Test: `port_guard_tests.rs` covers the full [`decide`] table (every
//! holder × owner combination, both override states), the `lsof` parser, and
//! the port-resolution precedence. The two subprocess probes are side-effecting
//! and exercised only by a real install.

use super::verify_launchd_state::LaunchdOwner;

/// Operator override that downgrades a port-guard refusal to a warning.
///
/// Why: the guard fails closed, and a fail-closed guard without a documented
/// escape hatch turns a host the guard cannot inspect (no `lsof`, a sandbox
/// that blocks `launchctl`) into an unrecoverable install. Making the override
/// an explicit, named opt-in keeps the default honest while leaving the
/// operator a way through — and every refusal message names it, so the way
/// through is discoverable at the moment of failure rather than in a doc.
/// What: when set to ANY value, [`decide`] returns
/// [`PortVerdict::ProceedOverridden`] instead of [`PortVerdict::Reject`].
/// Test: `override_downgrades_every_rejection`.
pub const ALLOW_FOREIGN_PORT_ENV: &str = "TCTL_ALLOW_FOREIGN_PORT";

/// Who, if anyone, is listening on the daemon's port right now.
///
/// Why: the guard's decision turns on three genuinely different observations,
/// and the third one — "the probe itself did not work" — is the one a naive
/// `bool` would silently fold into "free", re-creating the fail-open bug.
/// What: `Free` means the probe positively found no listener; `Held` carries
/// the listening PID; `Unknown` carries why the probe could not answer.
/// Test: `decide_*` covers each variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortHolder {
    /// Nothing is listening on the port.
    Free,
    /// This PID is listening on the port.
    Held(u32),
    /// The probe could not determine the holder; the payload says why.
    Unknown(String),
}

/// The guard's verdict for one prospective `launchctl bootstrap`.
///
/// Why: an enum rather than a `bool`/`Result` keeps the override case visible —
/// "we proceeded ANYWAY, and here is what we would have refused" is materially
/// different from "all clear", and the installer narration must not blur them.
/// What: `Proceed` — the port is provably free or provably held by the
/// supervised daemon. `ProceedOverridden` — a refusal downgraded by
/// [`ALLOW_FOREIGN_PORT_ENV`]; the payload is the refusal text, for the
/// warning. `Reject` — the payload is the full operator-facing message.
/// Test: every `decide_*` test asserts on these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortVerdict {
    /// Safe to bootstrap.
    Proceed,
    /// Would have been rejected, but the operator override is set.
    ProceedOverridden(String),
    /// Refuse to bootstrap; the payload explains why and how to clear it.
    Reject(String),
}

/// Decide whether a `launchctl bootstrap` for `binary` may proceed (#4470).
///
/// Why: THE #4470 safety property, isolated as one pure function so the whole
/// truth table is exhaustively testable without a live `lsof`, a live
/// `launchctl`, or a real daemon holding a real port.
///
/// What, given the port the member is expected to serve on:
/// - `Free` → `Proceed` (nothing to contradict).
/// - `Held(pid)` and launchd runs the same `pid` → `Proceed`; the holder IS
///   the supervised daemon, so a (re-)bootstrap is the ordinary idempotent
///   case.
/// - `Held(pid)` and launchd runs a DIFFERENT pid → refuse; two processes are
///   claiming one daemon and the bootstrap would report success regardless.
/// - `Held(pid)` and launchd runs nothing → refuse; this is the #4230 orphan
///   signature exactly.
/// - `Held(pid)` and launchd could not be asked → refuse; the holder cannot be
///   SHOWN to be legitimate, and an unproven holder is not a permitted one.
/// - `Unknown(why)` → refuse regardless of what launchd says; a foreign holder
///   has not been ruled out. This branch is the fail-closed core: it must never
///   fall through to `Proceed`.
///
/// `override_set` (the [`ALLOW_FOREIGN_PORT_ENV`] opt-in) converts any refusal
/// into [`PortVerdict::ProceedOverridden`], and NEVER converts a `Proceed` into
/// anything else.
///
/// Test: `decide_free_port_proceeds`, `decide_supervised_holder_proceeds`,
/// `decide_rejects_foreign_pid_while_launchd_runs_another`,
/// `decide_rejects_orphan_holder_launchd_does_not_run`,
/// `decide_rejects_when_launchd_cannot_be_asked`,
/// `decide_rejects_unreadable_probe`, `decide_never_proceeds_on_a_held_port`,
/// `override_downgrades_every_rejection`,
/// `override_does_not_manufacture_a_rejection`.
pub fn decide(
    binary: &str,
    port: u16,
    holder: &PortHolder,
    owner: LaunchdOwner,
    override_set: bool,
) -> PortVerdict {
    let reason = match (holder, owner) {
        (PortHolder::Free, _) => return PortVerdict::Proceed,
        (PortHolder::Held(pid), LaunchdOwner::Running(owned)) if *pid == owned => {
            return PortVerdict::Proceed
        }
        (PortHolder::Held(pid), LaunchdOwner::Running(owned)) => format!(
            "port {port} is held by pid {pid}, but launchd runs {binary} as pid {owned}. \
             Bootstrapping would report success while pid {pid} keeps serving that port. \
             {}",
            clear_recipe(port, *pid)
        ),
        (PortHolder::Held(pid), LaunchdOwner::NotRunning) => format!(
            "port {port} is held by pid {pid}, which launchd does not supervise — the \
             orphaned-daemon state of issue #4230. `launchctl bootstrap` would report \
             success while pid {pid} keeps serving the port, possibly from an older \
             binary. {}",
            clear_recipe(port, *pid)
        ),
        (PortHolder::Held(pid), LaunchdOwner::Unavailable) => format!(
            "port {port} is held by pid {pid} and launchd could not be asked which pid it \
             runs for {binary}, so the holder cannot be shown to be the supervised daemon. \
             {}",
            clear_recipe(port, *pid)
        ),
        (PortHolder::Unknown(why), _) => format!(
            "could not determine which process holds port {port} for {binary} ({why}), so a \
             foreign process holding it has not been ruled out. Check it by hand with \
             `lsof -nP -iTCP:{port} -sTCP:LISTEN`."
        ),
    };

    if override_set {
        PortVerdict::ProceedOverridden(reason)
    } else {
        PortVerdict::Reject(format!(
            "refusing to bootstrap {binary}: {reason} Set {ALLOW_FOREIGN_PORT_ENV}=1 to \
             bootstrap anyway (the daemon will not get port {port})."
        ))
    }
}

/// The "here is how to clear it" half of a refusal message.
///
/// Why: a guard that only says NO costs the operator a diagnosis; naming the
/// exact `lsof` and `kill` commands, with the observed PID already substituted,
/// makes the refusal actionable in one paste.
/// What: an `lsof` confirmation command plus a graceful `kill -TERM` for `pid`.
/// Test: asserted through the `decide_rejects_*` tests, which check the PID and
/// the recipe appear in the message.
fn clear_recipe(port: u16, pid: u32) -> String {
    format!(
        "Confirm with `lsof -nP -iTCP:{port} -sTCP:LISTEN`, then clear it with \
         `kill -TERM {pid}` before retrying."
    )
}

/// Extract listening PIDs from `lsof -F p` field output.
///
/// Why: keeping the parse pure and separate from the spawn is what lets the
/// `Unknown` (fail-closed) branch be reasoned about at all — output that
/// exists but yields no PID is a parse failure, not an empty port.
/// What: `lsof -F` emits one field per line, `p<pid>` starting each process
/// block. Returns every parsed PID in order, ignoring all other field lines.
/// Test: `parse_lsof_pids_reads_process_blocks`, `parse_lsof_pids_empty`,
/// `parse_lsof_pids_ignores_other_fields`.
pub fn parse_lsof_pids(text: &str) -> Vec<u32> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix('p'))
        .filter_map(|n| n.trim().parse::<u32>().ok())
        .collect()
}

/// Which port `binary` is expected to serve on, if any.
///
/// Why: a guard pointed at the wrong port is worse than no guard. The recorded
/// `http_addr` is the PRIMARY source because it survives a `--port` override
/// and the auto-port-walk trusty-memory performs (7070..=7079) — checking the
/// documented default for a daemon that legitimately walked would refuse
/// forever. The documented table is the fallback for a member that has never
/// run and so recorded nothing.
/// What: the port from `trusty_common::read_daemon_addr` when one is recorded
/// and parseable, else [`super::probe_http::fixed_port_for`], else `None`.
/// `None` is only reachable for a member that is not a stable-set daemon at
/// all — pinned by `every_launchd_member_has_a_guardable_port`.
/// Test: `resolve_guard_port_falls_back_to_the_documented_table`,
/// `every_launchd_member_has_a_guardable_port`.
pub fn resolve_guard_port(binary: &str) -> Option<u16> {
    if let Ok(Some(addr)) = trusty_common::read_daemon_addr(binary) {
        if let Some(port) = super::port::parse_port_from_addr(&addr) {
            return Some(port);
        }
    }
    super::probe_http::fixed_port_for(binary)
}

/// Observe who holds `port`, via `lsof` (side-effecting).
///
/// Why: `lsof` is the only portable way to get the OWNING PID rather than the
/// mere fact that a connect succeeds — and the PID is what the whole decision
/// turns on. A bind-probe would answer "in use" without ever naming the culprit.
/// What: runs `lsof -nP -iTCP:<port> -sTCP:LISTEN -Fp`. Exit 0 with parseable
/// PIDs → [`PortHolder::Held`] (the first PID; a port with several listeners is
/// still a port we do not own). Exit 1 with empty stdout is `lsof`'s documented
/// "no matches" → [`PortHolder::Free`]. Anything else — spawn failure, a
/// non-zero exit that still printed something, output with no parseable PID —
/// is [`PortHolder::Unknown`], which [`decide`] refuses on.
/// Test: side-effecting; the parse is `parse_lsof_pids_*` and the
/// interpretation of each outcome is covered through `decide`.
fn probe_port_holder(port: u16) -> PortHolder {
    let out = match std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"])
        .output()
    {
        Ok(out) => out,
        Err(e) => return PortHolder::Unknown(format!("could not run `lsof`: {e}")),
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let pids = parse_lsof_pids(&stdout);
    if let Some(pid) = pids.first() {
        return PortHolder::Held(*pid);
    }
    // `lsof` exits 1 with no output when nothing matches the filter; that is the
    // only shape we accept as a positive "the port is free".
    if stdout.trim().is_empty() {
        return PortHolder::Free;
    }
    PortHolder::Unknown(format!(
        "`lsof` printed output with no parseable pid (exit {})",
        out.status
    ))
}

/// Guard one member's prospective `launchctl bootstrap` (#4470).
///
/// Why: the single entry point every bootstrap-issuing path in this crate calls
/// — `service_bootstrap::bootstrap_one` (install) and `lifecycle::launchd_control`
/// (`tctl start` / `tctl restart`) — so the policy cannot drift between them and
/// no future bootstrap site has to re-derive it.
///
/// What: resolves the member's port, observes the holder and launchd's owner,
/// and maps [`decide`]'s verdict onto a `Result`. `Ok(())` means proceed;
/// `Err(msg)` is the operator-facing refusal. A member with no port at all is
/// vacuously fine — there is no port claim to contradict — which is distinct
/// from an unreadable probe and is unreachable for the launchd-managed members
/// (`every_launchd_member_has_a_guardable_port` pins that).
///
/// Test: side-effecting (two subprocesses); the policy is `decide_*` and the
/// call-site wiring is `service_bootstrap_tests::bootstrap_one_refuses_*`.
pub fn guard_bootstrap(binary: &str) -> Result<(), String> {
    let Some(port) = resolve_guard_port(binary) else {
        return Ok(());
    };
    let override_set = std::env::var_os(ALLOW_FOREIGN_PORT_ENV).is_some();
    let holder = probe_port_holder(port);
    let owner =
        super::verify_launchd_state::launchd_owner(&super::plist_label::plist_label_for(binary));
    match decide(binary, port, &holder, owner, override_set) {
        PortVerdict::Proceed => Ok(()),
        PortVerdict::ProceedOverridden(reason) => {
            eprintln!("{binary}: {ALLOW_FOREIGN_PORT_ENV} is set — bootstrapping anyway. {reason}");
            Ok(())
        }
        PortVerdict::Reject(msg) => Err(msg),
    }
}

#[cfg(test)]
#[path = "port_guard_tests.rs"]
mod tests;
