//! `tm doctor` probe: do the tmux SERVER's globals match tm's spec (#6469)?
//!
//! Why: `create_managed_session` applies `history-limit`, `mouse` and
//! `alternate-screen` before every pane tm creates, and verifies them (#3386).
//! Nothing verifies the server tm did NOT create the panes on. A tmux server
//! restart with tmux-continuum restore recreates every `tm-*` session through
//! resurrect's own bare `new-session`, so those panes bake tmux's factory
//! 2000-line `history-limit` and can enter the alternate screen — scrollback
//! collapses to one screen and nothing says so (observed on `tm-dogfood`,
//! server restart 2026-08-27). The daemon now re-asserts the globals at boot
//! reconcile; this is the probe that says whether they actually hold, for a
//! server that came up any other way.
//!
//! **A pane already created keeps the limit it was born with.** `history-limit`
//! is captured into a pane's ring buffer AT CREATION TIME, so a green row here
//! means new panes are correct, never that an existing pane was repaired. The
//! remedy this check names is therefore restarting the affected session, not
//! re-running a `set-option`.
//!
//! What: [`check_tmux_options`] reads the configured spec, probes the live
//! server, and folds the two into a [`DoctorCheck`]. [`build_tmux_options_check`]
//! is the pure decision over already-gathered values, so every branch is
//! testable without a tmux server.
//! Test: the `tests` module below.

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::trusty_tools_config::ResolvedTmuxOptions;

/// Stable check name.
pub(super) const CHECK_NAME: &str = "tmux_options";

/// What the live tmux server reports for each option tm specifies.
///
/// Why: each option is probed independently and any of them can fail on its
/// own (an old tmux that does not know the option, a server that went away
/// between two probes). Folding a failed probe into a default would report a
/// drift that was never observed, so "not observed" is its own value.
/// What: `None` means the probe could not read that option.
/// Test: `unreadable_options_report_unknown`, `matching_options_report_ok`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ObservedTmuxOptions {
    /// Server-global `history-limit`, or `None` when unreadable.
    pub history_limit: Option<u32>,
    /// Server-global `mouse`, or `None` when unreadable.
    pub mouse: Option<bool>,
    /// Window-global `alternate-screen` (#5151), or `None` when unreadable.
    pub alternate_screen: Option<bool>,
}

/// Render a tmux boolean the way tmux itself does.
fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// Decide the check from an already-gathered observation and the configured
/// spec.
///
/// Why: pure, so the drift/agreement/unreadable branches are testable with no
/// tmux server and no `$HOME`.
/// What: `Unknown` when no option could be read at all — nothing was learned,
/// and a clean bill of health would be a lie. `Warn` naming every option that
/// disagrees, plus the restart remedy, since an existing pane cannot be
/// repaired in place. `Ok` when every option that COULD be read agrees, naming
/// the ones that could not.
/// Test: `matching_options_report_ok`, `drifted_history_limit_warns`,
/// `unreadable_options_report_unknown`, `a_partially_readable_server_still_reports`.
pub(super) fn build_tmux_options_check(
    observed: ObservedTmuxOptions,
    expected: &ResolvedTmuxOptions,
) -> DoctorCheck {
    let mut drift: Vec<String> = Vec::new();
    let mut unread: Vec<&str> = Vec::new();

    match observed.history_limit {
        Some(got) if got != expected.history_limit => drift.push(format!(
            "history-limit is {got}, tm specifies {}",
            expected.history_limit
        )),
        Some(_) => {}
        None => unread.push("history-limit"),
    }
    match observed.mouse {
        Some(got) if got != expected.mouse => drift.push(format!(
            "mouse is {}, tm specifies {}",
            on_off(got),
            on_off(expected.mouse)
        )),
        Some(_) => {}
        None => unread.push("mouse"),
    }
    match observed.alternate_screen {
        Some(got) if got != expected.alternate_screen => drift.push(format!(
            "alternate-screen is {}, tm specifies {}",
            on_off(got),
            on_off(expected.alternate_screen)
        )),
        Some(_) => {}
        None => unread.push("alternate-screen"),
    }

    if unread.len() == 3 {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "tmux server options could not be read (no tmux binary, or no server running) — \
             whether restored panes carry tm's scrollback settings is unknown",
        );
    }

    if drift.is_empty() {
        let suffix = if unread.is_empty() {
            String::new()
        } else {
            format!(" (not readable: {})", unread.join(", "))
        };
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!("tmux server globals match tm's spec{suffix}"),
        );
    }

    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "tmux server globals drifted from tm's spec (#6469): {}. A server tm did not start \
             — a tmux-resurrect/continuum restore, or a manual `tmux new-session` — carries none \
             of them, so restored panes sit on tmux's factory 2000-line scrollback. Restart the \
             daemon to re-assert them, then restart the affected sessions: `history-limit` is \
             captured when a pane is created and cannot be grown in place.",
            drift.join("; ")
        ),
    )
}

/// Read the `mouse` server global.
///
/// Why: `core::tmux` already owns the `history-limit` and `alternate-screen`
/// readbacks (they gate every managed create), but nothing needed `mouse` until
/// this check — tm sets it and never verified it.
/// What: `show-options -g -v mouse`, mapping `on`/`off`. Anything else is
/// `None`: a value tmux does not document must not be guessed at, because a
/// wrong guess here reports agreement that was never observed.
/// #10101: on `server` (`None` for the default one).
/// Test: covered through `check_tmux_options`; the parse branches are the same
/// two `core::tmux::probe_alternate_screen` pins.
fn probe_mouse(bin: &str, server: Option<&str>) -> Option<bool> {
    let output = crate::core::tmux::run_tmux_on(
        bin,
        server,
        &crate::core::tmux::TmuxCommand::ShowGlobalOption {
            name: crate::core::tmux::MOUSE_OPTION.to_string(),
        },
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&output.stdout).trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// The three option probes against one server (#10101: `None` is the
/// default server).
fn observe_server(bin: &str, server: Option<&str>) -> ObservedTmuxOptions {
    ObservedTmuxOptions {
        history_limit: crate::core::tmux::probe_history_limit(bin, server).ok(),
        mouse: probe_mouse(bin, server),
        alternate_screen: crate::core::tmux::probe_alternate_screen(bin, server).ok(),
    }
}

/// Fold one check per tmux server into the one row the doctor shows (#10101).
///
/// Why: a PM on a workspace server gets that server's globals, not the
/// default server's, so the default server's row says nothing about it.
/// What: the default server's check alone is returned unchanged, so a host
/// with no workspace server reports exactly as before. Otherwise the worst
/// status, and one `<server>: <message>` clause per server.
/// Test: `one_server_reports_unchanged`, `a_workspace_server_is_reported_by_name`.
pub(super) fn fold_server_checks(mut checks: Vec<(String, DoctorCheck)>) -> DoctorCheck {
    if checks.len() == 1 {
        return checks.remove(0).1;
    }
    let status = checks
        .iter()
        .fold(CheckStatus::Ok, |worst, (_, c)| worst.worst(c.status));
    let message = checks
        .iter()
        .map(|(server, c)| format!("{server}: {}", c.message))
        .collect::<Vec<_>>()
        .join(" | ");
    DoctorCheck::new(CHECK_NAME, status, message)
}

/// Probe each live tmux server and compare it against the configured spec.
///
/// Why: the one entry point `run_doctor` calls.
/// What: resolves the tmux binary, runs the three read-only `show-options`
/// probes on the default server and (#10101) on every live workspace server,
/// decides each with [`build_tmux_options_check`] against the `tmux:` section
/// of `TrustyToolsConfig`, and folds them with [`fold_server_checks`].
/// Read-only — it never sets an option and never starts a server, so a quiet
/// host stays quiet and reports `Unknown`.
/// Test: the decision is covered by `build_tmux_options_check`'s and
/// `fold_server_checks`'s tests; the per-server bound by
/// `a_hung_workspace_server_reads_unknown_within_the_budget`.
pub(super) fn check_tmux_options() -> DoctorCheck {
    let bin = crate::core::tmux::resolve_tmux_binary_or_bare();
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let expected = crate::core::trusty_tools_config::resolve_tmux_options(&config);
    let socket_dir = crate::core::tmux_server::ambient_socket_dir();
    tmux_options_row(
        &bin,
        &socket_dir,
        &expected,
        crate::core::tmux_server::PROBE_BUDGET,
    )
}

/// [`check_tmux_options`] over a given tmux binary, socket directory and
/// per-server probe budget (#10101).
///
/// Why: a hung workspace server would block an unbounded `show-options` and
/// with it the whole `tm doctor` run.
/// What: the default server is probed as before. Each live workspace server
/// in `socket_dir` is probed through
/// [`crate::core::tmux_server::bounded_probe`] keyed on its socket: within
/// `budget`, and not at all while an earlier probe of it is still blocked.
/// Such a server's clause is `Unknown` naming the server (#4005: a probe that
/// timed out learned nothing), so the row is never `Ok`.
/// Test: `a_hung_workspace_server_reads_unknown_within_the_budget`.
pub(super) fn tmux_options_row(
    bin: &str,
    socket_dir: &std::path::Path,
    expected: &ResolvedTmuxOptions,
    budget: std::time::Duration,
) -> DoctorCheck {
    // An unreadable socket directory adds no servers; the default row stands.
    let servers = crate::core::tmux_server::live_workspace_servers(socket_dir).unwrap_or_default();
    let mut checks = vec![(
        "default server".to_owned(),
        build_tmux_options_check(observe_server(bin, None), expected),
    )];
    checks.extend(servers.into_iter().map(|server| {
        let key = crate::core::tmux_server::server_socket_path(socket_dir, &server);
        let (probe_bin, probe_server) = (bin.to_owned(), server.clone());
        // #10101: bounded, one in flight per server.
        let observed = crate::core::tmux_server::bounded_probe(&key, budget, move || {
            observe_server(&probe_bin, Some(&probe_server))
        });
        let check = match observed {
            Some(observed) => build_tmux_options_check(observed, expected),
            None => DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Unknown,
                format!(
                    "tmux server '{server}' did not answer within {}s; its options were not \
                     checked",
                    budget.as_secs_f32()
                ),
            ),
        };
        (server, check)
    }));
    fold_server_checks(checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_server_reports_unchanged() {
        let only = DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, "fine");
        let folded = fold_server_checks(vec![("default server".into(), only.clone())]);
        assert_eq!(folded.message, only.message);
        assert_eq!(folded.status, only.status);
    }

    #[test]
    fn a_workspace_server_is_reported_by_name() {
        let folded = fold_server_checks(vec![
            (
                "default server".into(),
                DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, "fine"),
            ),
            (
                "tm-ws-x".into(),
                DoctorCheck::new(CHECK_NAME, CheckStatus::Warn, "drifted"),
            ),
        ]);
        assert_eq!(folded.status, CheckStatus::Warn);
        assert_eq!(folded.message, "default server: fine | tm-ws-x: drifted");
    }

    /// #10101 error arm (Fail-Open Check): a live workspace server whose
    /// `show-options` hangs. The row returns within the budget, reads
    /// `Unknown` naming the server, and a second run while the first probe
    /// still blocks spawns no second tmux client. Red on ed04279554, whose
    /// unbounded probe waited out every hung call.
    #[test]
    fn a_hung_workspace_server_reads_unknown_within_the_budget() {
        let dir = crate::test_support::hermetic_temp_dir();
        let server = "tm-ws-10101d-opts";
        // A socket that accepts a connect, so the server reads live.
        let _listener =
            std::os::unix::net::UnixListener::bind(dir.path().join(server)).expect("bind");
        let log = dir.path().join("calls.log");
        let bin = dir.path().join("fake-tmux");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$1\" = -L ]; then sleep 3; fi\nexit 1\n",
            log.display()
        );
        trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
        let bin = bin.to_string_lossy().into_owned();
        let budget = std::time::Duration::from_millis(300);

        let started = std::time::Instant::now();
        let row = tmux_options_row(&bin, dir.path(), &spec(), budget);
        let took = started.elapsed();

        assert!(took < std::time::Duration::from_secs(2), "took {took:?}");
        assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
        assert!(
            row.message
                .contains(&format!("{server}: tmux server '{server}' did not answer")),
            "{}",
            row.message
        );
        let again = tmux_options_row(&bin, dir.path(), &spec(), budget);
        assert_eq!(again.status, CheckStatus::Unknown, "{}", again.message);
        let calls = std::fs::read_to_string(&log).expect("log");
        let on_server = calls.lines().filter(|l| l.starts_with("-L ")).count();
        assert_eq!(on_server, 1, "the hung server was probed again: {calls}");
    }

    /// The spec these tests compare against — tm's shipped defaults.
    fn spec() -> ResolvedTmuxOptions {
        ResolvedTmuxOptions {
            history_limit: 100_000,
            mouse: true,
            alternate_screen: false,
        }
    }

    #[test]
    fn matching_options_report_ok() {
        let check = build_tmux_options_check(
            ObservedTmuxOptions {
                history_limit: Some(100_000),
                mouse: Some(true),
                alternate_screen: Some(false),
            },
            &spec(),
        );
        assert_eq!(check.status, CheckStatus::Ok);
    }

    /// The #6469 signature: a restored server on tmux's factory 2000.
    #[test]
    fn drifted_history_limit_warns() {
        let check = build_tmux_options_check(
            ObservedTmuxOptions {
                history_limit: Some(2000),
                mouse: Some(true),
                alternate_screen: Some(false),
            },
            &spec(),
        );
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.message.contains("history-limit is 2000"),
            "{}",
            check.message
        );
        assert!(check.message.contains("100000"), "{}", check.message);
    }

    /// alternate-screen ON is the other half of the restore signature: the
    /// pane leaves no scrollback behind at all.
    #[test]
    fn drifted_alternate_screen_warns() {
        let check = build_tmux_options_check(
            ObservedTmuxOptions {
                history_limit: Some(100_000),
                mouse: Some(true),
                alternate_screen: Some(true),
            },
            &spec(),
        );
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.message.contains("alternate-screen is on"),
            "{}",
            check.message
        );
    }

    /// No tmux, or no server: nothing was learned, so the row must not read as
    /// a clean bill of health.
    #[test]
    fn unreadable_options_report_unknown() {
        let check = build_tmux_options_check(ObservedTmuxOptions::default(), &spec());
        assert_eq!(check.status, CheckStatus::Unknown);
    }

    /// A server that answers some probes and not others still reports on the
    /// ones it answered, naming the rest.
    #[test]
    fn a_partially_readable_server_still_reports() {
        let check = build_tmux_options_check(
            ObservedTmuxOptions {
                history_limit: Some(100_000),
                mouse: None,
                alternate_screen: None,
            },
            &spec(),
        );
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.message.contains("mouse"), "{}", check.message);
        assert!(
            check.message.contains("alternate-screen"),
            "{}",
            check.message
        );
    }
}
