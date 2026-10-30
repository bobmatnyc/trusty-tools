//! The minimum Claude Code version an Architect launch requires (#10459 PR 2).
//!
//! Why (DOC-86 §0, §5): the Architect's PM guard sets `onFailure: "block"`,
//! which Claude Code honours from v2.1.295. An older `claude` ignores the key,
//! so a guard that fails or times out would let the tool call through.
//! What: [`require_architect_claude_version`] runs `claude --version` (bounded
//! by [`CLAUDE_VERSION_PROBE_TIMEOUT`]) and refuses unless it parses to at
//! least [`ARCHITECT_MIN_CLAUDE_VERSION`]. A probe that cannot run, exits
//! non-zero or prints no version refuses too: the check fails closed.
//! Test: `architect_claude_version_tests.rs`
//! (`launch_refuses_below_minimum_version`, `launch_refuses_an_unreadable_version`,
//! `launch_refuses_an_unparsable_version`, `launch_accepts_the_minimum_and_newer`).

use thiserror::Error;

use crate::core::output_style::{CLAUDE_VERSION_PROBE_TIMEOUT, parse_claude_version};

/// The oldest Claude Code an Architect launches on: v2.1.295, the first with
/// `onFailure` (DOC-86 §0).
pub const ARCHITECT_MIN_CLAUDE_VERSION: (u32, u32, u32) = (2, 1, 295);

/// Why an Architect launch was refused on the Claude Code version.
///
/// Test: one arm per test in `architect_claude_version_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ArchitectVersionError {
    /// `claude --version` could not run, timed out or exited non-zero.
    #[error(
        "refusing the Architect launch (#10459): cannot read the Claude Code version \
         (`claude --version`: {0}); the Architect needs Claude Code v{min} or later",
        min = fmt_version(ARCHITECT_MIN_CLAUDE_VERSION)
    )]
    Unreadable(String),
    /// `claude --version` printed no `major.minor.patch` version.
    #[error(
        "refusing the Architect launch (#10459): `claude --version` printed no version \
         ({0:?}); the Architect needs Claude Code v{min} or later",
        min = fmt_version(ARCHITECT_MIN_CLAUDE_VERSION)
    )]
    Unparsable(String),
    /// The installed Claude Code is older than the minimum.
    #[error(
        "refusing the Architect launch (#10459): Claude Code v{found} is older than v{min}, \
         the first that honours `onFailure: \"block\"` on the PM guard; run `claude update`",
        found = fmt_version(*.0),
        min = fmt_version(ARCHITECT_MIN_CLAUDE_VERSION)
    )]
    TooOld((u32, u32, u32)),
}

/// `(2, 1, 295)` as `2.1.295`.
fn fmt_version((major, minor, patch): (u32, u32, u32)) -> String {
    format!("{major}.{minor}.{patch}")
}

/// Judge one `claude --version` probe result.
///
/// Why: the decision without the spawn, so every refusal arm is testable.
/// What: `Err(reason)` is [`ArchitectVersionError::Unreadable`]; stdout with
/// no version triple is [`ArchitectVersionError::Unparsable`]; a version below
/// [`ARCHITECT_MIN_CLAUDE_VERSION`] is [`ArchitectVersionError::TooOld`].
/// Otherwise the parsed version, which is `>=` the minimum.
/// Test: `launch_refuses_below_minimum_version`, `launch_refuses_an_unreadable_version`,
/// `launch_refuses_an_unparsable_version`, `launch_accepts_the_minimum_and_newer`.
pub fn check_claude_version(
    probe: Result<String, String>,
) -> Result<(u32, u32, u32), ArchitectVersionError> {
    let stdout = probe.map_err(ArchitectVersionError::Unreadable)?;
    let Some(version) = parse_claude_version(&stdout) else {
        let shown: String = stdout.trim().chars().take(80).collect();
        return Err(ArchitectVersionError::Unparsable(shown));
    };
    if version < ARCHITECT_MIN_CLAUDE_VERSION {
        return Err(ArchitectVersionError::TooOld(version));
    }
    Ok(version)
}

/// Run `claude --version` and judge it with [`check_claude_version`].
///
/// Why: every Architect launch seam (`tm fleet init`, `tm fleet relaunch`,
/// `tm launch`) calls this before it starts a `claude`.
/// What: spawns `claude --version` through the disclaim helper, bounded by
/// [`CLAUDE_VERSION_PROBE_TIMEOUT`]; a spawn error, a timeout or a non-zero
/// exit is the `Err` probe.
/// Test: `fleet_init_refuses_an_old_claude_code` (tests/tm_fleet.rs).
pub fn require_architect_claude_version() -> Result<(u32, u32, u32), ArchitectVersionError> {
    let probe = match crate::core::spawn_disclaim::disclaimed_stdout_with_timeout(
        "claude",
        &["--version".to_owned()],
        CLAUDE_VERSION_PROBE_TIMEOUT,
    ) {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => Err(format!(
            "exited with status {}",
            out.status
                .code()
                .map_or_else(|| "none".to_owned(), |c| c.to_string())
        )),
        Err(err) => Err(err.to_string()),
    };
    check_claude_version(probe)
}

/// [`require_architect_claude_version`] for an Architect `profile`; a PM or
/// agent launch is never probed.
///
/// Test: `a_pm_profile_is_never_probed`.
pub fn require_for_profile(
    profile: crate::core::session_profile::SessionProfile,
) -> Result<(), ArchitectVersionError> {
    if profile.is_architect() {
        require_architect_claude_version()?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "architect_claude_version_tests.rs"]
mod tests;
