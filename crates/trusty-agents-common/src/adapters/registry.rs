//! Adapter registry — maps adapter IDs to instances and runs auto-detection.
//!
//! Why: A central place to register adapters and dispatch detection so the
//! rest of the harness doesn't hard-code adapter selection. Phase 2
//! (issue #312) registers all concrete adapters by default.
//! What: `AdapterRegistry::new()` returns a fully-populated registry with all
//! built-in adapters; `empty()` returns an empty one for tests.
//! `register()` adds adapters; `detect()` runs all registered adapters
//! against a pane snapshot and returns the highest-confidence match,
//! falling back to shell when no non-shell adapter exceeds confidence 0.7.
//! Test: Default registry detects the appropriate adapter for sample pane
//! outputs; falls back to shell when nothing else matches.

use std::collections::HashMap;
use std::sync::Arc;

use super::augment::AugmentAdapter;
use super::claude_code::ClaudeCodeAdapter;
use super::claude_mpm::ClaudeMpmAdapter;
use super::codex::CodexAdapter;
use super::gemini::GeminiAdapter;
use super::shell::ShellAdapter;
use super::traits::HarnessAdapter;
use super::trusty_agents_adapter::TrustyAgentsAdapter;

/// Registry of harness adapters keyed by their stable `info().id`.
pub struct AdapterRegistry {
    adapters: HashMap<&'static str, Arc<dyn HarnessAdapter>>,
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterRegistry {
    /// Create a registry pre-populated with every built-in adapter.
    ///
    /// Why: Most callers want all adapters available — registering them by
    /// hand at every call site is noise. Tests that need a clean slate can
    /// use [`AdapterRegistry::empty`].
    pub fn new() -> Self {
        let mut registry = Self::empty();
        registry.register(Arc::new(ClaudeMpmAdapter));
        registry.register(Arc::new(ClaudeCodeAdapter));
        registry.register(Arc::new(CodexAdapter));
        registry.register(Arc::new(AugmentAdapter));
        registry.register(Arc::new(GeminiAdapter));
        registry.register(Arc::new(TrustyAgentsAdapter));
        registry.register(Arc::new(ShellAdapter)); // last-resort
        registry
    }

    /// Create an empty registry. Primarily useful for tests.
    pub fn empty() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    /// Register an adapter. Adapter ID comes from `adapter.info().id`.
    pub fn register(&mut self, adapter: Arc<dyn HarnessAdapter>) {
        self.adapters.insert(adapter.info().id, adapter);
    }

    /// Look up an adapter by ID.
    pub fn get(&self, id: &str) -> Option<Arc<dyn HarnessAdapter>> {
        self.adapters.get(id).cloned()
    }

    /// List all registered adapter IDs.
    pub fn list(&self) -> Vec<&'static str> {
        self.adapters.keys().copied().collect()
    }

    /// Auto-detect the best-matching adapter from pane output.
    ///
    /// Returns `(adapter_id, confidence)`. Non-shell adapters need
    /// confidence >= 0.7 to win; otherwise the shell adapter (registered
    /// under id "shell") is consulted as a last resort. If no shell adapter
    /// is registered either, falls back to `("shell", 0.5)`.
    pub fn detect(&self, pane_output: &str) -> (&'static str, f32) {
        let mut best: Option<(&'static str, f32)> = None;
        for (id, adapter) in &self.adapters {
            if *id == "shell" {
                continue;
            }
            let result = adapter.detect(pane_output);
            if result.matched
                && result.confidence >= 0.7
                && best.is_none_or(|(_, c)| result.confidence > c)
            {
                best = Some((id, result.confidence));
            }
        }
        if let Some(hit) = best {
            return hit;
        }
        if let Some(shell) = self.adapters.get("shell") {
            let r = shell.detect(pane_output);
            return ("shell", r.confidence);
        }
        ("shell", 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::super::traits::{
        AdapterInfo, DetectionResult, HarnessAdapter, HarnessObservation, HarnessState,
    };
    use super::*;

    struct FakeAdapter {
        info: AdapterInfo,
        match_text: &'static str,
        confidence: f32,
    }

    impl HarnessAdapter for FakeAdapter {
        fn info(&self) -> &AdapterInfo {
            &self.info
        }
        fn detect(&self, pane_output: &str) -> DetectionResult {
            if pane_output.contains(self.match_text) {
                DetectionResult::matched(self.confidence, "fake")
            } else {
                DetectionResult::no_match()
            }
        }
        fn observe(&self, _pane_output: &str) -> HarnessObservation {
            HarnessObservation {
                state: HarnessState::Idle,
                confidence: 1.0,
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
            &[]
        }
        fn brand_patterns(&self) -> &[&'static str] {
            &[]
        }
    }

    fn make_fake(id: &'static str, match_text: &'static str, confidence: f32) -> Arc<FakeAdapter> {
        Arc::new(FakeAdapter {
            info: AdapterInfo {
                id,
                name: id,
                description: "fake",
                command: "fake",
                default_args: &[],
            },
            match_text,
            confidence,
        })
    }

    #[test]
    fn empty_registry_falls_back_to_shell() {
        let reg = AdapterRegistry::empty();
        let (id, conf) = reg.detect("anything");
        assert_eq!(id, "shell");
        assert!((conf - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn register_and_get() {
        let mut reg = AdapterRegistry::empty();
        reg.register(make_fake("a", "foo", 0.8));
        assert!(reg.get("a").is_some());
        assert!(reg.get("missing").is_none());
        assert_eq!(reg.list(), vec!["a"]);
    }

    #[test]
    fn detect_picks_highest_confidence_match() {
        let mut reg = AdapterRegistry::empty();
        reg.register(make_fake("low", "foo", 0.75));
        reg.register(make_fake("high", "foo", 0.95));
        reg.register(make_fake("nomatch", "bar", 1.0));

        let (id, conf) = reg.detect("text with foo in it");
        assert_eq!(id, "high");
        assert!((conf - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn detect_no_match_falls_back_to_shell() {
        let mut reg = AdapterRegistry::empty();
        reg.register(make_fake("a", "foo", 0.9));
        let (id, _) = reg.detect("nothing here");
        assert_eq!(id, "shell");
    }

    #[test]
    fn default_registry_detects_claude_mpm() {
        let reg = AdapterRegistry::new();
        let (id, _) = reg.detect("PM ready\n> ");
        assert_eq!(id, "claude-mpm");
    }

    #[test]
    fn default_registry_detects_claude_code() {
        let reg = AdapterRegistry::new();
        let (id, _) = reg.detect("Claude Code v1.0\nReady\n> ");
        assert_eq!(id, "claude-code");
    }

    #[test]
    fn default_registry_detects_codex() {
        let reg = AdapterRegistry::new();
        let (id, _) = reg.detect("codex-cli v0.5\n> ");
        assert_eq!(id, "codex");
    }

    #[test]
    fn default_registry_falls_back_to_shell_for_plain_prompt() {
        let reg = AdapterRegistry::new();
        let (id, _) = reg.detect("masa@macbook ~/Projects $ ");
        assert_eq!(id, "shell");
    }

    #[test]
    fn default_registry_lists_all_adapters() {
        let reg = AdapterRegistry::new();
        let ids = reg.list();
        for expected in [
            "claude-mpm",
            "claude-code",
            "codex",
            "augment",
            "gemini",
            "trusty-agents",
            "shell",
        ] {
            assert!(ids.contains(&expected), "missing adapter id: {}", expected);
        }
    }
}
