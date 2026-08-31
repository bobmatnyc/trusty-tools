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
/// Test: covered through `check_tmux_options`; the parse branches are the same
/// two `core::tmux::probe_alternate_screen` pins.
fn probe_mouse(bin: &str) -> Option<bool> {
    let output = crate::core::tmux::run_tmux_with_bin(
        bin,
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

/// Probe the live tmux server and compare it against the configured spec.
///
/// Why: the one entry point `run_doctor` calls.
/// What: resolves the tmux binary, runs the three read-only `show-options`
/// probes, and hands them to [`build_tmux_options_check`] alongside the
/// `tmux:` section of `TrustyToolsConfig`. Read-only — it never sets an
/// option and never starts a server, so a quiet host stays quiet and reports
/// `Unknown`.
/// Test: the decision is covered by `build_tmux_options_check`'s tests; this
/// wrapper is process execution.
pub(super) fn check_tmux_options() -> DoctorCheck {
    let bin = crate::core::tmux::resolve_tmux_binary_or_bare();
    let observed = ObservedTmuxOptions {
        history_limit: crate::core::tmux::probe_history_limit(&bin).ok(),
        mouse: probe_mouse(&bin),
        alternate_screen: crate::core::tmux::probe_alternate_screen(&bin).ok(),
    };
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let expected = crate::core::trusty_tools_config::resolve_tmux_options(&config);
    build_tmux_options_check(observed, &expected)
}

#[cfg(test)]
mod tests {
    use super::*;

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
