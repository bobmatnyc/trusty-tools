//! The daemon side of Bash-fed worktree recording (#8994 S2b).
//!
//! RED STUB.

use std::sync::Arc;

use serde_json::Value;

use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::core::worktree_ledger::hook_ingest::ObserveOutcome;
use crate::core::worktree_ledger::reconcile::ReconcileReport;
use crate::daemon::state::DaemonState;

/// What one detached Bash-command ingest did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandReport {
    /// One outcome per `git worktree add`, in command order.
    pub added: Vec<ObserveOutcome>,
    /// One report per `git worktree remove` / `prune`, in command order.
    pub reconciled: Vec<ReconcileReport>,
    /// Every failure, each already logged at `warn` and recorded in the sidecar.
    pub errors: Vec<String>,
}

/// RED STUB.
pub fn spawn_for_bash(
    _state: &Arc<DaemonState>,
    _session: SessionId,
    _event: HookEvent,
    _payload: &Value,
) -> Option<tokio::task::JoinHandle<CommandReport>> {
    None
}

#[cfg(test)]
#[path = "worktree_command_ingest_tests.rs"]
mod tests;
