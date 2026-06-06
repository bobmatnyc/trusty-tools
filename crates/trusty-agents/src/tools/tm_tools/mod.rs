//! TM (Tmux Manager) tools — let the LLM query and control tmux sessions.
//!
//! Why: Without these tools, the ctrl/PM persona can only *describe* what
//! tmux/TM could do — it cannot actually inspect or manipulate live sessions.
//! Wiring TM through the tool registry means utterances like "what sessions
//! are running?", "pause the frontend session", or "what is api-work doing?"
//! flow through real `TmManager` calls rather than hallucination.
//! What: Nine `ToolExecutor` implementations covering session lifecycle
//! (`tm_new_session`, `tm_kill_session`), control (`tm_pause_session`,
//! `tm_resume_session`, `tm_send_message`), inspection (`tm_list_sessions`,
//! `tm_list_projects`, `tm_capture_pane`), and reconciliation
//! (`tm_reconcile`). Each tool holds a shared `Arc<Mutex<TmManager>>` so the
//! REPL's single TM instance is the source of truth across tools.
//! Test: Unit tests register the tools into a `ToolRegistry` and assert that
//! `register_tm_tools` adds all nine names. Behavioral tests are gated by a
//! live tmux server and live in `tests/`.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::tm::manager::TmManager;
use crate::tools::ToolRegistry;

mod control;
mod helpers;
mod lifecycle;
mod query;

pub use control::{TmPauseSessionTool, TmResumeSessionTool, TmSendMessageTool};
pub use lifecycle::{TmKillSessionTool, TmNewSessionTool};
pub use query::{TmCapturePaneTool, TmListProjectsTool, TmListSessionsTool, TmReconcileTool};

// ============================================================================
// Registration helpers
// ============================================================================

/// Register all TM tools into a tool registry, sharing one `TmManager` instance.
///
/// Why: The REPL owns the canonical `TmManager`; sharing it through the
/// registry means every `tm_*` call mutates the same in-memory state and
/// idle-monitor view.
/// What: Inserts all nine `Tm*Tool` structs, each with a clone of `tm`.
/// Test: `register_tm_tools_adds_all_nine` below.
pub fn register_tm_tools(registry: &mut ToolRegistry, tm: Arc<Mutex<TmManager>>) {
    registry.register(Arc::new(TmListSessionsTool {
        tm: Arc::clone(&tm),
    }));
    registry.register(Arc::new(TmListProjectsTool {
        tm: Arc::clone(&tm),
    }));
    registry.register(Arc::new(TmNewSessionTool {
        tm: Arc::clone(&tm),
    }));
    registry.register(Arc::new(TmPauseSessionTool {
        tm: Arc::clone(&tm),
    }));
    registry.register(Arc::new(TmResumeSessionTool {
        tm: Arc::clone(&tm),
    }));
    registry.register(Arc::new(TmSendMessageTool {
        tm: Arc::clone(&tm),
    }));
    registry.register(Arc::new(TmCapturePaneTool {
        tm: Arc::clone(&tm),
    }));
    registry.register(Arc::new(TmReconcileTool {
        tm: Arc::clone(&tm),
    }));
    registry.register(Arc::new(TmKillSessionTool { tm }));
}

/// Try to register TM tools by constructing a fresh `TmManager` rooted at
/// `state_dir`. Used by call sites that don't already hold a shared instance
/// (e.g., the free `run_pm_task_with_history`).
///
/// Why: Without a shared `Arc<Mutex<TmManager>>` threaded through every
/// dispatch path, the simplest way to give the LLM TM access is to construct
/// a manager on-demand. JSON registry I/O is cheap enough that doing so per
/// turn is acceptable; multiple managers across a single tmux server agree
/// because they all read/write the same `tm_sessions.json`.
/// What: Tries `TmManager::new(state_dir)`. On success, registers all nine
/// tools. On failure (typically: tmux binary missing), logs at debug and
/// returns without modifying the registry.
/// Test: `register_tm_tools_for_state_dir_skips_when_tmux_missing` —
/// constructing a TmManager without tmux fails fast and the registry is
/// unchanged.
#[allow(dead_code)]
pub fn register_tm_tools_for_state_dir(registry: &mut ToolRegistry, state_dir: &std::path::Path) {
    // Best-effort: ensure the directory exists so registry I/O won't fail
    // simply because no one has opened TM yet.
    let _ = std::fs::create_dir_all(state_dir);
    match TmManager::new(state_dir) {
        Ok(mgr) => {
            let arc = Arc::new(Mutex::new(mgr));
            register_tm_tools(registry, arc);
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                state_dir = %state_dir.display(),
                "register_tm_tools_for_state_dir: TmManager construction failed (tmux likely unavailable); skipping TM tool registration"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::TmuxOrchestrator;
    use crate::tools::traits::ToolExecutor;
    use serde_json::json;

    fn requires_tmux_or_skip() -> bool {
        TmuxOrchestrator::is_available()
    }

    #[test]
    fn register_tm_tools_adds_all_nine() {
        if !requires_tmux_or_skip() {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = TmManager::new(dir.path()).unwrap();
        let arc = Arc::new(Mutex::new(mgr));
        let mut registry = ToolRegistry::new();
        register_tm_tools(&mut registry, arc);

        for name in [
            "tm_list_sessions",
            "tm_list_projects",
            "tm_new_session",
            "tm_pause_session",
            "tm_resume_session",
            "tm_send_message",
            "tm_capture_pane",
            "tm_reconcile",
            "tm_kill_session",
        ] {
            assert!(registry.contains(name), "missing TM tool: {name}");
        }
    }

    #[tokio::test]
    async fn missing_session_name_is_recoverable_error() {
        if !requires_tmux_or_skip() {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = TmManager::new(dir.path()).unwrap();
        let arc = Arc::new(Mutex::new(mgr));

        let tool = TmPauseSessionTool { tm: arc };
        let r = tool.execute(json!({})).await;
        assert!(r.is_error());
        assert!(r.content().contains("session_name"));
    }

    #[tokio::test]
    async fn capture_pane_default_lines_when_omitted() {
        if !requires_tmux_or_skip() {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = TmManager::new(dir.path()).unwrap();
        let arc = Arc::new(Mutex::new(mgr));

        let tool = TmCapturePaneTool { tm: arc };
        // Session doesn't exist → adapter call errors. We just want to
        // confirm the args parser doesn't panic on missing `lines`.
        let r = tool
            .execute(json!({"session_name": "does-not-exist"}))
            .await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn list_sessions_no_params_required() {
        if !requires_tmux_or_skip() {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = TmManager::new(dir.path()).unwrap();
        let arc = Arc::new(Mutex::new(mgr));

        let tool = TmListSessionsTool { tm: arc };
        let r = tool.execute(json!({})).await;
        // Empty registry → empty array, success.
        assert!(!r.is_error(), "unexpected error: {}", r.content());
    }

    #[test]
    fn schemas_declare_required_params() {
        // No tmux required — schema is static.
        let dir = tempfile::TempDir::new().unwrap();
        if !requires_tmux_or_skip() {
            // Still skip if we can't construct TmManager.
            return;
        }
        let mgr = TmManager::new(dir.path()).unwrap();
        let arc = Arc::new(Mutex::new(mgr));

        let pause = TmPauseSessionTool {
            tm: Arc::clone(&arc),
        };
        let schema = pause.schema();
        assert_eq!(schema["function"]["name"], "tm_pause_session");
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "session_name");

        let send = TmSendMessageTool {
            tm: Arc::clone(&arc),
        };
        let schema = send.schema();
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|v| v == "session_name"));
        assert!(required.iter().any(|v| v == "message"));
    }
}
