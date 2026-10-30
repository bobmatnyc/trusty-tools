//! The minimum Claude Code version an Architect launch requires (#10459 PR 2).
//!
//! Why (DOC-86 §0, §5): the Architect's PM guard sets `onFailure: "block"`,
//! which Claude Code honours from v2.1.295. An older `claude` ignores the key,
//! so a guard that fails or times out would let the tool call through.
//! What: [`require_architect_claude_version`] runs `claude --version` (bounded
//! by [`crate::core::output_style::CLAUDE_VERSION_PROBE_TIMEOUT`]) and refuses unless it parses to at
//! least [`ARCHITECT_MIN_CLAUDE_VERSION`]. A probe that cannot run, exits
//! non-zero or prints no version refuses too: the check fails closed.
//! Test: `architect_claude_version_tests.rs`
//! (`launch_refuses_below_minimum_version`, `launch_refuses_an_unreadable_version`,
//! `launch_refuses_an_unparsable_version`, `launch_accepts_the_minimum_and_newer`).

use thiserror::Error;

use crate::core::output_style::parse_claude_version;

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
/// Why: every seam that starts an Architect `claude` reaches this before its
/// first write — [`crate::core::session_launch`]'s shared prep body, the
/// resume hook merge and the adapter's prompt build (#10459 fix round).
/// What: [`probe_claude_version`], then [`check_claude_version`].
/// Test: `fleet_init_refuses_an_old_claude_code` (tests/tm_fleet.rs),
/// `architect_version_refusal_10459.rs` (env_serial).
pub fn require_architect_claude_version() -> Result<(u32, u32, u32), ArchitectVersionError> {
    check_claude_version(probe_claude_version())
}

/// `claude --version`'s stdout, or why it could not be read.
///
/// What: spawns `claude --version` through the disclaim helper, bounded by
/// [`crate::core::output_style::CLAUDE_VERSION_PROBE_TIMEOUT`]; a spawn error, a timeout or a non-zero
/// exit is `Err`.
#[cfg(not(test))]
fn probe_claude_version() -> Result<String, String> {
    match crate::core::spawn_disclaim::disclaimed_stdout_with_timeout(
        "claude",
        &["--version".to_owned()],
        crate::core::output_style::CLAUDE_VERSION_PROBE_TIMEOUT,
    ) {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => Err(format!(
            "exited with status {}",
            out.status
                .code()
                .map_or_else(|| "none".to_owned(), |c| c.to_string())
        )),
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(test)]
thread_local! {
    /// The answer the unit-test probe gives on this thread; a passing version
    /// by default, so a test launching an Architect never depends on the host.
    pub(crate) static TEST_PROBE: std::cell::RefCell<Result<String, String>> =
        std::cell::RefCell::new(Ok("2.1.312 (Claude Code)".to_owned()));
}

/// The unit-test probe: this thread's [`TEST_PROBE`] answer, no spawn.
#[cfg(test)]
fn probe_claude_version() -> Result<String, String> {
    TEST_PROBE.with(|probe| probe.borrow().clone())
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

/// [`require_for_profile`] for a launch in `dir`, its profile resolved
/// against the operator's config (#10459): the early check `tm launch` and
/// `tm connect` make before any clone, worktree or settings write.
///
/// Test: `connect_refuses_an_old_claude_code_before_any_write`,
/// `launch_refuses_an_old_claude_code_before_any_write`.
pub fn require_for_dir(dir: &std::path::Path) -> Result<(), ArchitectVersionError> {
    require_for_profile(crate::core::session_profile::resolve_ambient(dir))
}

#[cfg(test)]
#[path = "architect_claude_version_tests.rs"]
mod tests;
