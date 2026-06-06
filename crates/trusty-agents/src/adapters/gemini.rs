//! Google Gemini Code harness adapter.
//!
//! Why: Detects Google's Gemini Code CLI in a tmux pane.
//! What: Implements `HarnessAdapter` for Gemini with brand patterns
//! "Gemini Code", "gemini", and "google ai".
//! Test: `"Gemini Code v1\n> "` detects with high confidence.

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
            Pattern::new("gemini-code", r"Gemini Code", 1.0),
            Pattern::new("gemini", r"(?i)\bgemini\b", 0.9),
            Pattern::new("google-ai", r"(?i)google.*ai", 0.8),
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
    id: "gemini",
    name: "Gemini Code",
    description: "Google Gemini Code CLI",
    command: "gemini",
    default_args: &[],
};

/// Adapter for Google's Gemini Code CLI.
pub struct GeminiAdapter;

impl HarnessAdapter for GeminiAdapter {
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
        &[r"Gemini Code", r"(?i)\bgemini\b", r"(?i)google.*ai"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_gemini_code() {
        let a = GeminiAdapter;
        let result = a.detect("Gemini Code v1\n> ");
        assert!(result.matched);
        assert!(result.confidence >= 0.9);
    }

    #[test]
    fn does_not_match_unrelated() {
        let a = GeminiAdapter;
        let result = a.detect("nothing\nhere");
        assert!(!result.matched);
    }
}
