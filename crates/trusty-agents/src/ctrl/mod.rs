//! CTRL actor — interactive multi-project PM coordination CLI.
//!
//! Why: A single binary entry point that manages multiple PM sessions across
//! different project directories, routing user input to the active session or
//! dispatching slash commands for lifecycle management.
//! What: `run_ctrl` presents a readline-style prompt, manages named `PmHandle`
//! actors (one per project), and dispatches tasks or commands accordingly.
//! Test: Run `cargo run` with no `--pm` flag; type `/help`; verify command
//! listing; `/connect <project_path>` to start a PM session.
//!
//! The CTRL implementation is split into focused submodules:
//! - `state` — Ctrl/PmHandle/PmMsg/ConversationTurn data types and lifecycle
//! - `config` — agent-config resolution, credential routing, prompt fragments
//! - `util` — small helpers (slot draining, self-project detection, bus audit log)
//! - `claude_cli` — single-shot `claude` CLI dispatch (OAuth-only path)
//! - `socket` / `socket_listener` — controller singleton socket + accept loop
//! - `supervisor` — long-running supervisor entry point
//! - `pm_task` — `run_pm_task_*` dispatch entry points
//! - `ctrl_turn` — single CTRL-level LLM turn (no PM attached)
//! - `repl` — interactive stdin loop + slash-command dispatcher
//! - `handlers` — per-tool ToolExecutor implementations

pub mod claude_cli;
pub mod config;
pub mod ctrl_turn;
pub mod handlers;
pub mod pm_task;
pub mod repl;
pub mod socket;
pub mod socket_listener;
pub mod state;
pub mod supervisor;
pub mod util;

#[cfg(test)]
mod tests;

pub use socket::{
    BindOutcome, CtrlSocket, ctrl_socket_path, cwd_project_id, is_connection_refused,
};
pub use supervisor::{CtrlSupervisor, SupervisorOutcome};

// Public surface re-exports — preserve the v0.38.x API for downstream callers
// (main.rs, REPL, tests). All implementations live in the submodules above.
pub use config::SessionOverrides;
pub use pm_task::{run_pm_task_with_history, run_pm_task_with_persona, run_pm_task_with_session};
pub use repl::{run_ctrl, run_ctrl_headless};
pub use socket_listener::{forward_to_controller, spawn_socket_listener};
pub use state::ConversationTurn;
pub use util::{PmMessageRecord, append_pm_message, detect_self_project};
