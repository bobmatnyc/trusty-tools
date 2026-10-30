//! The no-live-harness refusal every pane write runs (#10127).
//!
//! Why: a record can read `Active` after its harness exits; its pane is then
//! a shell, and a line typed there runs as a command. The inbound Slack
//! deliverer calls `inject` directly, skipping `send_input`'s readiness gate,
//! so the check sits in `inject_into_owned`, which every caller reaches.
//! What: [`SessionManager::require_live_harness`] probes the record's proven
//! pane for its runtime's binary and refuses unless the probe says `Present`.
//! Test: `inbound_send_into_an_exited_harness_pane_types_nothing`,
//! `inject_probes_the_owned_pane_for_the_runtime_binary`.

use super::manager::{ManagedError, SessionManager};
use super::record::SessionRecord;
use crate::core::process::PaneClaude;
use crate::runtime::RuntimeKind;

/// The process name a live `runtime` harness runs under.
fn harness_binary(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::ClaudeCode => "claude",
        // The `tcode` adapter's `TCODE_BINARY`.
        RuntimeKind::Tcode => "tcode",
    }
}

impl SessionManager {
    /// Refuse a pane write unless `record`'s harness runs in `pane` (#10127).
    ///
    /// Why: fail closed (#8942): a pane whose process tree cannot be read is
    /// refused as well as one that runs no harness.
    /// What: one [`super::ManagedTmuxDriver::pane_harness`] probe of the
    /// pane `owned_pane` proved. `Absent` → [`ManagedError::NoLiveHarness`]
    /// led by `no_live_harness:`; `Unknown` (including a stopped or background
    /// harness, #10136) → the same variant led by `harness_unconfirmed:`. Both
    /// say nothing was typed.
    /// Test: `inbound_send_into_an_exited_harness_pane_types_nothing`,
    /// `inject_probes_the_owned_pane_for_the_runtime_binary`,
    /// `a_harness_in_a_pane_holds_it_only_in_the_foreground`.
    pub(super) fn require_live_harness(
        &self,
        record: &SessionRecord,
        pane: &str,
    ) -> Result<(), ManagedError> {
        let harness = harness_binary(record.runtime);
        let name = &record.tmux_name;
        let why = match self.tmux.pane_harness(name, pane, harness) {
            PaneClaude::Present => return Ok(()),
            PaneClaude::Absent => format!(
                "no_live_harness: session {name} pane {pane} runs no {harness}; \
                 nothing was typed — relaunch the session first"
            ),
            // #10136: also a stopped or background harness, which does not
            // hold the pane's tty; typed text would reach the shell.
            PaneClaude::Unknown => format!(
                "harness_unconfirmed: session {name} pane {pane}: could not confirm \
                 {harness} runs there in the foreground; nothing was typed"
            ),
        };
        Err(ManagedError::NoLiveHarness(why))
    }
}

#[cfg(test)]
#[path = "harness_gate_tests.rs"]
mod tests;
