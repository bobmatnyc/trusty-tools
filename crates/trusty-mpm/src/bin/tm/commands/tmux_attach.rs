//! Nested-tmux-safe attach helper shared by `tm launch`, `tm connect`, and the
//! guided picker's resume/restart/launch-new paths.
//!
//! Why: tmux refuses `attach-session` when invoked from inside an existing
//! tmux client (i.e. `$TMUX`/`$TMUX_PANE` is set), to avoid nesting one tmux
//! client inside another. Every call site that hands the terminal over to a
//! tmux session hit this failure identically ("sessions should be nested with
//! care, unset $TMUX to force" / `Error: tmux attach-session exited with
//! failure"). `switch-client` is the nested-safe alternative, but issue #2678
//! proved a BARE `switch-client -t <session>` (no explicit `-c <client>`) is
//! unsafe on two counts: (1) it is fire-and-forget — it does not block the
//! calling process the way `attach-session` does, so a caller that loops back
//! to `stdin` after calling it (e.g. the guided picker) orphans itself in a
//! now-invisible pane; (2) its "current client" resolution is a
//! process-ancestry heuristic that can silently retarget a real, unrelated,
//! live tmux client elsewhere on the same server — independently reproduced
//! against a real iTerm2 client during the #2678 investigation.
//! What: [`attach_argv`]/[`switch_client_argv`] are the pure argv builders
//! (testable without spawning tmux); [`inside_tmux`] detects nesting via
//! EITHER `$TMUX` or `$TMUX_PANE` (hardened for #2678 — a spawn path that
//! drops one but not the other must still be treated as nested);
//! [`resolve_switch_target`]/[`parse_single_client_tty`] resolve the EXACT
//! client tty to move, by requiring exactly one client attached to the
//! CURRENT session (fail-closed — `None` — on zero or more than one, rather
//! than letting tmux guess); [`tmux_attach`] ties them together and returns
//! an [`AttachOutcome`] so callers that loop (the picker) know whether they
//! must stop reading stdin.
//!
//! Round 2 (code-critic BLOCK on the initial #2680 patch): the "exactly one
//! client" gate above is only as trustworthy as the SESSION it's scoped to.
//! The first pass resolved that session via an UNTARGETED
//! `display-message -p '#S'`, which — with no client context — falls back to
//! tmux's own "most recently active client" guess, the SAME ambiguous
//! resolution #2678 exists to eliminate; a lone client on the WRONG guessed
//! session is still a hijack, one layer of indirection deeper. Two
//! corrections close this: (1) [`session_for_pane`]/[`pane_tty_for`] resolve
//! by an EXPLICIT pane-id target (`-t "$TMUX_PANE"`) — tmux resolves an exact
//! pane id to exactly one pane or errors; there is no "guess the client"
//! fallback for an id-targeted query. (2) [`own_controlling_tty`] +
//! [`invoking_pane_identity_confirmed`] prove THIS process's real OS
//! controlling terminal (both stdin and stdout) literally IS the pane
//! `$TMUX_PANE` claims, BEFORE that pane is trusted for anything — closing
//! the headless/backgrounded gap where a leaked/stale `$TMUX_PANE` (e.g. a
//! `nohup`ed or re-parented process) would otherwise resolve a session it no
//! longer has any live relationship to. [`switch_decision`] is the pure,
//! I/O-free composition of all five resolved inputs into one fail-closed
//! gate — [`switch_client_to`] resolves each input via a real tmux/
//! `ttyname_r` call, then defers to it, so the composed sequence (not just
//! each sub-check in isolation) is directly unit-tested. Every read-only
//! tmux query added in round 2 ([`session_for_pane`], [`pane_tty_for`],
//! [`resolve_switch_target`]) resolves the tmux binary via
//! `resolve_tmux_binary_or_bare`, exactly like the actual attach/switch
//! spawn — a bare `"tmux"` lookup in any of them could fail against a
//! version-pinned/wrapped install and silently force the fail-closed path,
//! partially reintroducing the hang this PR fixes.
//! Test: `attach_argv_uses_attach_session`, `switch_client_argv_targets_explicit_client`,
//! `inside_tmux_detection_env_matrix` (via `inside_tmux_from_env`),
//! `parse_single_client_tty_*`, `attach_outcome_ends_interactive_loop_matrix`,
//! `invoking_pane_identity_confirmed_*`, `switch_decision_*` cover the pure
//! logic directly (including the full composed guard sequence); `tmux_attach`/
//! `resolve_switch_target`/`session_for_pane`/`pane_tty_for` themselves are
//! exercised indirectly by the picker/launch/connect flows (they spawn a
//! real `tmux` process).

use anyhow::Context as _;

/// Outcome of [`tmux_attach`] — tells a looping caller (the guided picker)
/// whether it may safely read `stdin` again.
///
/// Why (#2678): the picker's `run_tty_picker` loop used to unconditionally
/// fall through to re-fetch sessions and block on `stdin.read_line()` again
/// after every dispatch, including after a `switch-client` handoff. Because
/// `switch-client` does not block, that left the picker's own process
/// orphaned in a pane the operator could no longer see. Threading this
/// outcome back up lets the loop distinguish "the operator's terminal is
/// still this pane" from "control moved elsewhere (or could not be moved
/// safely) — stop."
/// What: four variants; [`Self::ends_interactive_loop`] is the single pure
/// decision every looping caller should consult.
/// Test: `attach_outcome_ends_interactive_loop_matrix`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachOutcome {
    /// `attach-session` ran (outside tmux) and returned control to this
    /// process — the operator detached or the session ended, and this pane
    /// is still the one they are looking at. Safe to keep looping.
    Attached,
    /// Inside tmux: the resolved single client was moved via an explicit
    /// `switch-client -c <tty>`. This process's pane is no longer the one
    /// the operator sees — callers MUST stop reading from stdin.
    Switched,
    /// Inside tmux, but the invoking client could not be resolved
    /// unambiguously (fail-closed, #2678) — nothing was switched. The
    /// target session is already ready; a manual-attach hint was already
    /// printed. Callers MUST stop reading from stdin — nobody is confirmed
    /// to be watching this pane.
    TargetUnresolved,
    /// No attach was attempted at all (a headless/no-TTY caller explicitly
    /// opted out upstream). Not produced by [`tmux_attach`] itself.
    Skipped,
}

impl AttachOutcome {
    /// Why: a picker loop must stop reading further stdin the moment the
    /// operator's client may have moved, or a fail-closed switch left nobody
    /// confirmed to be watching this pane — looping back to
    /// `stdin.read_line()` in either case is exactly the hang/orphan #2678
    /// exists to prevent.
    /// What: `true` for [`Self::Switched`] and [`Self::TargetUnresolved`];
    /// `false` for [`Self::Attached`] (the operator's terminal never left)
    /// and [`Self::Skipped`] (no attach was attempted, so nothing to guard).
    /// Test: `attach_outcome_ends_interactive_loop_matrix`.
    pub(crate) fn ends_interactive_loop(self) -> bool {
        matches!(self, Self::Switched | Self::TargetUnresolved)
    }
}

/// Build the tmux argv for a plain (outside-tmux) `attach-session`.
///
/// Why: kept as a pure, testable seam separate from [`switch_client_argv`] —
/// the two verbs now have different shapes (attach-session never takes a
/// `-c`), so a single combined builder would have to smuggle an `Option`
/// through, reintroducing the "was a bare switch-client ever possible"
/// ambiguity #2678 removes.
/// What: returns `["attach-session", "-t", session]`.
/// Test: `attach_argv_uses_attach_session`.
pub(crate) fn attach_argv(session: &str) -> Vec<String> {
    vec![
        "attach-session".to_string(),
        "-t".to_string(),
        session.to_string(),
    ]
}

/// Build the tmux argv for an EXPLICIT, safe `switch-client -c <client_tty> -t
/// <session>` (#2678).
///
/// Why: a bare `switch-client -t <session>` (no `-c`) resolves "the current
/// client" via a process-ancestry heuristic that was proven (issue #2678) to
/// silently retarget an unrelated, real, live client when that heuristic's
/// assumption doesn't hold (e.g. no client literally attached to the
/// invoking pane's own session). Every `switch-client` invocation in this
/// crate MUST carry an explicit `-c <client_tty>` resolved by
/// [`resolve_switch_target`] — there is no code path left that can construct
/// a bare `switch-client`.
/// What: returns `["switch-client", "-c", client_tty, "-t", session]`.
/// Test: `switch_client_argv_targets_explicit_client`.
pub(crate) fn switch_client_argv(session: &str, client_tty: &str) -> Vec<String> {
    vec![
        "switch-client".to_string(),
        "-c".to_string(),
        client_tty.to_string(),
        "-t".to_string(),
        session.to_string(),
    ]
}

/// Detect whether the current process is already running inside a tmux client.
///
/// Why (#2678 hardening): Bob's live repro printed the `attach-session`
/// branch's message despite genuinely running inside an attached tmux
/// client, meaning a `$TMUX`-only check missed the nesting — plausible if
/// the spawn path that reached bare `tm` dropped `$TMUX` but not
/// `$TMUX_PANE` (tmux sets both when it execs a pane's command, but they can
/// diverge across re-exec/wrapper layers). Checking EITHER is strictly more
/// conservative: it can only ever conclude "nested" MORE often, never less,
/// so the one outcome this exists to prevent — a nested `attach-session`
/// actually firing — becomes structurally harder to hit, not easier.
/// What: returns `true` when `$TMUX` OR `$TMUX_PANE` is set to a non-empty
/// string.
/// Test: `inside_tmux_detection_env_matrix` (via [`inside_tmux_from_env`]);
/// the real `std::env` read is exercised indirectly (environment-dependent).
pub(crate) fn inside_tmux() -> bool {
    inside_tmux_from_env(std::env::var("TMUX").ok(), std::env::var("TMUX_PANE").ok())
}

/// Pure decision behind [`inside_tmux`] — takes the two candidate env values
/// directly so the matrix (set / unset / empty) is unit-testable without
/// mutating real process environment.
///
/// Test: `inside_tmux_detection_env_matrix`.
fn inside_tmux_from_env(tmux: Option<String>, tmux_pane: Option<String>) -> bool {
    let present = |v: Option<String>| v.map(|s| !s.is_empty()).unwrap_or(false);
    present(tmux) || present(tmux_pane)
}

/// Pure gate behind `$TMUX_PANE`'s use in [`switch_client_to`] (#2678 round
/// 2): present-and-non-empty is the only value ever acted on for pane-id
/// targeting.
///
/// Why: extracted as a pure seam — mirroring [`inside_tmux_from_env`] — so
/// the env-presence gate that decides whether `switch_client_to` even
/// attempts pane-targeted resolution is unit-testable independent of a live
/// tmux server.
/// What: `Some(value)` unchanged when non-empty; `None` for both an unset
/// var and an empty-string export (a shell script edge case tmux itself
/// never produces).
/// Test: `tmux_pane_env_gate_present_and_empty_matrix`.
fn tmux_pane_id_from_env(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

/// Resolve the tmux session name of the CURRENT client's pane (#2157 item 4).
///
/// Why: the nested-session guard in `guided.rs` needs to know which tmux
/// session bare `tm` is running inside, so it can check whether that session
/// already maps to a managed record BEFORE offering to launch a brand-new
/// (nested) one. `$TMUX`'s own value encodes a socket path and a numeric
/// window/pane index, not the session's NAME, so the name has to come from
/// tmux itself.
/// What: returns `None` immediately when [`inside_tmux`] is `false` (no
/// session to query). Otherwise runs `tmux display-message -p '#S'` and
/// returns the trimmed, non-empty stdout, or `None` on any I/O failure,
/// non-zero exit, or empty output.
/// Test: I/O path, not unit-tested (requires a live tmux server); the guard's
/// decision logic that CONSUMES this value is pure and tested separately
/// (`nested_managed_match_*` in `tests_behavior_c_tests.rs`).
pub(crate) fn current_tmux_session_name() -> Option<String> {
    if !inside_tmux() {
        return None;
    }
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Resolve the tmux `pane_id` (e.g. `"%5"`) of the CURRENT client's pane
/// (#2453 review finding 1, round 2).
///
/// Why: [`current_tmux_session_name`] only proves the tmux SESSION matches —
/// every window/pane in that session shares the same name — which is not
/// proof the CURRENT pane is the one bound to a specific managed record. A
/// process-env-var comparison was tried and PROVEN insufficient: tmux's
/// session-scoped `set-environment` (used to heal `TM_MANAGED_SESSION_ID`
/// into a pane that never got the durable publish, #2157 item 3) is
/// inherited into the process env of every NEW pane/window created in that
/// session AFTERWARD — verified empirically against a live tmux 3.6b. tmux's
/// own `pane_id` is never inherited across panes, making it the only
/// reliable "is this literally the same pane" signal available to the CLI.
/// What: returns `None` immediately when [`inside_tmux`] is `false`.
/// Otherwise runs `tmux display-message -p '#{pane_id}'` and returns the
/// trimmed, non-empty stdout, or `None` on any I/O failure, non-zero exit,
/// or empty output — mirrors [`current_tmux_session_name`]'s exact shape.
/// Test: I/O path, not unit-tested (requires a live tmux server); the pure
/// decision that CONSUMES this value
/// (`guided::pane_identity_confirmed`) is unit-tested separately.
pub(crate) fn current_tmux_pane_id() -> Option<String> {
    if !inside_tmux() {
        return None;
    }
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#{pane_id}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// Parse `tmux list-clients` output into the single attached client's tty,
/// or `None` when the arity check fails (#2678).
///
/// Why: extracted as a pure function so the "exactly one client" fail-closed
/// rule is unit-testable against captured `list-clients` text, without a
/// live tmux server.
/// What: each non-empty `list-clients` line looks like
/// `/dev/ttys021: session-name [200x50 xterm-256color]`; this splits on the
/// first `:` and collects the leading tty field. Returns `Some(tty)` only
/// when exactly one candidate line is present; returns `None` for zero
/// (nothing attached — nothing safe to move) or more than one (ambiguous —
/// switching would silently prefer one client over another).
/// Test: `parse_single_client_tty_single_line_returns_tty`,
/// `parse_single_client_tty_empty_returns_none`,
/// `parse_single_client_tty_multiple_returns_none`.
pub(crate) fn parse_single_client_tty(list_clients_output: &str) -> Option<String> {
    let ttys: Vec<&str> = list_clients_output
        .lines()
        .filter_map(|line| line.split_once(':').map(|(tty, _rest)| tty.trim()))
        .filter(|tty| !tty.is_empty())
        .collect();
    match ttys.as_slice() {
        [single] => Some((*single).to_string()),
        _ => None,
    }
}

/// Resolve the tty of the single tmux client attached to `session`, for a
/// safe, explicit `switch-client -c <tty>` target (#2678).
///
/// Why: a bare `switch-client` resolves "the current client" via a
/// process-ancestry heuristic that assumes the invoking process is a
/// foreground descendant of the client's own attach process. When `tm` runs
/// in a way that breaks that assumption (no client literally attached to the
/// invoking pane's own session — e.g. a session driven purely via
/// `send-keys`, or any other tty/process layering), that heuristic has been
/// proven (issue #2678, reproduced against a real iTerm2 client) to silently
/// fall back and retarget a DIFFERENT, unrelated, live client elsewhere on
/// the same tmux server. This function replaces that ambiguous resolution
/// with an explicit, verifiable one: list the clients attached to `session`
/// and only proceed when there is EXACTLY one. `session` MUST itself already
/// be resolved deterministically (round 2, #2678: via [`session_for_pane`],
/// pane-id targeted) — this function only closes the client-arity half of
/// the ambiguity; a caller that hands it an untargeted/guessed session name
/// reopens the exact hijack this exists to prevent, one layer up.
/// What: resolves the tmux binary via `resolve_tmux_binary_or_bare` (round
/// 2 review, MEDIUM — every tmux invocation in this crate routes through the
/// common resolved entry point, #2399; a bare `"tmux"` lookup here could fail
/// against a version-pinned/wrapped install, silently forcing the
/// fail-closed path this fix depends on), runs `list-clients -t <session>`,
/// and delegates parsing to [`parse_single_client_tty`]. Returns `None` on
/// any I/O failure, non-zero exit, or when the arity check fails.
/// Test: I/O path, not unit-tested (requires a live tmux server); the parse/
/// arity logic is covered by `parse_single_client_tty_*`.
pub(crate) fn resolve_switch_target(session: &str) -> Option<String> {
    let tmux_bin = trusty_mpm::core::tmux::resolve_tmux_binary_or_bare();
    let output = std::process::Command::new(&tmux_bin)
        .args(["list-clients", "-t", session])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_single_client_tty(&String::from_utf8_lossy(&output.stdout))
}

/// Resolve the tmux SESSION name of an EXPLICIT pane, by pane-id target
/// (#2678 round 2 — code-critic BLOCK on the initial patch).
///
/// Why: [`current_tmux_session_name`] (used elsewhere for a non-security
/// -critical UX guard) queries `#S` UNTARGETED, which tmux resolves via a
/// "most recently active client" heuristic — the SAME fallback the #2678
/// investigation proved silently points at an unrelated, real, live
/// client/session when the invoking process is not a genuine foreground
/// descendant of that client. An explicit pane-id target (`-t "%N"`) has NO
/// such fallback: tmux resolves a pane id to exactly the one pane with that
/// id, or errors — there is nothing left to guess. [`switch_client_to`] uses
/// this, not [`current_tmux_session_name`], for exactly that reason.
/// What: resolves the tmux binary via `resolve_tmux_binary_or_bare` (round
/// 2 review, MEDIUM — see [`resolve_switch_target`]'s doc for why a bare
/// `"tmux"` lookup here is unsafe), runs
/// `display-message -p -t <pane_id> '#S'`, and returns the trimmed,
/// non-empty stdout, or `None` on any I/O failure, non-zero exit (e.g. the
/// pane no longer exists), or empty output.
/// Test: I/O path, not unit-tested (requires a live tmux server) — mirrors
/// [`current_tmux_session_name`]'s exact shape, targeted instead of bare.
fn session_for_pane(pane_id: &str) -> Option<String> {
    let tmux_bin = trusty_mpm::core::tmux::resolve_tmux_binary_or_bare();
    let output = std::process::Command::new(&tmux_bin)
        .args(["display-message", "-p", "-t", pane_id, "#S"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Resolve the tmux pty (`#{pane_tty}`) of an EXPLICIT pane, by pane-id
/// target (#2678 round 2).
///
/// Why: paired with [`own_controlling_tty`]/[`invoking_pane_identity_confirmed`]
/// to prove THIS process's real OS controlling terminal literally IS the
/// pane `$TMUX_PANE` claims, before that pane is trusted to resolve a
/// session at all — closing the headless/backgrounded, leaked/stale-env gap
/// (a `nohup`ed or re-parented process can retain a `$TMUX_PANE` value from
/// a pane it is no longer live in).
/// What: resolves the tmux binary via `resolve_tmux_binary_or_bare` (round
/// 2 review, MEDIUM — see [`resolve_switch_target`]'s doc for why a bare
/// `"tmux"` lookup here is unsafe), runs
/// `display-message -p -t <pane_id> '#{pane_tty}'`, and returns the trimmed,
/// non-empty stdout, or `None` on any I/O failure, non-zero exit, or empty
/// output.
/// Test: I/O path, not unit-tested (requires a live tmux server); the pure
/// comparison that CONSUMES this value is unit-tested via
/// `invoking_pane_identity_confirmed_*`.
fn pane_tty_for(pane_id: &str) -> Option<String> {
    let tmux_bin = trusty_mpm::core::tmux::resolve_tmux_binary_or_bare();
    let output = std::process::Command::new(&tmux_bin)
        .args(["display-message", "-p", "-t", pane_id, "#{pane_tty}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let tty = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!tty.is_empty()).then_some(tty)
}

/// Resolve this process's own controlling terminal device path via
/// `ttyname_r` on `fd` (#2678 round 2).
///
/// Why: before ever trusting `$TMUX_PANE` to resolve which session/client to
/// move, this process must prove it is genuinely, currently attached to a
/// real controlling terminal — not headless/backgrounded with a leaked,
/// stale `$TMUX_PANE` inherited from a process tree whose actual tty is long
/// gone (`nohup`, a detached daemon child, a re-parented shell, …).
/// `ttyname_r` is the direct OS-level answer: it fails whenever `fd` is not
/// connected to a tty at all, which is exactly the signal needed to fail
/// closed. Mirrors the `libc`-via-`unsafe` pattern already used elsewhere in
/// this crate (`formatters/banner/mod.rs`'s `TIOCGWINSZ` ioctl,
/// `core/discovery.rs`'s `libc::kill`).
/// What: returns `Some(path)` on success, `None` on any `ttyname_r` failure
/// (not a tty, or any other OS error).
/// Test: OS-dependent (requires a real pty), not unit-tested directly; the
/// pure comparison it feeds — [`invoking_pane_identity_confirmed`] — is.
fn own_controlling_tty(fd: libc::c_int) -> Option<String> {
    let mut buf = [0_u8; 256];
    // SAFETY: `buf` is a valid, appropriately-sized, mutable byte buffer for
    // the duration of this call, sized well above any real tty device path;
    // `fd` is a plain integer file descriptor (STDIN_FILENO/STDOUT_FILENO)
    // supplied by the caller, never dereferenced here.
    let rc = unsafe { libc::ttyname_r(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).ok().map(str::to_string)
}

/// Confirm this process's own controlling terminal genuinely IS the pane
/// tmux reports for `$TMUX_PANE` (#2678 round 2 — the positive identity
/// check the code-critic review required).
///
/// Why: `$TMUX_PANE` is a process-env value — cheap to carry stale or leaked
/// into a context that is no longer that pane's live, interactive process
/// (backgrounded, `nohup`ed, re-parented). Trusting it to resolve "my own
/// session" without first proving it corresponds to THIS process's actual OS
/// controlling terminal would reopen an unverified-identity gap in the same
/// family as the switch-client hijack (#2678), one layer of indirection
/// deeper. This is deliberately a pure, exhaustively-testable seam over the
/// OS-derived strings, independent of a live tmux server — "prove the
/// resolved pane IS the invoking process's own terminal," not "a value is
/// present."
/// What: `true` only when BOTH `own_stdin_tty` and `own_stdout_tty` are
/// `Some` and equal to `pane_tty` — requiring both closes the gap where one
/// stream is redirected away from the terminal while the other still looks
/// like a tty. Any `None` (no controlling terminal at all — headless/
/// backgrounded) or mismatch (a stale/leaked `$TMUX_PANE` pointing at a pane
/// this process is not really running in) returns `false` — fail closed.
/// Test: `invoking_pane_identity_confirmed_matches_allows_switch`,
/// `invoking_pane_identity_confirmed_stdin_mismatch_fails_closed`,
/// `invoking_pane_identity_confirmed_no_controlling_tty_fails_closed`,
/// `invoking_pane_identity_confirmed_partial_tty_fails_closed`.
fn invoking_pane_identity_confirmed(
    own_stdin_tty: Option<&str>,
    own_stdout_tty: Option<&str>,
    pane_tty: &str,
) -> bool {
    matches!(
        (own_stdin_tty, own_stdout_tty),
        (Some(stdin), Some(stdout)) if stdin == pane_tty && stdout == pane_tty
    )
}

/// Print the fail-closed hint when a switch-client target could not be
/// resolved safely (#2678) — the session is ready, but nothing was moved.
///
/// Why (#2678 round 2 LOW): the original wording's second line suggested
/// `tmux switch-client -t <name>` — the exact bare, unguarded form this PR
/// eliminates. Recommending it in the fail-closed hint would steer an
/// operator right back into the unsafe idiom. The remedy now only ever names
/// `attach-session` from a fresh shell.
fn print_fail_closed_hint(name: &str) {
    eprintln!(
        "tm: session '{name}' is ready, but tm could not verify it is safe to move this \
         terminal's client — refusing to guess (issue #2678: an unverified switch-client can \
         silently retarget an unrelated terminal)."
    );
    eprintln!(
        "tm: attach from a fresh shell (outside any tmux client) with: tmux attach-session -t \
         {name}"
    );
}

/// Pure composed-guard decision behind [`switch_client_to`] (round 2 review,
/// MEDIUM — the individual sub-checks were each unit-tested, but nothing
/// exercised the ORDER/composition of the full chain end-to-end).
///
/// Why: `switch_client_to` threads five independently-resolved, fallible
/// values through a sequence of checks before ever calling `switch-client`.
/// Each sub-predicate ([`tmux_pane_id_from_env`],
/// [`invoking_pane_identity_confirmed`], the arity check inside
/// [`resolve_switch_target`]) already has its own unit tests, but none of
/// them proved the WHOLE sequence composes correctly — e.g. that a resolved
/// session and client do NOT override an earlier identity mismatch. Factoring
/// the composition into this pure function (no I/O — it takes
/// already-resolved `Option`s) makes that whole-chain behavior directly
/// testable without a live tmux server, and gives [`switch_client_to`] a
/// single source of truth for the gate instead of duplicating the sequence
/// inline.
/// What: mirrors [`switch_client_to`]'s exact step order: `tmux_pane`
/// present → `pane_tty` resolved → BOTH `own_stdin_tty`/`own_stdout_tty`
/// equal `pane_tty` (via [`invoking_pane_identity_confirmed`]) → `session`
/// resolved → `client_tty` resolved. Returns `Some(client_tty)` only when
/// EVERY step holds (the caller may safely run `switch-client -c
/// <client_tty>`); returns `None` the instant any step fails — fail closed,
/// regardless of what later inputs would have resolved to.
/// Test: `switch_decision_all_steps_confirmed_allows_switch`,
/// `switch_decision_missing_pane_env_fails_closed`,
/// `switch_decision_pane_tty_unresolved_fails_closed`,
/// `switch_decision_identity_mismatch_fails_closed`,
/// `switch_decision_session_unresolved_fails_closed`,
/// `switch_decision_client_unresolved_fails_closed`.
fn switch_decision(
    tmux_pane: Option<&str>,
    pane_tty: Option<&str>,
    own_stdin_tty: Option<&str>,
    own_stdout_tty: Option<&str>,
    session: Option<&str>,
    client_tty: Option<&str>,
) -> Option<String> {
    tmux_pane?;
    let pane_tty = pane_tty?;
    if !invoking_pane_identity_confirmed(own_stdin_tty, own_stdout_tty, pane_tty) {
        return None;
    }
    session?;
    client_tty.map(str::to_owned)
}

/// Move the resolved single client of the pane's OWN session into `name` via
/// an explicit `switch-client -c <tty>` (#2678, hardened round 2).
///
/// Why: the inside-tmux half of [`tmux_attach`] — the sole I/O driver that
/// resolves each of [`switch_decision`]'s inputs via a real tmux/`ttyname_r`
/// call, then defers the actual pass/fail-closed gate to that pure function
/// so the composition itself stays testable and free of drift from what
/// actually runs.
/// What: (1) reads `$TMUX_PANE`; (2) resolves that pane's tty via
/// [`pane_tty_for`]; (3) resolves this process's own stdin/stdout
/// controlling terminal via [`own_controlling_tty`]; (4) resolves the
/// SESSION that pane belongs to via [`session_for_pane`] (pane-id targeted,
/// never the ambiguous untargeted `#S`); (5) resolves the single attached
/// client of that session via [`resolve_switch_target`]. All five inputs are
/// then threaded through [`switch_decision`] in one call — on `Some(tty)`,
/// runs `switch-client -c <tty> -t <name>` and returns
/// [`AttachOutcome::Switched`]; on `None` (ANY step failed, in ANY
/// combination), prints [`print_fail_closed_hint`] and returns
/// [`AttachOutcome::TargetUnresolved`] WITHOUT invoking tmux at all — never a
/// bare, unguarded `switch-client`.
fn switch_client_to(name: &str) -> anyhow::Result<AttachOutcome> {
    let tmux_pane = tmux_pane_id_from_env(std::env::var("TMUX_PANE").ok());
    let pane_tty = tmux_pane.as_deref().and_then(pane_tty_for);
    let own_stdin_tty = own_controlling_tty(libc::STDIN_FILENO);
    let own_stdout_tty = own_controlling_tty(libc::STDOUT_FILENO);
    let session = tmux_pane.as_deref().and_then(session_for_pane);
    let client_tty = session.as_deref().and_then(resolve_switch_target);

    let Some(client_tty) = switch_decision(
        tmux_pane.as_deref(),
        pane_tty.as_deref(),
        own_stdin_tty.as_deref(),
        own_stdout_tty.as_deref(),
        session.as_deref(),
        client_tty.as_deref(),
    ) else {
        print_fail_closed_hint(name);
        return Ok(AttachOutcome::TargetUnresolved);
    };

    eprintln!("tm: switching client to session '{name}'");
    let tmux_bin = trusty_mpm::core::tmux::resolve_tmux_binary_or_bare();
    let status = std::process::Command::new(&tmux_bin)
        .args(switch_client_argv(name, &client_tty))
        .status()
        .context("failed to invoke tmux")?;
    if !status.success() {
        anyhow::bail!(
            "tmux switch-client exited with failure — the target session '{name}' may be \
             attached on a different tmux server; try `tmux -L <socket> switch-client -t \
             {name}` or detach the other client first"
        );
    }
    Ok(AttachOutcome::Switched)
}

/// Attach (or switch) the current terminal into the tmux session `name`.
///
/// Why: single choke point for the nested-tmux-safe attach behavior so
/// `guided.rs`, `guided_resume.rs`, `guided_launch.rs`, and `launch.rs` don't
/// each re-implement the `$TMUX` branch — and so a `switch-client` failure
/// gets a more actionable error than a bare "exited with failure", and so a
/// looping caller (the picker) has one place to learn whether it must stop
/// (#2678).
/// What: outside tmux, shells out to `attach-session` (blocking; returns
/// [`AttachOutcome::Attached`] once the operator detaches or the session
/// ends). Inside tmux, delegates to [`switch_client_to`], which resolves the
/// exact client to move and either switches it explicitly
/// ([`AttachOutcome::Switched`]) or fails closed
/// ([`AttachOutcome::TargetUnresolved`]) — never a bare, unguarded
/// `switch-client`. Returns `Err` only when tmux itself was invoked and
/// exited non-zero.
/// Test: exercised indirectly by the picker/launch/connect flows; the argv
/// selection is unit-tested via `attach_argv`/`switch_client_argv`; the
/// fail-closed/loop-termination decision via
/// `attach_outcome_ends_interactive_loop_matrix`.
pub(crate) fn tmux_attach(name: &str) -> anyhow::Result<AttachOutcome> {
    if inside_tmux() {
        return switch_client_to(name);
    }
    eprintln!("tm: attaching to session '{name}'");
    // #2398: resolves the binary through the crate's shared resolver rather
    // than a bare "tmux" lookup — this is the "attach path" migration (the
    // interactive attach-session/switch-client spawn itself inherits this
    // process's stdio via `.status()`, which does not fit
    // `core::tmux::run_tmux`'s output-capturing `.output()` shape, so only
    // binary resolution is unified here; see `core::tmux`'s module doc for
    // the full scope note).
    let tmux_bin = trusty_mpm::core::tmux::resolve_tmux_binary_or_bare();
    let status = std::process::Command::new(&tmux_bin)
        .args(attach_argv(name))
        .status()
        .context("failed to invoke tmux")?;
    if !status.success() {
        anyhow::bail!("tmux attach-session exited with failure");
    }
    Ok(AttachOutcome::Attached)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outside tmux, attach must use `attach-session` — today's behavior,
    /// unchanged for the non-nested case.
    ///
    /// Test: this function IS the test.
    #[test]
    fn attach_argv_uses_attach_session() {
        assert_eq!(
            attach_argv("my-session"),
            vec!["attach-session", "-t", "my-session"]
        );
    }

    /// `switch-client` must always carry an explicit `-c <client_tty>` — the
    /// core #2678 fix. No code path may construct a bare switch-client argv.
    ///
    /// Test: this function IS the test.
    #[test]
    fn switch_client_argv_targets_explicit_client() {
        assert_eq!(
            switch_client_argv("tmpm-mcp-a-protocol-00f6f5ef", "/dev/ttys021"),
            vec![
                "switch-client",
                "-c",
                "/dev/ttys021",
                "-t",
                "tmpm-mcp-a-protocol-00f6f5ef"
            ]
        );
    }

    /// Env matrix for the hardened #2678 detection: `$TMUX` set, `$TMUX`
    /// unset but `$TMUX_PANE` set, neither set, and both set-but-empty (which
    /// tmux never does, but a shell script could export).
    #[test]
    fn inside_tmux_detection_env_matrix() {
        assert!(inside_tmux_from_env(
            Some("/tmp/tmux-501/default,123,0".into()),
            None
        ));
        assert!(inside_tmux_from_env(None, Some("%37".into())));
        assert!(inside_tmux_from_env(
            Some("/tmp/tmux-501/default,123,0".into()),
            Some("%37".into())
        ));
        assert!(!inside_tmux_from_env(None, None));
        assert!(!inside_tmux_from_env(
            Some(String::new()),
            Some(String::new())
        ));
        assert!(!inside_tmux_from_env(Some(String::new()), None));
    }

    /// A single attached client is the only arity that resolves — this is
    /// the fail-closed guardrail that prevents #2678's client-hijack.
    #[test]
    fn parse_single_client_tty_single_line_returns_tty() {
        assert_eq!(
            parse_single_client_tty("/dev/ttys021: tm-writing-01 [200x50 xterm-256color]\n"),
            Some("/dev/ttys021".to_string())
        );
    }

    #[test]
    fn parse_single_client_tty_empty_returns_none() {
        assert_eq!(parse_single_client_tty(""), None);
    }

    #[test]
    fn parse_single_client_tty_multiple_returns_none() {
        let output = "/dev/ttys021: tm-writing-01 [200x50 xterm-256color]\n\
                       /dev/ttys033: tm-writing-01 [180x40 xterm-256color]\n";
        assert_eq!(parse_single_client_tty(output), None);
    }

    /// `AttachOutcome`'s loop-termination decision — the exact seam that
    /// prevents the picker from looping back to `stdin` after a handoff (or
    /// a fail-closed skip) and keeps looping in the two cases where the
    /// operator's terminal never left.
    #[test]
    fn attach_outcome_ends_interactive_loop_matrix() {
        assert!(!AttachOutcome::Attached.ends_interactive_loop());
        assert!(AttachOutcome::Switched.ends_interactive_loop());
        assert!(AttachOutcome::TargetUnresolved.ends_interactive_loop());
        assert!(!AttachOutcome::Skipped.ends_interactive_loop());
    }

    /// The exact-match case: this process's own stdin AND stdout controlling
    /// terminal both equal the pane's tty — the only case that may proceed
    /// to `switch-client` (#2678 round 2).
    #[test]
    fn invoking_pane_identity_confirmed_matches_allows_switch() {
        assert!(invoking_pane_identity_confirmed(
            Some("/dev/ttys001"),
            Some("/dev/ttys001"),
            "/dev/ttys001"
        ));
    }

    /// A stale/leaked `$TMUX_PANE` pointing at a DIFFERENT pane than the one
    /// this process's own stdin is really attached to must fail closed — the
    /// exact residual hijack path the round-2 review flagged (a session
    /// resolved from an unrelated pane).
    #[test]
    fn invoking_pane_identity_confirmed_stdin_mismatch_fails_closed() {
        assert!(!invoking_pane_identity_confirmed(
            Some("/dev/ttys999"),
            Some("/dev/ttys001"),
            "/dev/ttys001"
        ));
    }

    /// Same mismatch, but on stdout instead of stdin — requiring BOTH to
    /// match closes the gap where only one stream is redirected away from
    /// the real terminal.
    #[test]
    fn invoking_pane_identity_confirmed_stdout_mismatch_fails_closed() {
        assert!(!invoking_pane_identity_confirmed(
            Some("/dev/ttys001"),
            Some("/dev/ttys999"),
            "/dev/ttys001"
        ));
    }

    /// No controlling terminal at all (both `ttyname_r` calls failed) — the
    /// headless/backgrounded/`nohup`ed case explicitly widened by the round-2
    /// review (a leaked `$TMUX_PANE` with no real tty behind it). Must fail
    /// closed, never switch.
    #[test]
    fn invoking_pane_identity_confirmed_no_controlling_tty_fails_closed() {
        assert!(!invoking_pane_identity_confirmed(
            None,
            None,
            "/dev/ttys001"
        ));
    }

    /// One stream resolves to a real tty, the other has none at all — still
    /// not a confirmed identity; requiring both `Some` and matching is what
    /// makes this fail closed rather than accepting a partial signal.
    #[test]
    fn invoking_pane_identity_confirmed_partial_tty_fails_closed() {
        assert!(!invoking_pane_identity_confirmed(
            Some("/dev/ttys001"),
            None,
            "/dev/ttys001"
        ));
    }

    /// `$TMUX_PANE`-based resolution's own env gate: present-and-non-empty is
    /// the only value `switch_client_to` will ever act on; unset or an
    /// empty-string export (a shell script edge case, mirrors
    /// `inside_tmux_detection_env_matrix`) must both fail closed before any
    /// tmux I/O is attempted.
    #[test]
    fn tmux_pane_env_gate_present_and_empty_matrix() {
        assert_eq!(
            tmux_pane_id_from_env(Some("%260".to_string())),
            Some("%260".to_string())
        );
        assert_eq!(tmux_pane_id_from_env(Some(String::new())), None);
        assert_eq!(tmux_pane_id_from_env(None), None);
    }

    /// The composed guard sequence, end to end: every step resolves and
    /// agrees — this is the only case that may proceed to `switch-client`
    /// (round 2 review, MEDIUM — the composition itself, not just each
    /// sub-check, needed direct coverage).
    #[test]
    fn switch_decision_all_steps_confirmed_allows_switch() {
        assert_eq!(
            switch_decision(
                Some("%260"),
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                Some("tm-dogfood-relaunch-01"),
                Some("/dev/ttys000"),
            ),
            Some("/dev/ttys000".to_string())
        );
    }

    /// No `$TMUX_PANE` at all — fails closed even though every later input
    /// (deliberately crafted here) would otherwise agree, proving the env
    /// gate is checked first and is not bypassable by downstream agreement.
    #[test]
    fn switch_decision_missing_pane_env_fails_closed() {
        assert_eq!(
            switch_decision(
                None,
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                Some("tm-dogfood-relaunch-01"),
                Some("/dev/ttys000"),
            ),
            None
        );
    }

    /// `$TMUX_PANE` present but its pane tty could not be resolved (e.g. the
    /// pane no longer exists) — fails closed before identity is even
    /// evaluated.
    #[test]
    fn switch_decision_pane_tty_unresolved_fails_closed() {
        assert_eq!(
            switch_decision(
                Some("%260"),
                None,
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                Some("tm-dogfood-relaunch-01"),
                Some("/dev/ttys000"),
            ),
            None
        );
    }

    /// Session AND client both resolve cleanly, but this process's own
    /// controlling terminal does not match the claimed pane — the exact
    /// residual-hijack shape the round-2 review flagged. A confirmed session/
    /// client must NOT override a failed identity check.
    #[test]
    fn switch_decision_identity_mismatch_fails_closed() {
        assert_eq!(
            switch_decision(
                Some("%260"),
                Some("/dev/ttys001"),
                Some("/dev/ttys999"),
                Some("/dev/ttys001"),
                Some("tm-dogfood-relaunch-01"),
                Some("/dev/ttys000"),
            ),
            None
        );
    }

    /// Identity confirmed, but the pane's own session could not be resolved
    /// — fails closed before a client is ever looked up.
    #[test]
    fn switch_decision_session_unresolved_fails_closed() {
        assert_eq!(
            switch_decision(
                Some("%260"),
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                None,
                Some("/dev/ttys000"),
            ),
            None
        );
    }

    /// Everything up to the session resolves and matches, but the session's
    /// client arity check failed (zero or 2+ attached clients) — fails
    /// closed, matching `resolve_switch_target`'s own fail-closed contract.
    #[test]
    fn switch_decision_client_unresolved_fails_closed() {
        assert_eq!(
            switch_decision(
                Some("%260"),
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                Some("/dev/ttys001"),
                Some("tm-dogfood-relaunch-01"),
                None,
            ),
            None
        );
    }
}
