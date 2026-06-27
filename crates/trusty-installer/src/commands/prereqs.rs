//! External prerequisite validation for `trusty-installer install`.
//!
//! Why: Several trusty-* tools require external binaries that `cargo install`
//! will not pull in. Probing before install lets the operator fix the environment
//! before a half-installed stack produces confusing runtime errors.
//!
//! What: Defines a prerequisite table (binary, what it is needed for, install
//! hint) and `check_and_warn` which runs the table against the selected crates,
//! emitting warnings for any missing tools. Only tools relevant to the selected
//! members are checked.
//!
//! Test: `tests` validates the relevance filtering and the missing-tool detection
//! using an injected checker function instead of real `which::which` calls.

use crate::commands::progress_ui::narrator;

/// A single external prerequisite.
///
/// Why: Bundles the binary name, description, affected crates, and install hint
/// so the check loop is data-driven and adding a new prereq is a one-liner.
///
/// What: `binary` is the executable name probed with the checker; `required_for`
/// is the human label; `affects` is the set of stable-set crate/binary names
/// this prereq gates; `hint` is the install guidance emitted when missing.
///
/// Test: Used by `PREREQS` table and `check_and_warn`.
pub struct Prereq {
    /// The binary to look up (e.g. "tmux").
    pub binary: &'static str,
    /// Human-readable label for what this binary is needed for.
    pub required_for: &'static str,
    /// Stable-set crate or binary names that need this prerequisite.
    pub affects: &'static [&'static str],
    /// Install guidance printed when the binary is missing.
    pub hint: &'static str,
}

/// A missing prerequisite returned by `check_and_warn`.
///
/// Why: Callers may want to block on some missing tools (trusty-mpm → tmux)
/// while only warning for others; returning a typed list lets the caller decide.
///
/// What: Holds the `binary` name and the `hint` string.
///
/// Test: `tests::check_returns_missing_for_injected_absent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// The missing binary name.
    pub binary: String,
    /// Install guidance for the missing binary.
    pub hint: String,
}

/// The canonical prerequisite table.
///
/// Why: Data-driven table means adding a new prereq requires no code changes
/// in the check logic — just a new entry here.
///
/// What: Three prereqs — `claude` (Claude Code / claude-mpm), `git` (project
/// operations), and `tmux` (trusty-mpm session management).
///
/// Test: `tests::prereq_table_has_expected_entries`.
pub const PREREQS: &[Prereq] = &[
    Prereq {
        binary: "claude",
        required_for: "Claude Code (claude-mpm)",
        affects: &["trusty-mpm"],
        hint: "Install Claude Code: https://claude.ai/download",
    },
    Prereq {
        binary: "git",
        required_for: "project operations",
        affects: &[
            "trusty-search",
            "trusty-memory",
            "trusty-analyze",
            "trusty-review",
            "tga",
            "trusty-console",
            "trusty-mpm",
        ],
        hint: "Install git: https://git-scm.com/downloads",
    },
    Prereq {
        binary: "tmux",
        required_for: "trusty-mpm session management",
        affects: &["trusty-mpm"],
        hint: "Install tmux: `brew install tmux` (macOS) or `apt install tmux` (Debian/Ubuntu)",
    },
];

/// Check for missing prerequisites relevant to the selected crates.
///
/// Why: Only check prerequisites actually needed by the selected members — no
/// false positives for tools the user hasn't installed (and doesn't need) yet.
///
/// What: For each entry in `PREREQS`, checks if any selected crate/binary name
/// appears in the prereq's `affects` list; if so, runs `check_fn` to probe
/// for the binary. Returns a `Vec<Missing>` for absent tools. Emits a warning
/// line to stderr for each missing tool (suppressed under `--json` if `json` is
/// true — the raw missing list is returned for the caller to format).
///
/// Test: `tests::check_filters_by_selection`, `tests::check_returns_missing_for_injected_absent`,
/// `tests::check_no_warn_for_unrelated`.
pub fn check_and_warn(
    selected: &[String],
    json: bool,
    check_fn: impl Fn(&str) -> bool,
) -> Vec<Missing> {
    let narr = narrator(json);
    let mut missing = Vec::new();
    for prereq in PREREQS {
        let relevant = prereq
            .affects
            .iter()
            .any(|affected| selected.iter().any(|s| s == affected));
        if !relevant {
            continue;
        }
        if !check_fn(prereq.binary) {
            let m = Missing {
                binary: prereq.binary.to_owned(),
                hint: prereq.hint.to_owned(),
            };
            if !json {
                let _ = narr.info(&format!(
                    "warning: `{}` not found on PATH (needed for {}). {}",
                    prereq.binary, prereq.required_for, prereq.hint
                ));
            }
            missing.push(m);
        }
    }
    missing
}

/// Convenience wrapper using real `which::which` as the checker.
///
/// Why: Production code calls this; tests inject a fake checker via
/// `check_and_warn` directly.
///
/// What: Calls `check_and_warn` with `which::which` as the probe function,
/// returning the missing list.
///
/// Test: Exercised indirectly by `install::run` integration path; unit tests
/// use `check_and_warn` with a fake checker.
pub fn check_and_warn_real(selected: &[String], json: bool) -> Vec<Missing> {
    check_and_warn(selected, json, |bin| which::which(bin).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The table must contain entries for claude, git, and tmux.
    /// What: Asserts all three binary names are present.
    /// Test: This is the test.
    #[test]
    fn prereq_table_has_expected_entries() {
        let binaries: Vec<&str> = PREREQS.iter().map(|p| p.binary).collect();
        assert!(binaries.contains(&"claude"), "claude prereq missing");
        assert!(binaries.contains(&"git"), "git prereq missing");
        assert!(binaries.contains(&"tmux"), "tmux prereq missing");
    }

    /// Why: Only prereqs for selected crates should be checked.
    /// What: Selects only "tga" (no mpm); asserts tmux and claude are NOT in the
    /// missing list even when the fake checker says everything is absent.
    /// Test: This is the test.
    #[test]
    fn check_filters_by_selection() {
        let selected = vec!["tga".to_owned()];
        // Fake checker: always says binary is absent.
        let missing = check_and_warn(&selected, true, |_| false);
        // git affects tga, but tmux and claude do not.
        let binaries: Vec<&str> = missing.iter().map(|m| m.binary.as_str()).collect();
        assert!(
            !binaries.contains(&"tmux"),
            "tmux should not be checked for tga-only install"
        );
        assert!(
            !binaries.contains(&"claude"),
            "claude should not be checked for tga-only install"
        );
        // git SHOULD appear since it affects tga.
        assert!(
            binaries.contains(&"git"),
            "git should be flagged as missing"
        );
    }

    /// Why: When a relevant binary is absent, it must appear in the returned list.
    /// What: Selects "trusty-mpm"; fake checker says tmux is absent; asserts tmux
    /// appears in the missing list with the correct hint.
    /// Test: This is the test.
    #[test]
    fn check_returns_missing_for_injected_absent() {
        let selected = vec!["trusty-mpm".to_owned()];
        let missing = check_and_warn(&selected, true, |bin| bin != "tmux");
        let tmux_miss = missing.iter().find(|m| m.binary == "tmux");
        assert!(tmux_miss.is_some(), "tmux should be missing");
        assert!(
            tmux_miss.unwrap().hint.contains("tmux"),
            "hint should mention tmux"
        );
    }

    /// Why: When all binaries are present, no missing list is returned.
    /// What: Fake checker always returns true; asserts empty missing list.
    /// Test: This is the test.
    #[test]
    fn check_no_missing_when_all_present() {
        let selected = vec!["trusty-mpm".to_owned(), "trusty-search".to_owned()];
        let missing = check_and_warn(&selected, true, |_| true);
        assert!(missing.is_empty());
    }

    /// Why: An empty selection should produce no warnings (nothing to check).
    /// What: Passes empty selection with always-absent checker; asserts empty result.
    /// Test: This is the test.
    #[test]
    fn check_empty_selection_no_warnings() {
        let selected: Vec<String> = vec![];
        let missing = check_and_warn(&selected, true, |_| false);
        assert!(missing.is_empty());
    }
}
