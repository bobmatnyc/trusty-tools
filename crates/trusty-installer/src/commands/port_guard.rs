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
//! [`PortHolder`] and a [`LaunchdOwner`]; [`decide_over_range`] applies it
//! across every port a member may bind; [`guard_bootstrap`] (a stable-set
//! member) and [`guard_bootstrap_label`] (anything else, e.g. the trusty-mpm
//! supervisor) gather the observations and turn the verdict into a `Result`.
//!
//! Every `launchctl bootstrap`-issuing site in this crate routes through those
//! two entry points, and `commands::bootstrap_sites_tests` FAILS if a new site
//! appears that is not accounted for — because the first round of this fix
//! enumerated the sites by hand, missed the supervisor, and nothing noticed.
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
//! #6264: the probe reads `lsof`'s per-file TCP state and reports only a
//! process holding the port in LISTEN. It used to take the first PID in the
//! dump and trust `-sTCP:LISTEN` to have removed the clients connected TO the
//! port; `lsof` lists in PID order, so a client older than the daemon sorted
//! first and the guard refused a bootstrap over a process that was not holding
//! the port at all. Every trusty daemon this guard covers still binds TCP
//! (verified against each crate's source for #6264), so the port table itself
//! is not stale — only the holder attribution was.
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
/// What: when set to an affirmative VALUE (`1`/`true`/`yes`, case-insensitive),
/// [`decide`] returns [`PortVerdict::ProceedOverridden`] instead of
/// [`PortVerdict::Reject`]. Deliberately NOT the crate's usual
/// "set-to-anything" convention (`NO_SERVICE_ENV`): every refusal message this
/// module emits says "set `…=1`", which teaches value semantics, so a presence
/// check would make `…=0` ENABLE the bypass an operator was trying to turn off.
/// A safety bypass is the wrong place for that footgun.
/// Test: `override_downgrades_every_rejection`,
/// `override_env_value_requires_an_affirmative_value`.
pub const ALLOW_FOREIGN_PORT_ENV: &str = "TCTL_ALLOW_FOREIGN_PORT";

/// Whether an [`ALLOW_FOREIGN_PORT_ENV`] value enables the bypass.
///
/// Why: pure so the value semantics are testable without mutating the process
/// environment (which races other tests). See [`ALLOW_FOREIGN_PORT_ENV`] for
/// why this is a value check and not a presence check.
/// What: `true` only for `1`, `true`, or `yes` (case-insensitive, trimmed).
/// Anything else — including unset, empty, and `0` — leaves the guard armed.
/// Test: `override_env_value_requires_an_affirmative_value`.
pub fn override_from_env_value(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Who, if anyone, is listening on the daemon's port right now.
///
/// Why: the guard's decision turns on three genuinely different observations,
/// and the third one — "the probe itself did not work" — is the one a naive
/// `bool` would silently fold into "free", re-creating the fail-open bug.
/// What: `Free` means the probe positively established that nothing is
/// listening — `lsof` named no PID AND a real bind to the port succeeded (see
/// [`probe_port_holder`]; `lsof` alone cannot establish this, because without
/// root it only sees the caller's own processes). `Held` carries the listening
/// PID. `Unknown` carries why the probe could not answer, and includes the
/// "something holds it but I cannot see who" case.
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

/// Extract the PIDs that hold a LISTEN socket from `lsof -F pT` field output.
///
/// Why (#6264): the pre-fix parser returned EVERY PID in the dump and the
/// caller took the first, trusting the `-sTCP:LISTEN` selector to have removed
/// the client connections. A process connected TO the port is not a holder of
/// it, and `lsof` lists processes in PID order, so any client with a lower PID
/// than the daemon sorts ahead of it — the guard then accuses a client of
/// squatting and refuses a bootstrap the supervised daemon would have survived.
/// Reading `lsof`'s own per-file `TST=` state makes LISTEN the parser's
/// decision instead of a selector's, so a dump that reaches this function with
/// client rows still in it yields the listener.
///
/// What: `lsof -F` emits one field per line; `p<pid>` opens a process block,
/// `f<fd>` opens a file within it, and `TST=<state>` carries that file's TCP
/// state. A PID is returned only when one of its files is in `LISTEN`, in
/// first-seen order, deduplicated (a daemon has several files on its port).
/// When the dump carries NO `TST=` line at all — an `lsof` build that does not
/// report TCP state — every PID is returned, which is exactly the pre-fix
/// behaviour and keeps `-sTCP:LISTEN` as the only filter rather than
/// classifying a visible holder as invisible.
///
/// Test: `parse_lsof_listen_pids_reads_process_blocks`,
/// `parse_lsof_listen_pids_empty`, `parse_lsof_listen_pids_ignores_other_fields`,
/// `parse_lsof_listen_pids_skips_client_connections`,
/// `parse_lsof_listen_pids_without_state_fields_falls_back_to_every_pid`.
pub fn parse_lsof_listen_pids(text: &str) -> Vec<u32> {
    let mut current: Option<u32> = None;
    let mut every: Vec<u32> = Vec::new();
    let mut listening: Vec<u32> = Vec::new();
    let mut saw_state = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(pid) = line
            .strip_prefix('p')
            .and_then(|n| n.trim().parse::<u32>().ok())
        {
            current = Some(pid);
            if !every.contains(&pid) {
                every.push(pid);
            }
        } else if let Some(state) = line.strip_prefix("TST=") {
            saw_state = true;
            if state.trim() == "LISTEN" {
                if let Some(pid) = current {
                    if !listening.contains(&pid) {
                        listening.push(pid);
                    }
                }
            }
        }
    }
    if saw_state {
        listening
    } else {
        every
    }
}

/// Which port(s) `binary` is expected to serve on.
///
/// Why: a guard pointed at the wrong port is worse than no guard, and there are
/// two distinct wrong-port hazards. (1) A STALE default: the recorded
/// `http_addr` is the PRIMARY source because it survives a `--port` override
/// and reflects the port the daemon actually bound, so a member is checked
/// where it really is. (2) No recorded address: the member's documented default
/// is the one candidate.
/// What: a single-element vec holding the recorded port when one is recorded
/// and parseable (authoritative — that IS the bound port); else the member's
/// single documented default; else empty.
///
/// **The walk-range branch is gone (#6286).** trusty-memory was the only member
/// that walked (`7070..=7079`), and ADR-0032 moved it onto a Unix socket, so
/// nothing produced a range any more. `decide_over_range` below still iterates
/// whatever this returns, which is how a future walker gets the #4470
/// relaxation back without this function pretending to have one today. Empty is reachable for a member that is not a stable-set daemon
/// at all, and — since #6277 — for a member that binds a Unix socket rather
/// than a port. Both are pinned by `every_launchd_member_is_guardable`.
/// Test: `resolve_guard_ports_falls_back_to_the_documented_table`,
/// `resolve_guard_ports_returns_the_whole_walk_range_for_a_fresh_walker`,
/// `every_launchd_member_is_guardable`.
pub fn resolve_guard_ports(binary: &str) -> Vec<u16> {
    // #6277: a member that binds a Unix socket has NO port, and must not
    // inherit one from a leftover `http_addr`. On any machine that ran
    // trusty-review before the transport swap that file is still on disk with
    // `127.0.0.1:7891` in it, and reading it here would make the guard refuse a
    // bootstrap because some unrelated process holds a port this daemon will
    // never bind. Checked BEFORE the recorded-address read, because that read is
    // the one that resurrects the stale value. #6286 makes trusty-memory the
    // sharpest case: its daemon deletes both `http_addr` files at every start,
    // but only once it has started, and the guard runs before that.
    if super::probe_http::uds_socket_for(binary).is_some() {
        return Vec::new();
    }
    if let Ok(Some(addr)) = trusty_common::read_daemon_addr(binary) {
        if let Some(port) = super::port::parse_port_from_addr(&addr) {
            return vec![port];
        }
    }
    super::probe_http::fixed_port_for(binary)
        .map(|p| vec![p])
        .unwrap_or_default()
}

/// Decide over every candidate port a member may bind (#4470, MEDIUM-3 fix).
///
/// Why: a walking daemon only fails when it runs out of ports. Refusing because
/// the FIRST candidate is busy would block a legitimate fresh install on a
/// machine carrying any unrelated listener on the default port.
///
/// What: returns the first non-rejecting verdict — the daemon will take that
/// port. Only when EVERY candidate rejects does it refuse, reporting the
/// verdict for the member's primary (first) port so the message names the port
/// the operator expects, plus how many candidates were tried. Fail-closed is
/// preserved exactly: `Unknown` never counts as a usable port, so a range whose
/// every entry is unreadable still refuses.
///
/// Residual, stated rather than papered over: if an ORPHAN of this same daemon
/// holds the default port while a later port in the range is free, the guard
/// proceeds and the new daemon walks past it. The orphan is then still serving
/// the default port. That is the daemon's own designed behaviour (the stack
/// discovers members through `http_addr`, not the default), and distinguishing
/// "my own orphan" from "an unrelated squatter" is not something a port probe
/// can do. The non-walking members — where the #4230 incident actually
/// happened — have no range and are checked strictly.
///
/// Test: `decide_over_range_proceeds_when_any_candidate_is_usable`,
/// `decide_over_range_refuses_only_when_every_candidate_is_taken`,
/// `decide_over_range_refuses_when_every_candidate_is_unreadable`.
pub fn decide_over_range(
    what: &str,
    ports: &[u16],
    observe: impl Fn(u16) -> (PortHolder, LaunchdOwner),
    override_set: bool,
) -> PortVerdict {
    let mut first_rejection = None;
    for (index, port) in ports.iter().enumerate() {
        let (holder, owner) = observe(*port);
        match decide(what, *port, &holder, owner, override_set) {
            PortVerdict::Reject(msg) => {
                if index == 0 {
                    first_rejection = Some(msg);
                }
            }
            usable => return usable,
        }
    }
    match first_rejection {
        Some(msg) if ports.len() > 1 => PortVerdict::Reject(format!(
            "{msg} (all {} candidate ports for {what} are unavailable)",
            ports.len()
        )),
        Some(msg) => PortVerdict::Reject(msg),
        // Unreachable for a non-empty `ports` (the loop either returned a
        // usable verdict or recorded index 0's rejection); an EMPTY slice means
        // no port claim exists to contradict — see `guard_bootstrap`.
        None => PortVerdict::Proceed,
    }
}

/// Observe who holds `port` (side-effecting): `lsof` for the PID, then a real
/// bind to confirm an empty `lsof` result actually means "free".
///
/// Why two probes, not one. `lsof` is the only portable way to get the OWNING
/// PID, which is what the decision and the remediation message both turn on.
/// But **without root, `lsof` only reports file descriptors of processes the
/// caller owns** — a root-owned or other-user listener produces the identical
/// empty stdout and exit 1 as a genuinely free port. Trusting `lsof` alone
/// therefore classified another user's foreign holder as `Free` and PROCEEDED:
/// a fail-open branch inside the function whose whole contract is fail-closed,
/// and precisely the hole this guard exists to close. A bind cannot name the
/// culprit, but it can distinguish "nothing is there" from "something is there
/// that I cannot see", which is exactly the missing bit.
///
/// What: runs `lsof -nP -iTCP:<port> -sTCP:LISTEN -FpT`. The `T` field carries
/// each file's TCP state, so [`parse_lsof_listen_pids`] can confirm LISTEN
/// itself rather than trusting `-sTCP:LISTEN` to have filtered (#6264). A
/// listening PID → [`PortHolder::Held`] (the first; a port with several
/// listeners is still not
/// ours). No PID → cross-check by binding `127.0.0.1:<port>`: a successful bind
/// (immediately dropped) is the only thing that yields [`PortHolder::Free`];
/// `AddrInUse` yields [`PortHolder::Unknown`] naming the invisible holder; any
/// other bind error is also `Unknown`. Spawn failure or unparseable output is
/// `Unknown`. Every `Unknown` is a refusal in [`decide`].
///
/// The bind is momentary and happens strictly BEFORE the bootstrap, so it
/// cannot collide with the daemon launchd is about to start. `SO_REUSEADDR`
/// (which std sets on Unix) lets a `TIME_WAIT` port bind, which is correct
/// here — a socket in `TIME_WAIT` has no listener and will not answer health
/// checks; only a live listener yields `AddrInUse`.
///
/// Test: side-effecting; the parse is `parse_lsof_pids_*`, the classification
/// of a real occupied port is `probe_reports_unknown_for_a_port_held_by_an_
/// invisible_listener` and `probe_reports_free_for_a_genuinely_free_port`, and
/// the interpretation of each outcome is covered through `decide`.
fn probe_port_holder(port: u16) -> PortHolder {
    let lsof = std::process::Command::new("lsof")
        // #6264: `-FpT` adds the per-file TCP state so the LISTEN decision is
        // this crate's, not the `-sTCP:LISTEN` selector's.
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-FpT"])
        .output();
    let observation = match &lsof {
        Ok(out) => Ok((String::from_utf8_lossy(&out.stdout), out.status.to_string())),
        Err(e) => Err(e.to_string()),
    };
    let observation = match &observation {
        Ok((stdout, status)) => Ok((stdout.as_ref(), status.as_str())),
        Err(e) => Err(e.as_str()),
    };
    probe_port_holder_from(port, observation)
}

/// The classification half of [`probe_port_holder`], over an already-taken
/// `lsof` observation.
///
/// Why (#4470 MEDIUM-1, and why this seam exists at all): the whole point of
/// the bind cross-check is the case where `lsof` reports NOTHING while the port
/// is occupied — a root-owned or other-user listener, invisible to a non-root
/// `lsof`. That case cannot be reproduced in a test by binding a fixture
/// listener, because a fixture the test owns is one `lsof` CAN see: the
/// cross-check is never reached and a test built that way passes just as
/// happily against the fail-open version. (Confirmed by mutation: an earlier
/// version of this module's test did exactly that and did NOT fail when the
/// cross-check was deleted.) Taking the observation as a parameter lets a test
/// hand over an EMPTY `lsof` result for a port it has genuinely occupied, which
/// is precisely the non-root blindness, and makes deleting the cross-check a
/// test failure.
///
/// What: `Err` (spawn failure) → [`PortHolder::Unknown`]. A LISTENING PID →
/// [`PortHolder::Held`]. Non-empty output naming no listener — unparseable
/// text, or (since #6264) a dump carrying only client connections → `Unknown`.
/// EMPTY output → defer to [`port_is_bindable`]; never `Free` directly.
///
/// Test: `empty_lsof_output_on_an_occupied_port_is_unknown_not_free`,
/// `empty_lsof_output_on_a_free_port_is_free`,
/// `lsof_naming_a_pid_reports_it_held`, `lsof_spawn_failure_is_unknown`,
/// `unparseable_lsof_output_is_unknown`,
/// `a_supervised_listener_among_client_rows_is_the_holder`.
fn probe_port_holder_from(port: u16, observation: Result<(&str, &str), &str>) -> PortHolder {
    let (stdout, status) = match observation {
        Ok(v) => v,
        Err(e) => return PortHolder::Unknown(format!("could not run `lsof`: {e}")),
    };
    if let Some(pid) = parse_lsof_listen_pids(stdout).first() {
        return PortHolder::Held(*pid);
    }
    if !stdout.trim().is_empty() {
        return PortHolder::Unknown(format!(
            "`lsof` printed output naming no LISTEN holder of port {port} (exit {status})"
        ));
    }
    // `lsof` named nobody. That is NOT proof the port is free — see this
    // function's doc. Only a successful bind is.
    port_is_bindable(port)
}

/// Classify a port by attempting a real bind (the [`probe_port_holder`]
/// cross-check).
///
/// Why: split out so the fail-closed mapping — only `Ok` becomes `Free` — is a
/// single, named place rather than an inline branch that a later edit could
/// quietly widen.
/// What: binds `127.0.0.1:<port>` and drops the listener immediately. `Ok` →
/// [`PortHolder::Free`]; `AddrInUse` → [`PortHolder::Unknown`] describing a
/// holder `lsof` could not see (typically another user's or a root-owned
/// process); any other error → [`PortHolder::Unknown`] carrying the error.
/// Test: `empty_lsof_output_on_an_occupied_port_is_unknown_not_free`,
/// `empty_lsof_output_on_a_free_port_is_free`.
fn port_is_bindable(port: u16) -> PortHolder {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(_listener) => PortHolder::Free,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => PortHolder::Unknown(format!(
            "port {port} is in use but its holder is not visible to `lsof` \
             (without root, `lsof` only reports processes you own — the holder \
             is likely running as root or another user); re-run \
             `sudo lsof -nP -iTCP:{port} -sTCP:LISTEN` to identify it"
        )),
        Err(e) => PortHolder::Unknown(format!("could not test-bind port {port}: {e}")),
    }
}

/// Guard one stable-set MEMBER's prospective `launchctl bootstrap` (#4470).
///
/// Why: the entry point every member bootstrap path in this crate calls —
/// `service_bootstrap::bootstrap_one` (install), `verify_tail`'s second-layer
/// repair, and `lifecycle::launchd_control` (`tctl start` / `tctl restart`) —
/// so the policy cannot drift between them and no future bootstrap site has to
/// re-derive it.
///
/// What: resolves the member's candidate ports and its launchd label, then
/// defers to [`guard_bootstrap_label`]. A member with no port at all is
/// vacuously fine — there is no port claim to contradict — which is distinct
/// from an unreadable probe. Since #6277 that branch IS reachable for a
/// launchd member: trusty-review binds a Unix socket, and the collision it can
/// actually have is over that path, which `bind_singleton_hardened` arbitrates
/// at bind time by probing before it takes anything over.
/// `every_launchd_member_is_guardable` pins that every launchd member has one
/// or the other.
///
/// Test: side-effecting (subprocess + bind); the policy is `decide_*` /
/// `decide_over_range_*` and the call-site wiring is
/// `service_bootstrap_tests::bootstrap_one_refuses_*`.
pub fn guard_bootstrap(binary: &str) -> Result<(), String> {
    guard_bootstrap_label(
        binary,
        &super::plist_label::plist_label_for(binary),
        &resolve_guard_ports(binary),
    )
}

/// Guard a prospective `launchctl bootstrap` for an explicitly named launchd
/// label and port set (#4470).
///
/// Why: not every bootstrap in this crate belongs to a stable-set MEMBER. The
/// trusty-mpm supervisor (`plist_bootstrap`) has its own label and its own
/// fixed port, and it was the one bootstrap site the first round of this fix
/// missed. Taking `(what, label, ports)` explicitly — rather than deriving them
/// from a member name — is what lets every site share ONE policy instead of the
/// supervisor growing a second, drifting copy.
///
/// What: observes the port holder and launchd's owner for `label`, runs
/// [`decide_over_range`], and maps the verdict onto a `Result`. `Ok(())` means
/// proceed; `Err(msg)` is the operator-facing refusal. An empty `ports` is
/// vacuously fine. When the override is honoured the bypass is logged loudly
/// (it is a safety bypass, so a silent success would be the wrong default).
///
/// Test: side-effecting; the policy is `decide_*` / `decide_over_range_*`.
pub fn guard_bootstrap_label(what: &str, label: &str, ports: &[u16]) -> Result<(), String> {
    if ports.is_empty() {
        return Ok(());
    }
    let override_set =
        override_from_env_value(std::env::var(ALLOW_FOREIGN_PORT_ENV).ok().as_deref());
    let verdict = decide_over_range(
        what,
        ports,
        |port| {
            (
                probe_port_holder(port),
                super::verify_launchd_state::launchd_owner(label),
            )
        },
        override_set,
    );
    match verdict {
        PortVerdict::Proceed => Ok(()),
        PortVerdict::ProceedOverridden(reason) => {
            eprintln!(
                "WARNING: {what}: {ALLOW_FOREIGN_PORT_ENV} is set, so the #4470 foreign-port \
                 guard was BYPASSED and the bootstrap is proceeding anyway. The daemon may not \
                 get its port. Reason the guard would have refused: {reason}"
            );
            Ok(())
        }
        PortVerdict::Reject(msg) => Err(msg),
    }
}

#[cfg(test)]
#[path = "port_guard_tests.rs"]
mod tests;
