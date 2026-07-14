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
//! Test: `attach_argv_uses_attach_session`, `switch_client_argv_targets_explicit_client`,
//! `inside_tmux_detection_env_matrix` (via `inside_tmux_from_env`),
//! `parse_single_client_tty_*`, `attach_outcome_ends_interactive_loop_matrix`
//! cover the pure logic directly; `tmux_attach`/`resolve_switch_target`
//! themselves are exercised indirectly by the picker/launch/connect flows
//! (they spawn a real `tmux` process).

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
/// (the CURRENT session — the one this `tm` process's own pane lives in) and
/// only proceed when there is EXACTLY one.
/// What: runs `tmux list-clients -t <session>` and delegates parsing to
/// [`parse_single_client_tty`]. Returns `None` on any I/O failure, non-zero
/// exit, or when the arity check fails.
/// Test: I/O path, not unit-tested (requires a live tmux server); the parse/
/// arity logic is covered by `parse_single_client_tty_*`.
pub(crate) fn resolve_switch_target(session: &str) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args(["list-clients", "-t", session])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_single_client_tty(&String::from_utf8_lossy(&output.stdout))
}

/// Print the fail-closed hint when a switch-client target could not be
/// resolved safely (#2678) — the session is ready, but nothing was moved.
fn print_fail_closed_hint(name: &str) {
    eprintln!(
        "tm: session '{name}' is ready, but the attached tmux client could not be resolved \
         unambiguously — refusing to guess (a bare switch-client can silently retarget an \
         unrelated terminal, issue #2678)."
    );
    eprintln!("tm: attach manually with: tmux attach-session -t {name}");
    eprintln!("tm: (from inside another tmux client instead: tmux switch-client -t {name})");
}

/// Move the resolved single client of the CURRENT session into `name` via an
/// explicit `switch-client -c <tty>` (#2678).
///
/// Why: the inside-tmux half of [`tmux_attach`], split out so the fail-closed
/// branches (no current session, no unambiguous client) short-circuit before
/// any tmux mutation is attempted.
/// What: resolves the current session name, then the single attached
/// client's tty via [`resolve_switch_target`]; on success runs
/// `switch-client -c <tty> -t <name>` and returns
/// [`AttachOutcome::Switched`]. When either resolution step fails, prints
/// [`print_fail_closed_hint`] and returns [`AttachOutcome::TargetUnresolved`]
/// WITHOUT invoking tmux at all — never a bare, unguarded `switch-client`.
fn switch_client_to(name: &str) -> anyhow::Result<AttachOutcome> {
    let Some(current_session) = current_tmux_session_name() else {
        print_fail_closed_hint(name);
        return Ok(AttachOutcome::TargetUnresolved);
    };
    let Some(client_tty) = resolve_switch_target(&current_session) else {
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
}
