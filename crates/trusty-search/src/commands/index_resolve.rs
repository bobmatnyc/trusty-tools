//! Shared "what index are we operating on?" helpers used by the project-scoped
//! CLI subcommands (`search`, `watch`, `add`, `remove`, `reindex`).
//!
//! Why: every project subcommand needs the same precedence rules: an explicit
//! `--index` flag wins, otherwise auto-detect from CWD; and the same user-facing
//! warning when we fall back to the bare directory name. Centralising removes
//! duplication and shrinks `main.rs`.
//! What: two thin helpers wrapping `crate::detect`.
//! Test: covered indirectly by `cargo run -- search foo` (explicit index path)
//! and `cargo run -- search foo` from inside this repo (auto-detect path).

use crate::detect::{detect_project, DetectionMethod};
use anyhow::Result;
use colored::Colorize;

/// Resolve the effective index ID: explicit `--index` flag wins, otherwise
/// auto-detect from CWD via `detect_project`.
///
/// Why: every project-scoped command needs the same precedence rules.
/// What: returns `(index_id, warned)` where `warned` is true when we fell back
/// to the CWD basename and should print a warning. Errors when detection
/// refuses the resolved root (#6550) — running from `$HOME` derived an index id
/// naming the operator, and no command should proceed against that. An explicit
/// `--index` is checked first and never reaches detection, so it stays the way
/// out.
/// Test: With explicit Some("foo") → returns ("foo", false). With None inside
/// this repo → returns ("trusty-search", false) (detected via .git).
/// `resolve_index_explicit_wins`, `resolve_index_none_falls_through_to_detection`.
pub fn resolve_index(explicit: &Option<String>) -> Result<(String, bool)> {
    if let Some(id) = explicit {
        return Ok((id.clone(), false));
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let ctx = detect_project(&cwd)?;
    let warned = matches!(ctx.detection_method, DetectionMethod::Fallback);
    Ok((ctx.index_id, warned))
}

/// Why: Make fallback detection visible so users know to run `init`.
/// What: Prints a one-line yellow warning to stderr if `warned` is true.
/// Test: Call with warned=true and capture stderr → contains "⚠".
pub fn print_index_header(index_id: &str, warned: bool) {
    if warned {
        eprintln!(
            "{} No .git or .trusty-search found — using directory name '{}'. \
             Run `trusty-search init` to register this project.",
            "⚠".yellow(),
            index_id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_index_explicit_wins() {
        let explicit = Some("my-explicit-index".to_string());
        let (id, warned) = resolve_index(&explicit).expect("explicit never detects");
        assert_eq!(id, "my-explicit-index");
        assert!(!warned, "explicit --index should never warn");
    }

    #[test]
    fn resolve_index_explicit_empty_string_still_wins() {
        // Even an empty string is preferred over auto-detect: callers are
        // responsible for trimming/validating.
        let explicit = Some(String::new());
        let (id, warned) = resolve_index(&explicit).expect("explicit never detects");
        assert_eq!(id, "");
        assert!(!warned);
    }

    #[test]
    fn resolve_index_none_falls_through_to_detection() {
        // We can't assert the exact id (depends on CWD when test runs) but we
        // can assert the contract: returns a non-empty index_id, doesn't panic.
        let (id, _warned) = resolve_index(&None).expect("the test CWD is a real project");
        assert!(
            !id.is_empty(),
            "auto-detected index_id should never be empty"
        );
    }

    #[test]
    fn print_index_header_warned_false_is_silent() {
        // When warned=false the function MUST NOT panic and should return cleanly.
        // (stderr capture varies by test runner; we just smoke-test the call.)
        print_index_header("any-id", false);
    }

    #[test]
    fn print_index_header_warned_true_does_not_panic() {
        // Smoke-test the warned branch — it formats a colored eprintln.
        print_index_header("fallback-id", true);
    }
}
