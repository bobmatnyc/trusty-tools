//! Build the live "## Active TM Sessions" block injected into system prompts.
//!
//! Why: Without a snapshot the LLM either hallucinates ("I don't see any
//! sessions") or refuses to answer. Surfacing the tmux session list at prompt
//! build time gives ctrl/PM ground truth to reason from before deciding which
//! `tm_*` tool to call.
//! What: `build_tm_context_block` returns a markdown block ready to splice
//! into a system prompt, or an empty string when tmux is unavailable.
//! Test: Indirectly via `ctrl_chat_turn` integration tests when tmux exists;
//! the empty-string return when tmux is missing is exercised in CI.

use std::path::Path;

/// Build a "## Active TM Sessions" block for injection into system prompts.
///
/// Why: Without a live snapshot, the LLM either hallucinates ("I don't see any
/// sessions") or refuses to answer. Surfacing the tmux session list at prompt
/// build time gives ctrl/PM ground truth to reason from before deciding which
/// `tm_*` tool to call.
/// What: Tries to construct a `TmManager` rooted at `state_dir`, calls
/// `list_sessions`, and renders one line per session. Returns an empty string
/// if TM is unavailable (no tmux) so the calling prompt is unchanged.
/// Test: Indirectly via `ctrl_chat_turn` integration tests when tmux exists;
/// the empty-string return when tmux is missing is exercised in CI.
pub(crate) async fn build_tm_context_block(state_dir: &Path) -> String {
    let _ = std::fs::create_dir_all(state_dir);
    let mgr = match crate::tm::TmManager::new(state_dir) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    let sessions = match mgr.list_sessions().await {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if sessions.is_empty() {
        return "## Active TM Sessions\nNo tmux sessions currently managed.".to_string();
    }
    let mut block = String::from(
        "## Active TM Sessions\nYou can inspect or control these sessions using the tm_* tools.\n\n",
    );
    for s in &sessions {
        block.push_str(&format!(
            "- **{}** | adapter: {} | status: {} | project: {} | last active: {}\n",
            s.name,
            s.adapter_type.as_str(),
            s.status,
            s.project_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?"),
            s.last_active_ago(),
        ));
    }
    block
}
