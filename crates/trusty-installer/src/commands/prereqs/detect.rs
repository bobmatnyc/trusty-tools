//! Binary detection beyond `which::which` — extra paths and version queries.
//!
//! Why: `which::which` only searches `PATH`, but `claude` is commonly installed
//! in `~/.claude/local/` (the official native installer) or in npm's global bin
//! directory, neither of which is guaranteed to be on PATH when `tctl install`
//! runs inside a managed session. Falling back to these locations avoids false
//! "not found" negatives for users who installed via the official method.
//!
//! What: `detect_prereq_status` checks PATH via an injectable `check_fn`,
//! then probes a caller-supplied extra-path list, and finally calls an injectable
//! `version_fn` to retrieve the version string for the ✓ confirmation line.
//!
//! Test: `tests::detect_*` inject fake `check_fn`/`version_fn` closures to
//! exercise each detection path without touching the real filesystem.

use std::path::PathBuf;

/// The detection result for a single prerequisite binary.
///
/// Why: Callers need to both branch on presence and report the version string
/// in the ✓ confirmation line without probing twice.
///
/// What: `Found { version }` carries an optional version string; `Absent`
/// means the binary was not discovered on PATH or in any fallback location.
///
/// Test: Returned by `detect_prereq_status`; asserted in `tests::detect_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrereqStatus {
    /// Binary found; `version` is `None` when the version probe fails.
    Found {
        /// Version string extracted from the binary's `--version` output.
        version: Option<String>,
    },
    /// Binary not found anywhere.
    Absent,
}

/// Extra lookup directories for `claude` beyond the system PATH.
///
/// Why: The official Claude Code native installer places the binary at
/// `~/.claude/local/claude`; npm's global bin may be at `~/.npm-global/bin`
/// or `~/.local/bin` depending on platform and npm prefix configuration.
///
/// What: Returns a `Vec<PathBuf>` of candidate directories; `~` is expanded to
/// the actual home directory via `dirs::home_dir`. Returns an empty `Vec` when
/// the home directory cannot be determined.
///
/// Test: Unit tests for `detect_prereq_status` inject their own extra-path
/// lists (pointing at temp dirs) so this function's home-dir expansion is
/// never exercised under test.
pub fn claude_extra_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".claude").join("local"),
        home.join(".npm-global").join("bin"),
        home.join(".local").join("bin"),
    ]
}

/// Detect whether a prerequisite binary is available and retrieve its version.
///
/// Why: A two-step strategy (PATH check → extra-path scan) followed by a
/// version probe gives the prereq phase everything it needs to print a ✓ line
/// with the version or an ✗ line with install guidance.
///
/// What:
/// 1. Calls `check_fn(binary)` — mirrors `which::which`; returns `true` if found.
/// 2. If absent, walks `extra_paths` and looks for a file named `binary`.
/// 3. If found anywhere, calls `version_fn(binary_or_full_path)` to get the
///    version string (may return `None` on parse failure or missing binary).
/// 4. Returns `PrereqStatus::Found { version }` or `PrereqStatus::Absent`.
///
/// Test: `tests::detect_found_on_path`, `tests::detect_found_in_extra_path`,
/// `tests::detect_absent`.
pub fn detect_prereq_status(
    binary: &str,
    check_fn: &dyn Fn(&str) -> bool,
    extra_paths: &[PathBuf],
    version_fn: &dyn Fn(&str) -> Option<String>,
) -> PrereqStatus {
    if check_fn(binary) {
        let version = version_fn(binary);
        return PrereqStatus::Found { version };
    }
    for dir in extra_paths {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            let path_str = candidate.to_string_lossy().into_owned();
            let version = version_fn(&path_str);
            return PrereqStatus::Found { version };
        }
    }
    PrereqStatus::Absent
}

/// Parse a version string from a binary's stdout output.
///
/// Why: Different binaries emit version strings in different formats
/// (`tmux 3.4`, `1.2.3`, `Claude Code 1.2.3`). Centralising the parse
/// keeps the real version prober and tests consistent.
///
/// What: Strips leading/trailing whitespace from `raw`, then extracts the
/// last whitespace-delimited token that looks like a version (contains a
/// digit). Falls back to the full trimmed string on failure.
///
/// Test: `tests::parse_version_*`.
pub fn parse_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Find the last token that contains a digit — handles "tmux 3.4",
    // "1.2.3", "Claude Code 1.2.3", etc.
    let version_token = trimmed
        .split_whitespace()
        .rev()
        .find(|token| token.chars().any(|c| c.is_ascii_digit()));
    Some(version_token.unwrap_or(trimmed).to_owned())
}

/// Run a version command for a binary and parse the output.
///
/// Why: The version-argument convention differs between tools — tmux uses
/// `-V`, everything else uses `--version`. This function encodes that table
/// once.
///
/// What: Chooses the right argument based on `binary`, spawns the command,
/// and calls `parse_version` on stdout. Returns `None` on any I/O or parse
/// error.
///
/// Test: Side-effecting (spawns real process); unit tests inject a fake
/// `version_fn` into `detect_prereq_status` instead.
pub fn probe_version(binary: &str) -> Option<String> {
    let arg = if binary == "tmux" { "-V" } else { "--version" };
    let out = std::process::Command::new(binary).arg(arg).output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    parse_version(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Why: A binary on PATH must be found immediately via `check_fn`, and
    /// `version_fn` must be called to fill the version field.
    /// What: Fake check_fn returns true; fake version_fn returns Some("3.4").
    /// Test: This is the test.
    #[test]
    fn detect_found_on_path() {
        let status = detect_prereq_status("tmux", &|_| true, &[], &|_| Some("3.4".to_owned()));
        assert_eq!(
            status,
            PrereqStatus::Found {
                version: Some("3.4".to_owned())
            }
        );
    }

    /// Why: When PATH check fails but the binary exists in an extra path, it
    /// must be detected and the version must be probed.
    /// What: Creates a real temp file for the binary; check_fn returns false
    /// but the file exists in extra_paths.
    /// Test: This is the test.
    #[test]
    fn detect_found_in_extra_path() {
        let dir = TempDir::new().expect("tmp dir");
        let bin_path = dir.path().join("claude");
        fs::write(&bin_path, b"#!/bin/sh\n").expect("write");
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).expect("chmod");
        let extra = vec![dir.path().to_owned()];
        let status =
            detect_prereq_status("claude", &|_| false, &extra, &|_| Some("1.2.3".to_owned()));
        assert_eq!(
            status,
            PrereqStatus::Found {
                version: Some("1.2.3".to_owned())
            }
        );
    }

    /// Why: When PATH check fails and no extra path has the binary, the result
    /// must be `Absent`.
    /// What: check_fn returns false; extra_paths is empty.
    /// Test: This is the test.
    #[test]
    fn detect_absent() {
        let status = detect_prereq_status("tmux", &|_| false, &[], &|_| None);
        assert_eq!(status, PrereqStatus::Absent);
    }

    /// Why: `parse_version` must extract the trailing numeric token from
    /// `tmux`'s `-V` output format.
    /// What: "tmux 3.4" → Some("3.4").
    /// Test: This is the test.
    #[test]
    fn parse_version_tmux_style() {
        assert_eq!(parse_version("tmux 3.4"), Some("3.4".to_owned()));
    }

    /// Why: `parse_version` must handle a bare semver string (claude's format).
    /// What: "1.2.3" → Some("1.2.3").
    /// Test: This is the test.
    #[test]
    fn parse_version_bare_semver() {
        assert_eq!(parse_version("1.2.3"), Some("1.2.3".to_owned()));
    }

    /// Why: Multi-word prefixes ("Claude Code 1.2.3") must return just the
    /// version token.
    /// What: "Claude Code 1.2.3" → Some("1.2.3").
    /// Test: This is the test.
    #[test]
    fn parse_version_multi_word_prefix() {
        assert_eq!(parse_version("Claude Code 1.2.3"), Some("1.2.3".to_owned()));
    }

    /// Why: An empty string must return `None`.
    /// What: "" → None.
    /// Test: This is the test.
    #[test]
    fn parse_version_empty() {
        assert_eq!(parse_version(""), None);
    }

    /// Why: Whitespace-only input must return `None`.
    /// What: "  " → None.
    /// Test: This is the test.
    #[test]
    fn parse_version_whitespace_only() {
        assert_eq!(parse_version("  "), None);
    }
}
