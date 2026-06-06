//! Plain shell fallback adapter.
//!
//! Why: When no AI harness adapter recognizes a pane, the registry needs a
//! last-resort match. Shell is always returned with low-but-non-zero
//! confidence so callers always get *some* adapter back.
//! What: Implements `HarnessAdapter` for plain bash/zsh/sh shells. Brand
//! patterns match common prompt suffixes (`$ `, `% `, `# `) and `user@host`
//! style PS1 prefixes. `detect()` always returns a match: high-ish (0.6) if
//! a prompt-shape is found, else a flat 0.4 "shell-fallback".
//! Test: `"masa@macbook ~ $ "` matches; arbitrary text still returns the
//! 0.4 fallback match.

use std::sync::OnceLock;

use super::patterns::{Pattern, best_match, last_n_lines};
use super::traits::{
    AdapterInfo, DetectionResult, HarnessAdapter, HarnessObservation, HarnessState,
};

static BRAND_PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();

fn brand_patterns() -> &'static [Pattern] {
    BRAND_PATTERNS.get_or_init(|| {
        vec![
            Pattern::new("bash-prompt", r"(?m)[$]\s*$", 0.6),
            Pattern::new("zsh-prompt", r"(?m)[%]\s*$", 0.6),
            Pattern::new("root-prompt", r"(?m)[#]\s*$", 0.5),
            Pattern::new("user-host", r"\w+@[\w\-]+", 0.5),
        ]
    })
}

const INFO: AdapterInfo = AdapterInfo {
    id: "shell",
    name: "Shell",
    description: "Plain bash/zsh/sh shell (fallback)",
    command: "bash",
    default_args: &[],
};

/// Fallback adapter for plain shells.
pub struct ShellAdapter;

impl HarnessAdapter for ShellAdapter {
    fn info(&self) -> &AdapterInfo {
        &INFO
    }

    fn detect(&self, pane_output: &str) -> DetectionResult {
        let window = last_n_lines(pane_output, 20);
        if let Some(p) = best_match(&window, brand_patterns()) {
            DetectionResult::matched(p.confidence, p.name)
        } else {
            // Always return a weak fallback match so the registry has
            // something to fall back to.
            DetectionResult::matched(0.4, "shell-fallback")
        }
    }

    fn observe(&self, pane_output: &str) -> HarnessObservation {
        let window = last_n_lines(pane_output, 5);
        // Heuristic: trailing prompt → idle, otherwise unknown/working.
        let trimmed_end = window.trim_end();
        let last_char = trimmed_end.chars().last();
        let state = match last_char {
            Some('$') | Some('%') | Some('#') | Some('>') => HarnessState::Idle,
            None => HarnessState::Starting,
            _ => HarnessState::Working,
        };
        HarnessObservation {
            state,
            confidence: 0.6,
            errors: vec![],
        }
    }

    fn pause_command(&self) -> Option<&'static str> {
        None
    }

    fn resume_command(&self) -> Option<&'static str> {
        None
    }

    fn idle_patterns(&self) -> &[&'static str] {
        &[r"(?m)[$]\s*$", r"(?m)[%]\s*$", r"(?m)[#]\s*$"]
    }

    fn brand_patterns(&self) -> &[&'static str] {
        &[
            r"(?m)[$]\s*$",
            r"(?m)[%]\s*$",
            r"(?m)[#]\s*$",
            r"\w+@[\w\-]+",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bash_prompt() {
        let a = ShellAdapter;
        let result = a.detect("masa@macbook ~/Projects $ ");
        assert!(result.matched);
        // Either user-host or bash-prompt should match.
        assert!(result.confidence >= 0.5);
    }

    #[test]
    fn falls_back_for_arbitrary_text() {
        let a = ShellAdapter;
        let result = a.detect("just some random text");
        // Shell always returns matched (it's the fallback).
        assert!(result.matched);
        assert!((result.confidence - 0.4).abs() < f32::EPSILON);
    }

    #[test]
    fn observe_idle_on_dollar_prompt() {
        let a = ShellAdapter;
        let obs = a.observe("masa@host ~ $");
        assert_eq!(obs.state, HarnessState::Idle);
    }
}
