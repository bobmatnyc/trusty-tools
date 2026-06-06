//! Self-aware adapter for the trusty-agents PM REPL.
//!
//! Why: Lets the harness recognize when a tmux pane is hosting the trusty-agents
//! PM REPL itself (e.g. nested or self-managed sessions).
//! What: Implements `HarnessAdapter` for trusty-agents with brand patterns
//! "trusty-agents", "tagent", "PM orchestrator", "ctrl>", and "Izzie>".
//! Test: `"trusty-agents v0.38\nctrl> "` detects with high confidence.

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
            Pattern::new("trusty-agents", r"trusty-agents", 1.0),
            Pattern::new("tagent", r"tagent", 1.0),
            Pattern::new("trusty-agents-legacy", r"trusty-agents", 0.9), // legacy banner compat
            Pattern::new("pm-orchestrator", r"PM orchestrator", 0.95),
            Pattern::new("ctrl-prompt", r"ctrl>", 0.8),
            Pattern::new("izzie-prompt", r"Izzie>", 0.8),
        ]
    })
}

fn idle_patterns() -> &'static [Pattern] {
    IDLE_PATTERNS.get_or_init(|| {
        vec![
            Pattern::new("ctrl-prompt", r"(?m)ctrl>\s*$", 0.9),
            Pattern::new("izzie-prompt", r"(?m)Izzie>\s*$", 0.9),
            Pattern::new("prompt", r"(?m)^>\s*$", 0.7),
        ]
    })
}

fn working_patterns() -> &'static [Pattern] {
    WORKING_PATTERNS.get_or_init(|| {
        vec![
            Pattern::new("thinking", r"(?i)thinking", 0.8),
            Pattern::new("delegating", r"(?i)delegating", 0.9),
        ]
    })
}

const INFO: AdapterInfo = AdapterInfo {
    id: "trusty-agents",
    name: "trusty-agents",
    description: "trusty-agents PM REPL (self)",
    command: "tagent",
    default_args: &[],
};

/// Adapter for the trusty-agents PM REPL itself.
pub struct TrustyAgentsAdapter;

impl HarnessAdapter for TrustyAgentsAdapter {
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
        &[r"(?m)ctrl>\s*$", r"(?m)Izzie>\s*$", r"(?m)^>\s*$"]
    }

    fn working_patterns(&self) -> &[&'static str] {
        &[r"(?i)thinking", r"(?i)delegating"]
    }

    fn brand_patterns(&self) -> &[&'static str] {
        &[
            r"trusty-agents",
            r"tagent",
            r"trusty-agents",
            r"PM orchestrator",
            r"ctrl>",
            r"Izzie>",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_trusty_agents_banner() {
        let a = TrustyAgentsAdapter;
        let result = a.detect("trusty-agents v0.38\nctrl> ");
        assert!(result.matched);
        assert!(result.confidence >= 0.9);
    }

    #[test]
    fn detects_legacy_banner_compat() {
        let a = TrustyAgentsAdapter;
        let result = a.detect("trusty-agents v0.1\nctrl> ");
        assert!(result.matched);
        assert!(result.confidence >= 0.8);
    }

    #[test]
    fn detects_ctrl_prompt_alone() {
        let a = TrustyAgentsAdapter;
        let result = a.detect("ctrl> ");
        assert!(result.matched);
    }

    #[test]
    fn does_not_match_unrelated() {
        let a = TrustyAgentsAdapter;
        let result = a.detect("masa@host $ ls");
        assert!(!result.matched);
    }
}
