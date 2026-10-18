//! `tm fleet relaunch` refuses to run inside a Claude Code session (#10033).
//!
//! Why: the pm-guard floor reads Bash command text, and a command-text parser
//! can be bypassed. Owner ruling D8a: `tm fleet relaunch` itself refuses when a
//! hooked Claude session runs it, so a parser bypass still cannot relaunch.
//! What: [`refuse_inside_claude_code`] reads Claude Code's child-process
//! markers ([`SESSION_MARKERS`]). Claude Code's child-process env injector
//! sets all four in every process it starts, the Bash tool's shell included
//! (`core::claude_env_scrub` cites the bundle). An operator shell carries none;
//! tm scrubs them from every `claude`, tmux server and daemon it spawns. Any
//! marker present refuses: a Claude Code value, a non-UTF-8 value (unreadable)
//! and any other value (ambiguous) alike. The message names each variable and
//! its reason, never its value.
//! Test: `session_markers_refuse_and_name_no_value`,
//! `an_unreadable_or_ambiguous_marker_refuses_and_names_the_reason`,
//! `no_marker_lets_relaunch_run`,
//! `fleet_relaunch_refuses_inside_a_claude_code_session`,
//! `fleet_relaunch_refuses_an_unreadable_session_marker`.

use std::ffi::OsString;

/// The markers Claude Code's child-process env injector sets unconditionally.
///
/// Why: one marker alone is cleared by one `unset`; the injector's whole
/// unconditional set is what every Bash tool child inherits.
/// What: the variable names, `CLAUDECODE` (the documented "running inside
/// Claude Code" marker) first.
pub(crate) const SESSION_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_PID",
];

/// Why one marker refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    /// The value is one Claude Code sets.
    Session,
    /// The value is not UTF-8, so it cannot be read.
    Unreadable,
    /// The value is readable but not one Claude Code sets.
    Ambiguous,
}

impl Reason {
    fn describe(self) -> &'static str {
        match self {
            Self::Session => "is set, as Claude Code sets it in every process it starts",
            Self::Unreadable => "is set but its value is not UTF-8, so it cannot be read",
            Self::Ambiguous => "is set to a value Claude Code does not set, so it is ambiguous",
        }
    }
}

/// Whether `value` is one Claude Code sets for `name`.
fn claude_value(name: &str, value: &str) -> bool {
    match name {
        "CLAUDECODE" | "CLAUDE_CODE_CHILD_SESSION" => value == "1",
        "CLAUDE_PID" => value.parse::<u32>().is_ok_and(|pid| pid > 0),
        _ => !value.trim().is_empty(),
    }
}

/// Each present marker with its [`Reason`], in [`SESSION_MARKERS`] order.
///
/// Why: separated from the message so a test asserts on the classification.
/// What: an absent variable is skipped; every present one is classified.
pub(crate) fn present_markers(
    env: impl Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, Reason)> {
    SESSION_MARKERS
        .iter()
        .filter_map(|&name| {
            let value = env(name)?;
            let reason = match value.to_str() {
                None => Reason::Unreadable,
                Some(v) if claude_value(name, v) => Reason::Session,
                Some(_) => Reason::Ambiguous,
            };
            Some((name, reason))
        })
        .collect()
}

/// `Err` naming every present marker when this process runs inside a Claude
/// Code session; `Ok` when no marker is present.
///
/// Why: see the module doc. Fail closed: a marker tm cannot read, or whose
/// value Claude Code never sets, refuses as a Claude Code value does.
/// What: [`present_markers`] over `env` (production passes
/// `std::env::var_os`); the error lists each name and reason, then names the
/// operator terminal as the place to run the command. No value is printed.
/// Test: `session_markers_refuse_and_name_no_value`,
/// `an_unreadable_or_ambiguous_marker_refuses_and_names_the_reason`,
/// `no_marker_lets_relaunch_run`.
pub(crate) fn refuse_inside_claude_code(
    env: impl Fn(&str) -> Option<OsString>,
) -> anyhow::Result<()> {
    let present = present_markers(env);
    if present.is_empty() {
        return Ok(());
    }
    let reasons: Vec<String> = present
        .iter()
        .map(|(name, reason)| format!("`{name}` {}", reason.describe()))
        .collect();
    anyhow::bail!(
        "tm fleet relaunch refused: it runs inside a Claude Code session ({}). Nothing was \
         stopped. Run `tm fleet relaunch` from an operator terminal outside any Claude Code \
         session (#10033)",
        reasons.join("; ")
    )
}

#[cfg(test)]
#[path = "inside_claude_tests.rs"]
mod tests;
