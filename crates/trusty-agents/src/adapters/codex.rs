//! OpenAI Codex CLI harness adapter.
//!
//! Why: Detects OpenAI's `codex-cli` running in a tmux pane.
//! What: Implements `HarnessAdapter` for Codex with brand patterns matching
//! "codex", "openai", and "codex-cli".
//! Test: `"codex-cli v0.5\n> "` detects with high confidence.

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
            Pattern::new("codex", r"(?i)\bcodex\b", 1.0),
            Pattern::new("codex-cli", r"(?i)codex-cli", 1.0),
            Pattern::new("openai", r"openai", 0.85),
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
            Pattern::new("thinking", r"(?i)thinking", 0.8),
            Pattern::new("generating", r"(?i)generating", 0.8),
        ]
    })
}

const INFO: AdapterInfo = AdapterInfo {
    id: "codex",
    name: "Codex",
    description: "OpenAI Codex CLI",
    command: "codex",
    default_args: &[],
};

/// Adapter for OpenAI's Codex CLI.
pub struct CodexAdapter;

impl HarnessAdapter for CodexAdapter {
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
        &[r"(?i)thinking", r"(?i)generating"]
    }

    fn brand_patterns(&self) -> &[&'static str] {
        &[r"(?i)\bcodex\b", r"(?i)codex-cli", r"openai"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_codex_cli() {
        let a = CodexAdapter;
        let result = a.detect("codex-cli v0.5\n> ");
        assert!(result.matched);
        assert!(result.confidence >= 0.9);
    }

    #[test]
    fn does_not_match_unrelated_output() {
        let a = CodexAdapter;
        let result = a.detect("just a shell\nnothing here");
        assert!(!result.matched);
    }

    #[test]
    fn observe_idle() {
        let a = CodexAdapter;
        let obs = a.observe("> ");
        assert_eq!(obs.state, HarnessState::Idle);
    }
}
