//! claude-mpm harness adapter.
//!
//! Why: Detects when a tmux pane is running claude-mpm (the PM orchestrator)
//! so the harness can route pause/resume commands and observe orchestrator
//! state correctly.
//! What: Implements `HarnessAdapter` for claude-mpm. Brand patterns match
//! "PM ready", "claude-mpm", and orchestration vocabulary.
//! Test: Sample pane output `"PM ready\n> "` detects with high confidence;
//! pane with "thinking" reports Working state.

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
            Pattern::new("pm-ready", r"PM ready", 1.0),
            Pattern::new("claude-mpm", r"(?i)claude-mpm", 1.0),
            Pattern::new("mpm-word", r"(?i)\bMPM\b", 0.9),
            Pattern::new("orchestrate", r"(?i)orchestrat", 0.8),
            Pattern::new("delegate", r"(?i)delegat", 0.7),
        ]
    })
}

fn idle_patterns() -> &'static [Pattern] {
    IDLE_PATTERNS.get_or_init(|| {
        vec![
            Pattern::new("prompt", r"(?m)^>\s*$", 0.9),
            Pattern::new("pm-ready", r"(?i)PM ready", 0.95),
            Pattern::new("awaiting", r"(?i)awaiting instructions", 0.9),
        ]
    })
}

fn working_patterns() -> &'static [Pattern] {
    WORKING_PATTERNS.get_or_init(|| {
        vec![
            Pattern::new("thinking", r"(?i)thinking", 0.8),
            Pattern::new("delegating", r"(?i)delegating", 0.9),
            Pattern::new("running", r"(?i)running", 0.7),
        ]
    })
}

const INFO: AdapterInfo = AdapterInfo {
    id: "claude-mpm",
    name: "claude-mpm",
    description: "claude-mpm PM orchestrator",
    command: "claude-mpm",
    default_args: &[],
};

/// Adapter for the claude-mpm PM orchestrator.
pub struct ClaudeMpmAdapter;

impl HarnessAdapter for ClaudeMpmAdapter {
    fn info(&self) -> &AdapterInfo {
        &INFO
    }

    fn detect(&self, pane_output: &str) -> DetectionResult {
        // #330: After the first user/PM exchange the brand banner ("PM ready",
        // "claude-mpm") scrolls out of the visible window and `brand_patterns`
        // returns no match — the adapter would lose its identity even though
        // the pane is still showing the orchestrator's idle prompt. Combine
        // brand detection with idle detection so a pane that displays the
        // claude-mpm idle markers (`PM ready`, `awaiting instructions`, or a
        // bare `>` prompt) is still recognized.
        let window = last_n_lines(pane_output, 100);
        let brand = best_match(&window, brand_patterns());
        let idle = best_match(&window, idle_patterns());
        match (brand, idle) {
            (Some(b), Some(i)) => {
                // Both signals present — boost a touch above either alone.
                let conf = (b.confidence.max(i.confidence * 0.85) + 0.05).min(1.0);
                let name = if b.confidence >= i.confidence * 0.85 {
                    b.name
                } else {
                    i.name
                };
                DetectionResult::matched(conf, name)
            }
            (Some(b), None) => DetectionResult::matched(b.confidence, b.name),
            (None, Some(i)) => {
                // Idle-only: down-weight because some idle patterns (a bare
                // `>` prompt) are not exclusive to claude-mpm. The strong
                // `PM ready` / `awaiting instructions` signals still produce
                // ~0.76+ confidence; a lone shell `>` lands at ~0.72.
                DetectionResult::matched(i.confidence * 0.8, i.name)
            }
            (None, None) => DetectionResult::no_match(),
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
        Some("/mpm-session-pause")
    }

    fn resume_command(&self) -> Option<&'static str> {
        Some("/mpm-session-resume")
    }

    fn idle_patterns(&self) -> &[&'static str] {
        &[r"(?m)^>\s*$", r"(?i)PM ready", r"(?i)awaiting instructions"]
    }

    fn working_patterns(&self) -> &[&'static str] {
        &[r"(?i)thinking", r"(?i)delegating", r"(?i)running"]
    }

    fn brand_patterns(&self) -> &[&'static str] {
        &[
            r"PM ready",
            r"(?i)claude-mpm",
            r"(?i)\bMPM\b",
            r"(?i)orchestrat",
            r"(?i)delegat",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_pm_ready_with_high_confidence() {
        let a = ClaudeMpmAdapter;
        let result = a.detect("PM ready\n> ");
        assert!(result.matched);
        assert!(result.confidence >= 0.9);
    }

    #[test]
    fn does_not_match_unrelated_output() {
        let a = ClaudeMpmAdapter;
        let result = a.detect("masa@host ~ $ ls\nfoo bar baz");
        assert!(!result.matched);
    }

    #[test]
    fn observe_idle_on_pm_ready() {
        let a = ClaudeMpmAdapter;
        let obs = a.observe("PM ready\n> ");
        assert_eq!(obs.state, HarnessState::Idle);
    }

    #[test]
    fn observe_working_on_thinking() {
        let a = ClaudeMpmAdapter;
        let obs = a.observe("PM ready\nThinking about your request...");
        assert_eq!(obs.state, HarnessState::Working);
    }

    /// Why (#330): After the first PM/user exchange the brand banner scrolls
    /// out of the visible window. The adapter must keep recognizing the pane
    /// via its idle markers (`PM ready`, `awaiting instructions`, bare `>`)
    /// instead of dropping back to "no match".
    /// What: Pane output that lacks any brand pattern but contains the
    /// `awaiting instructions` idle marker should still detect with non-zero
    /// confidence.
    /// Test: Assert detection on `"\nawaiting instructions\n> "`.
    #[test]
    fn detects_via_idle_pattern_after_brand_scrolled_off() {
        let a = ClaudeMpmAdapter;
        // No brand tokens — just the idle marker plus a bare prompt.
        let result = a.detect("some output\nawaiting instructions\n> ");
        assert!(result.matched, "should match via idle pattern alone");
        assert!(
            result.confidence >= 0.5,
            "idle-only confidence too low: {}",
            result.confidence
        );
        // Sanity: a totally unrelated shell pane still does not match.
        let neg = a.detect("masa@host ~ $ ls -la\nfile1 file2");
        assert!(!neg.matched, "shell output must not false-positive");
    }

    #[test]
    fn pause_resume_commands_present() {
        let a = ClaudeMpmAdapter;
        assert_eq!(a.pause_command(), Some("/mpm-session-pause"));
        assert_eq!(a.resume_command(), Some("/mpm-session-resume"));
        assert!(a.can_pause());
        assert!(a.can_resume());
    }
}
