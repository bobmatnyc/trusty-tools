//! Core trait and value types for harness adapters.
//!
//! Why: The harness needs a uniform way to talk to multiple AI coding tools
//! (Claude Code, MPM, Codex, plain shells, …). The `HarnessAdapter` trait
//! defines the interface every adapter implements; concrete adapters arrive
//! in Phase 2.
//! What: `HarnessState` enum, `DetectionResult` / `HarnessObservation` value
//! types, `AdapterInfo` metadata struct, and the `HarnessAdapter` trait.
//! Test: Phase-2 adapter implementations test the trait surface; for Phase 1
//! we only verify the types compile and round-trip through serde.

use serde::{Deserialize, Serialize};

/// High-level state of a harness instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessState {
    /// Harness is starting up.
    Starting,
    /// Harness is ready, waiting for input.
    Idle,
    /// Harness is actively working.
    Working,
    /// Harness is paused (user-initiated).
    Paused,
    /// Harness encountered an error.
    Error,
    /// Harness has stopped.
    Stopped,
}

/// Result of running a single adapter detection pass on a pane snapshot.
#[derive(Debug, Clone)]
pub struct DetectionResult {
    /// Whether the adapter recognizes this output as belonging to its harness.
    pub matched: bool,
    /// Confidence in the match (0.0 - 1.0).
    pub confidence: f32,
    /// Name of the pattern that matched, if any.
    pub pattern: Option<&'static str>,
}

impl DetectionResult {
    /// Construct a non-match.
    pub fn no_match() -> Self {
        Self {
            matched: false,
            confidence: 0.0,
            pattern: None,
        }
    }

    /// Construct a match with the given confidence and pattern name.
    pub fn matched(confidence: f32, pattern: &'static str) -> Self {
        Self {
            matched: true,
            confidence,
            pattern: Some(pattern),
        }
    }
}

/// Outcome of observing a harness's pane: state + confidence + collected errors.
#[derive(Debug, Clone)]
pub struct HarnessObservation {
    /// Detected state.
    pub state: HarnessState,
    /// Confidence in the state detection (0.0 - 1.0).
    pub confidence: f32,
    /// Any error strings extracted from the pane.
    pub errors: Vec<String>,
}

/// Static metadata about an adapter.
#[derive(Debug, Clone)]
pub struct AdapterInfo {
    /// Stable identifier (e.g. "claude-code", "mpm", "shell").
    pub id: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Description of the harness this adapter targets.
    pub description: &'static str,
    /// Command used to launch the harness.
    pub command: &'static str,
    /// Default arguments passed to the command.
    pub default_args: &'static [&'static str],
}

/// Adapter contract for AI harnesses that run inside tmux panes.
///
/// Implementations recognize their own harness from raw pane output, expose
/// the commands needed to pause/resume, and provide the patterns the
/// detection registry uses to identify them.
pub trait HarnessAdapter: Send + Sync {
    /// Adapter metadata.
    fn info(&self) -> &AdapterInfo;

    /// Decide whether `pane_output` is from this adapter's harness.
    fn detect(&self, pane_output: &str) -> DetectionResult;

    /// Inspect `pane_output` and report the harness's current state.
    fn observe(&self, pane_output: &str) -> HarnessObservation;

    /// Command to pause the harness, if supported.
    fn pause_command(&self) -> Option<&'static str>;

    /// Command to resume the harness, if supported.
    fn resume_command(&self) -> Option<&'static str>;

    /// Whether this adapter supports pausing.
    fn can_pause(&self) -> bool {
        self.pause_command().is_some()
    }

    /// Whether this adapter supports resuming.
    fn can_resume(&self) -> bool {
        self.resume_command().is_some()
    }

    /// Format a user message for delivery to the harness.
    fn format_message(&self, message: &str) -> String {
        message.to_string()
    }

    /// Patterns indicating the harness is idle.
    fn idle_patterns(&self) -> &[&'static str];

    /// Patterns indicating an error.
    fn error_patterns(&self) -> &[&'static str] {
        &[]
    }

    /// Patterns indicating active work.
    fn working_patterns(&self) -> &[&'static str] {
        &[]
    }

    /// Brand-identification patterns used during initial detection.
    fn brand_patterns(&self) -> &[&'static str];
}
