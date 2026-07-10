//! Turnkey task injection: deliver a session's `--task` into its pane once the
//! runtime is ready (issues #1903 / #1299).
//!
//! Why: `tm session new --task "<text>"` (and `tm ticket`, which posts to the
//! same spawn endpoint) stored the task as metadata but never typed it into the
//! freshly-launched Claude Code pane — the session sat idle at an empty prompt
//! and any caller expecting turnkey execution waited forever. The fix reuses the
//! EXISTING `send_input`/`send_line` seam (`tm session send` /
//! `POST .../{id}/send`) to deliver the task, but only after the runtime is
//! ready to accept typed input, replacing the blind fixed sleep the meta-launch
//! prototype used ([`crate::bin` `meta::launch`]) with a bounded readiness poll.
//! What: [`should_inject_task`] is the pure gate (opt-out flag, non-empty task,
//! Claude Code runtime, live session) and [`SessionManager::inject_task_when_ready`]
//! is the readiness-gated delivery. Only `RuntimeKind::ClaudeCode` is injected —
//! the `tcode` adapter already delivers the task in its `run-task` launch command,
//! so injecting there would duplicate it.
//! Test: `should_inject_*` (pure gate) and `inject_when_ready_*` (delivery via
//! the `FakeTmuxDriver` seam) in this module's `tests` submodule.

use std::time::Duration;

use tracing::{info, warn};

use crate::runtime::RuntimeKind;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState};

/// Max readiness-probe attempts before giving up (issue #1903).
///
/// Why: injection must be BOUNDED — a runtime that never comes up (spawn failed
/// silently, `claude` crashed on launch) must not leave a background task
/// polling forever. With the backoff schedule below this caps the wait at
/// roughly one minute, comfortably longer than Claude Code's 1–3 s launch while
/// still terminating.
const READY_MAX_ATTEMPTS: u32 = 30;

/// Initial delay between readiness probes; doubles up to [`READY_BACKOFF_MAX`].
const READY_BACKOFF_START: Duration = Duration::from_millis(250);

/// Ceiling for the exponential readiness backoff.
const READY_BACKOFF_MAX: Duration = Duration::from_secs(2);

/// Pure gate: should the spawned session's task be auto-injected into its pane?
///
/// Why: the daemon spawn path must decide — before spending a background task on
/// readiness polling — whether injection applies at all. Keeping the decision a
/// pure function of primitives makes every branch unit-testable without a daemon,
/// a tmux, or a session record.
/// What: returns `true` only when ALL hold: the caller did not opt out
/// (`inject_flag`), the task is non-empty after trimming, the runtime is
/// `ClaudeCode` (the `tcode` adapter already injects the task in its launch
/// command, so re-injecting would duplicate it), and the session reached
/// `Active` (a spawn that errored or was withheld by the intent-conformance
/// front gate stays `Provisioning`/`Errored` — never inject into those).
/// Test: `should_inject_task_true_for_active_claude_code`,
/// `should_inject_task_false_when_opted_out`,
/// `should_inject_task_false_for_empty_task`,
/// `should_inject_task_false_for_tcode`,
/// `should_inject_task_false_when_not_active`.
pub fn should_inject_task(
    inject_flag: bool,
    task: &str,
    runtime: RuntimeKind,
    state: &ManagedSessionState,
) -> bool {
    inject_flag
        && !task.trim().is_empty()
        && runtime == RuntimeKind::ClaudeCode
        && *state == ManagedSessionState::Active
}

impl SessionManager {
    /// Deliver `task` into the session's pane once its runtime is ready (#1903).
    ///
    /// Why: this is the turnkey half of `--task` — after the pane is up and the
    /// runtime has launched, the task is typed in through the SAME seam
    /// `tm session send` uses ([`Self::send_input`] → `send_line` + Enter), so a
    /// caller that treats `--task` as "start working immediately" gets exactly
    /// that. Delivery MUST wait for readiness: keystrokes sent before `claude`
    /// has exec'd are lost, so this polls [`super::driver::ManagedTmuxDriver::runtime_ready`]
    /// with bounded exponential backoff rather than a blind sleep.
    /// What: returns `Ok(false)` early for an empty task; otherwise polls
    /// readiness up to [`READY_MAX_ATTEMPTS`] times (backoff
    /// [`READY_BACKOFF_START`]→[`READY_BACKOFF_MAX`]), bailing (with a warning,
    /// `Ok(false)`) if the session transitions to `Stopped`/`Errored`/
    /// `Decommissioned` while waiting or never becomes ready within the budget.
    /// On readiness it calls [`Self::send_input`] and returns `Ok(true)`. The
    /// task is NOT lost when injection is skipped — it remains on the record as
    /// metadata, exactly as before this fix, so a caller can still deliver it
    /// manually via `tm session send`.
    /// Test: `inject_when_ready_sends_task_via_send_seam`,
    /// `inject_when_ready_skips_empty_task` in this module's `tests`.
    pub async fn inject_task_when_ready(
        &self,
        id: &ManagedSessionId,
        task: &str,
    ) -> Result<bool, ManagedError> {
        if task.trim().is_empty() {
            return Ok(false);
        }
        let name = self.get(id).await?.tmux_name;

        let mut delay = READY_BACKOFF_START;
        let mut ready = false;
        for attempt in 0..READY_MAX_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(READY_BACKOFF_MAX);
            }
            // Bail if the session died / was torn down while we waited — typing
            // into a dead pane is pointless and `send_input` would reject it.
            let state = self.get(id).await?.state;
            if matches!(
                state,
                ManagedSessionState::Stopped
                    | ManagedSessionState::Errored
                    | ManagedSessionState::Decommissioned
            ) {
                warn!(
                    id = %id,
                    name = %name,
                    state = %state,
                    "task injection aborted: session no longer live (task retained as metadata)"
                );
                return Ok(false);
            }
            if self.tmux.runtime_ready(&name) {
                ready = true;
                break;
            }
        }

        if !ready {
            warn!(
                id = %id,
                name = %name,
                "runtime not ready within budget; task NOT injected (retained as metadata — \
                 deliver it with `tm session send`)"
            );
            return Ok(false);
        }

        self.send_input(id, task).await?;
        info!(id = %id, name = %name, "injected --task into ready session pane (#1903)");
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::record::ManagedSessionState;
    use super::super::tests::make_manager;
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn should_inject_task_true_for_active_claude_code() {
        assert!(should_inject_task(
            true,
            "implement OAuth2",
            RuntimeKind::ClaudeCode,
            &ManagedSessionState::Active,
        ));
    }

    #[test]
    fn should_inject_task_false_when_opted_out() {
        // `--no-inject` / metadata-only: never inject even for a live claude session.
        assert!(!should_inject_task(
            false,
            "implement OAuth2",
            RuntimeKind::ClaudeCode,
            &ManagedSessionState::Active,
        ));
    }

    #[test]
    fn should_inject_task_false_for_empty_task() {
        assert!(!should_inject_task(
            true,
            "   ",
            RuntimeKind::ClaudeCode,
            &ManagedSessionState::Active,
        ));
    }

    #[test]
    fn should_inject_task_false_for_tcode() {
        // tcode injects the task in its own launch command; re-injecting would duplicate it.
        assert!(!should_inject_task(
            true,
            "implement OAuth2",
            RuntimeKind::Tcode,
            &ManagedSessionState::Active,
        ));
    }

    #[test]
    fn should_inject_task_false_when_not_active() {
        // A spawn that errored or was withheld by the front gate must not be injected.
        for state in [
            ManagedSessionState::Provisioning,
            ManagedSessionState::Errored,
            ManagedSessionState::Stopped,
            ManagedSessionState::Decommissioned,
        ] {
            assert!(!should_inject_task(
                true,
                "implement OAuth2",
                RuntimeKind::ClaudeCode,
                &state,
            ));
        }
    }

    #[tokio::test]
    async fn inject_when_ready_sends_task_via_send_seam() {
        // Integration-style: with the FakeTmuxDriver (whose `runtime_ready`
        // defaults to `session_exists` → true after create), injection delivers
        // the task through the SAME `send_line` seam `tm session send` uses.
        let dir = TempDir::new().unwrap();
        let (mgr, fake) = make_manager(&dir).await;
        let record = mgr
            .create("wire up the widget".into(), None, None, None, None, None)
            .await
            .expect("create");

        let injected = mgr
            .inject_task_when_ready(&record.id, "wire up the widget")
            .await
            .expect("inject");

        assert!(
            injected,
            "task should have been injected into the ready pane"
        );
        let sends = fake.send_calls.lock().unwrap();
        assert_eq!(
            sends.len(),
            1,
            "exactly one send-line should fire: {sends:?}"
        );
        assert_eq!(sends[0].0, record.tmux_name);
        assert_eq!(sends[0].1, "wire up the widget");
    }

    #[tokio::test]
    async fn inject_when_ready_skips_empty_task() {
        // Metadata-vs-inject: an empty task is a no-op — nothing is typed.
        let dir = TempDir::new().unwrap();
        let (mgr, fake) = make_manager(&dir).await;
        let record = mgr
            .create("   ".into(), None, None, None, None, None)
            .await
            .expect("create");

        let injected = mgr
            .inject_task_when_ready(&record.id, "   ")
            .await
            .expect("inject");

        assert!(!injected, "empty task must not be injected");
        assert!(
            fake.send_calls.lock().unwrap().is_empty(),
            "no send-line should fire for an empty task"
        );
    }
}
