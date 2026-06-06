//! Claude Code CLI harness adapter.
//!
//! Why: Detects Anthropic's Claude Code CLI in a tmux pane so the harness can
//! observe its state. Claude Code has no native pause/resume.
//! What: Implements `HarnessAdapter` for Claude Code. Brand patterns include
//! the literal "Claude Code" banner, the working-spinner glyph "✻", and the
//! `claude.ai` domain.
//! Test: `"Claude Code v1.0\n> "` detects with high confidence; the working
//! glyph in output reports Working state.

use std::sync::OnceLock;

use super::patterns::{Pattern, any_match, best_match, last_n_lines};
use super::traits::{
    AdapterInfo, DetectionResult, HarnessAdapter, HarnessObservation, HarnessState,
};

static BRAND_PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
static IDLE_PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
static WORKING_PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();

fn brand_patterns() -> &'static [Pattern] {
    BRAND_PATTERNS.get_or_init(|| {
        vec![
            Pattern::new("claude-code", r"Claude Code", 1.0),
            Pattern::new("claude-ai", r"claude\.ai", 0.95),
            Pattern::new("skip-permissions", r"dangerously-skip-permissions", 0.9),
            Pattern::new("working-glyph", r"✻", 0.85),
            Pattern::new("prompt", r"(?m)^>\s*$", 0.6),
        ]
    })
}

fn idle_patterns() -> &'static [Pattern] {
    IDLE_PATTERNS.get_or_init(|| {
        vec![
            Pattern::new("prompt", r"(?m)^>\s*$", 0.9),
            Pattern::new("waiting", r"(?i)waiting for input", 0.9),
        ]
    })
}

fn working_patterns() -> &'static [Pattern] {
    WORKING_PATTERNS.get_or_init(|| {
        vec![
            Pattern::new("glyph", r"✻", 0.9),
            Pattern::new("thinking", r"(?i)thinking", 0.8),
            Pattern::new("generating", r"(?i)generating", 0.8),
        ]
    })
}

const INFO: AdapterInfo = AdapterInfo {
    id: "claude-code",
    name: "Claude Code",
    description: "Anthropic Claude Code CLI",
    command: "claude",
    default_args: &[],
};

/// Adapter for Anthropic's Claude Code CLI.
pub struct ClaudeCodeAdapter;

impl HarnessAdapter for ClaudeCodeAdapter {
    fn info(&self) -> &AdapterInfo {
        &INFO
    }

    fn detect(&self, pane_output: &str) -> DetectionResult {
        let window = last_n_lines(pane_output, 100);
        match best_match(&window, brand_patterns()) {
            Some(p) => DetectionResult::matched(p.confidence, p.name),
            None => DetectionResult::no_match(),
        }
    }

    fn observe(&self, pane_output: &str) -> HarnessObservation {
        let window = last_n_lines(pane_output, 30);
        if any_match(&window, working_patterns()) {
            return HarnessObservation {
                state: HarnessState::Working,
                confidence: 0.8,
                errors: vec![],
            };
        }
        if any_match(&window, idle_patterns()) {
            return HarnessObservation {
                state: HarnessState::Idle,
                confidence: 0.9,
                errors: vec![],
            };
        }
        HarnessObservation {
            state: if window.trim().is_empty() {
                HarnessState::Starting
            } else {
                HarnessState::Working
            },
            confidence: 0.5,
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
        &[r"(?m)^>\s*$", r"(?i)waiting for input"]
    }

    fn working_patterns(&self) -> &[&'static str] {
        &["✻", r"(?i)thinking", r"(?i)generating"]
    }

    fn brand_patterns(&self) -> &[&'static str] {
        &[
            r"Claude Code",
            r"claude\.ai",
            r"dangerously-skip-permissions",
            "✻",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_claude_code_banner() {
        let a = ClaudeCodeAdapter;
        let result = a.detect("Claude Code v1.0\n> ");
        assert!(result.matched);
        assert!(result.confidence >= 0.9);
    }

    #[test]
    fn does_not_match_unrelated_output() {
        let a = ClaudeCodeAdapter;
        let result = a.detect("foo\nbar\nbaz");
        assert!(!result.matched);
    }

    #[test]
    fn observe_working_on_glyph() {
        let a = ClaudeCodeAdapter;
        let obs = a.observe("✻ Crunching...");
        assert_eq!(obs.state, HarnessState::Working);
    }

    #[test]
    fn no_pause_resume() {
        let a = ClaudeCodeAdapter;
        assert_eq!(a.pause_command(), None);
        assert_eq!(a.resume_command(), None);
    }
}
