//! Turnkey task injection: deliver a session's `--task` into its pane once the
//! runtime is ready (issues #1903 / #1299), with delivery-status observability
//! and pane-content readiness hardening (#2364, follow-up from the #2361 review).
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
//! so injecting there would duplicate it. Delivery updates
//! [`super::injection_status::InjectionStatus`] on the session record at every transition
//! (`Pending` → `Success`/`FailedTimeout`/`FailedSessionDied`) so callers can poll
//! it instead of blind-waiting. The readiness check itself is hardened beyond the
//! bare "`claude` PID exists" probe: [`pane_shows_blocking_modal`] additionally
//! inspects captured pane content (via the SAME pane-scoped `capture_pane`/
//! `capture` fallback `SessionManager::observe` uses — #2545 convention, never a
//! session-scoped read when a `pane_id` is known) for known first-run onboarding /
//! trust-dialog markers, so a modal that would otherwise silently swallow the
//! injected keystrokes instead keeps the loop polling (and, if the modal never
//! clears, correctly reports `FailedTimeout` instead of the #2361-review-flagged
//! silent-discard bug).
//! [`SessionManager::pane_readiness`] (#3591) factors the `runtime_ready` +
//! `pane_shows_blocking_modal` conjunction out of this module's poll loop into
//! one [`PaneReadiness`]-returning probe. `SessionManager::send_input`
//! (`super::manager`, also #3591) reuses this module's modal-detection
//! machinery directly (`pane_shows_blocking_modal` +
//! `capture_readiness_pane`) as a fail-fast gate, but deliberately does NOT
//! reuse the `runtime_ready` half — see [`PaneReadiness`]'s doc for why (in
//! short: `runtime_ready` is `false` by construction for the fallback
//! no-tmux driver and every hermetic test built on it, which made it too
//! blunt an instrument outside this module's own bounded, just-spawned poll
//! loop).
//! Test: `should_inject_*` (pure gate), `pane_shows_blocking_modal_*` (pure
//! marker match), `pane_readiness_*` (the poll loop's probe), and
//! `inject_when_ready_*` (delivery + status transitions via the
//! `FakeTmuxDriver` seam) in this module's `tests` submodule.

use std::time::Duration;

use tracing::{info, warn};

use crate::runtime::RuntimeKind;

use super::injection_status::InjectionStatus;
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

/// Number of trailing pane lines inspected by the blocking-modal probe.
///
/// Why: a first-run onboarding wizard or trust dialog renders near the bottom
/// of the pane; capturing a modest tail is enough to catch it without paying
/// for a full-scrollback capture on every readiness poll.
const MODAL_PROBE_LINES: usize = 60;

/// Known first-run onboarding / trust-dialog copy that indicates the pane is
/// showing a BLOCKING modal rather than its normal ready input prompt
/// (issue #2364b, readiness-probe hardening).
///
/// Why: `runtime_ready`'s `claude`-PID probe is a NECESSARY but not SUFFICIENT
/// readiness signal — the pane exists and the process has exec'd the instant
/// the PID appears, but a first-run onboarding modal or trust prompt on a
/// fresh `CLAUDE_CONFIG_DIR` can still be occupying the TUI's input focus at
/// that moment (per the #2361 review: this precedes the TUI-input-ready
/// state). Typing the task then would have it silently consumed by the modal
/// instead of landing as a task. Rather than trying to positively match Claude
/// Code's normal ready-prompt rendering (which can drift across releases),
/// this is a conservative, deliberately narrow BLOCKLIST of modal copy: as
/// long as none of these markers appear in the captured tail, the pane is
/// presumed to be showing its normal ready state.
/// What: case-sensitive substring markers checked by
/// [`pane_shows_blocking_modal`]. A false negative (unrecognized modal text)
/// leaves behavior unchanged from before this hardening; a false positive
/// (marker coincidentally appears in unrelated pane content) merely delays
/// injection by one backoff step — the bounded retry loop still terminates via
/// [`READY_MAX_ATTEMPTS`], now correctly reporting `FailedTimeout` rather than
/// silently injecting into the modal.
const BLOCKING_MODAL_MARKERS: &[&str] = &[
    "Do you trust the files in this folder",
    "Choose the text style that looks best",
    "Enable bypass permissions mode",
    "Welcome to Claude Code",
];

/// Pure check: does captured pane text show a known first-run/trust modal?
///
/// Why: split out of the readiness loop so the marker match is independently
/// unit-testable without a tmux driver or session record.
/// What: returns `true` iff `pane_text` contains any [`BLOCKING_MODAL_MARKERS`]
/// entry as a substring.
/// Test: `pane_shows_blocking_modal_true_for_trust_dialog`,
/// `pane_shows_blocking_modal_false_for_normal_prompt`.
pub fn pane_shows_blocking_modal(pane_text: &str) -> bool {
    BLOCKING_MODAL_MARKERS.iter().any(|m| pane_text.contains(m))
}

/// Outcome of a one-shot pane-readiness probe used by
/// [`SessionManager::inject_task_when_ready`]'s bounded poll loop (issue #3591
/// factored this out of the loop body).
///
/// Why NOT also used by `send_input`'s gate (issue #3591 design note): the
/// natural instinct is to reuse this wholesale for `send_input` too, but
/// `runtime_ready` looks for a live `claude` PID via `ps`, which is `false`
/// by definition for [`super::real_tmux::FakeNoopTmuxDriver`] (the
/// documented "tmux is not installed" fallback — its `list_sessions` always
/// returns empty, so `session_exists`/`runtime_ready`'s default impls can
/// never report `true`) and for every hermetic test built on
/// `DaemonState::with_root_isolated_managed`, which wires that same fake
/// driver specifically so handler/proxy/connector tests don't need a real
/// tmux. Gating `send_input` on `runtime_ready` was tried and reverted after
/// it broke three such tests
/// (`connectors::tm::tests::create_session_full_lifecycle`,
/// `daemon::managed_routes::proxy::tests::proxy_message_route_focused_sends_unfocused_no_focus`,
/// `daemon::mcp_proxy::tests::mcp_proxy_focus_message_summary_unfocus_round_trip`)
/// — proof that a hard `runtime_ready` gate has too high a false-positive
/// rate outside the narrow, bounded, just-spawned context
/// `inject_task_when_ready` uses it in. `send_input`'s gate instead reuses
/// only the modal-detection half directly (see
/// `super::manager::SessionManager::send_input`), and relies on its OWN
/// state guard (refusing `Provisioning`/`Errored`) for the shell-execution
/// risk this issue is about.
/// What: `Ready` — the runtime PID is up AND the captured pane tail shows no
/// known blocking-modal marker. `RuntimeNotReady` — the PID has not appeared
/// yet (still booting), is definitively absent (crashed out to a bare
/// shell), or the driver has no real signal to offer (see above).
/// `BlockingModal` — the PID is up but the pane tail matches a
/// [`BLOCKING_MODAL_MARKERS`] entry.
/// Test: `pane_readiness_ready_when_pid_up_and_no_modal`,
/// `pane_readiness_not_ready_when_pid_absent`,
/// `pane_readiness_blocking_modal_when_pid_up_but_modal_shown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneReadiness {
    /// Runtime PID present, no known blocking modal in the captured tail.
    Ready,
    /// The runtime PID has not (yet, or ever) appeared.
    RuntimeNotReady,
    /// PID present but the pane tail matches a known first-run/trust-dialog marker.
    BlockingModal,
}

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
    /// Deliver `task` into the session's pane once its runtime is ready (#1903),
    /// tracking delivery status on the record throughout (#2364).
    ///
    /// Why: this is the turnkey half of `--task` — after the pane is up and the
    /// runtime has launched, the task is typed in through the SAME seam
    /// `tm session send` uses ([`Self::send_input`] → `send_line` + Enter), so a
    /// caller that treats `--task` as "start working immediately" gets exactly
    /// that. Delivery MUST wait for readiness: keystrokes sent before `claude`
    /// has exec'd (or while a first-run onboarding/trust modal is occupying
    /// input focus) are lost, so this polls
    /// [`super::driver::ManagedTmuxDriver::runtime_ready`] AND
    /// [`pane_shows_blocking_modal`] with bounded exponential backoff rather
    /// than a blind sleep. Before this fix injection was fire-and-forget — a
    /// backoff exhaustion or a session dying mid-wait was only ever a `warn!`
    /// log line, with no caller-visible signal; every exit path now also
    /// updates [`super::injection_status::InjectionStatus`] via [`Self::set_injection_status`].
    /// What: returns `Ok(false)` early for an empty task (status stays
    /// `NotApplicable` — this mirrors [`should_inject_task`]'s empty-task gate,
    /// so a direct call with a blank task is a documented no-op, not a
    /// reportable failure). Otherwise marks the record `Pending` and polls
    /// readiness up to [`READY_MAX_ATTEMPTS`] times (backoff
    /// [`READY_BACKOFF_START`]→[`READY_BACKOFF_MAX`]): each attempt bails
    /// `FailedSessionDied` (with a warning, `Ok(false)`) if the session
    /// transitioned to `Stopped`/`Errored`/`Decommissioned` while waiting, and
    /// only treats the runtime as ready when BOTH `runtime_ready` reports the
    /// `claude` PID present AND the captured pane tail shows no known
    /// blocking-modal marker (pane-scoped `capture_pane` when the record's
    /// `pane_id` is known, session-scoped `capture` otherwise — mirrors
    /// [`Self::observe`]'s #2545 pane-scoped-capture convention; never a
    /// session-scoped read when a pane id is known). Exhausting the budget
    /// (whether from PID absence or a persistently blocked modal) marks
    /// `FailedTimeout`. On readiness it calls [`Self::send_input`]: success
    /// marks `Success` and returns `Ok(true)`; a failure at this late stage
    /// (readiness already confirmed, so the pane became undeliverable-to in
    /// the interim — practically indistinguishable from the session dying)
    /// marks `FailedSessionDied` and propagates the `Err`. The task is NOT
    /// lost on any failure path — it remains on the record as metadata,
    /// exactly as before this fix, so a caller can still deliver it manually
    /// via `tm session send`.
    /// Test: `inject_when_ready_sends_task_via_send_seam`,
    /// `inject_when_ready_skips_empty_task`,
    /// `inject_when_ready_marks_success_status`,
    /// `inject_when_ready_marks_failed_session_died_status`,
    /// `inject_when_ready_times_out_when_modal_never_clears`,
    /// `inject_when_ready_checks_pane_scoped_capture_when_pane_id_known` in
    /// this module's `tests`.
    pub async fn inject_task_when_ready(
        &self,
        id: &ManagedSessionId,
        task: &str,
    ) -> Result<bool, ManagedError> {
        if task.trim().is_empty() {
            return Ok(false);
        }
        let initial = self.get(id).await?;
        let name = initial.tmux_name;
        let pane_id = initial.pane_id;

        self.set_injection_status(id, InjectionStatus::Pending)
            .await;

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
                self.set_injection_status(id, InjectionStatus::FailedSessionDied)
                    .await;
                return Ok(false);
            }
            if self.pane_readiness(&name, pane_id.as_deref()) == PaneReadiness::Ready {
                ready = true;
                break;
            }
        }

        if !ready {
            warn!(
                id = %id,
                name = %name,
                "runtime not ready within budget (PID absent or blocked by a first-run modal); \
                 task NOT injected (retained as metadata — deliver it with `tm session send`)"
            );
            self.set_injection_status(id, InjectionStatus::FailedTimeout)
                .await;
            return Ok(false);
        }

        // A `send_input` failure at this point (readiness already confirmed)
        // means the pane became undeliverable-to between the last readiness
        // check and now — practically indistinguishable from the session
        // having died out from under us, so it is recorded as
        // `FailedSessionDied` (#2364) rather than left stuck at `Pending`
        // forever, which would recreate the exact observability gap this
        // issue exists to close.
        if let Err(e) = self.send_input(id, task).await {
            warn!(
                id = %id,
                name = %name,
                "send_input failed after readiness was confirmed: {e}; task retained as metadata"
            );
            self.set_injection_status(id, InjectionStatus::FailedSessionDied)
                .await;
            return Err(e);
        }
        info!(id = %id, name = %name, "injected --task into ready session pane (#1903)");
        self.set_injection_status(id, InjectionStatus::Success)
            .await;
        Ok(true)
    }

    /// Capture the pane tail used by the readiness-modal probe, pane-scoped
    /// when the record captured a `pane_id` at spawn time (#2545 convention).
    ///
    /// Why: split out so [`Self::inject_task_when_ready`]'s poll loop reads as
    /// one clear condition; mirrors [`Self::observe`]'s identical
    /// pane-id-known → `capture_pane` / else → `capture` fallback so the
    /// modal probe never reintroduces a session-scoped read when a pane id is
    /// already known (the sibling-window hijack #2468 closed for every other
    /// pane read). Capture failures degrade to an empty string — a read
    /// failure must never itself look like "modal detected"; it is treated as
    /// "nothing to flag," leaving the PID-presence check as the sole
    /// readiness signal for that attempt (the loop simply retries).
    /// `pub(super)` (#3591): also called directly by [`Self::send_input`]'s
    /// modal-only gate in `super::manager` — see that method's doc for why it
    /// reuses this capture but NOT [`Self::pane_readiness`] wholesale.
    /// What: `capture_pane(name, pane_id, MODAL_PROBE_LINES)` when `pane_id`
    /// is `Some`, else `capture(name, MODAL_PROBE_LINES)`.
    /// Test: exercised via `inject_when_ready_waits_out_blocking_modal_then_succeeds`
    /// (pane-scoped) and the session-scoped fallback is covered by every other
    /// `inject_when_ready_*` test (no `pane_id` seeded); `send_input`'s reuse
    /// is covered by `manager_send_input_rejected_when_pane_shows_blocking_modal`.
    pub(super) fn capture_readiness_pane(&self, name: &str, pane_id: Option<&str>) -> String {
        match pane_id {
            Some(p) => self.tmux.capture_pane(name, p, MODAL_PROBE_LINES),
            None => self.tmux.capture(name, MODAL_PROBE_LINES),
        }
        .unwrap_or_default()
    }

    /// One-shot readiness probe used by [`Self::inject_task_when_ready`]'s
    /// bounded poll loop (issue #3591 factored this out of the loop body; see
    /// [`PaneReadiness`]'s doc for why `send_input` deliberately does NOT
    /// reuse this wholesale).
    ///
    /// Why: split out so the loop reads as one clear condition instead of the
    /// `runtime_ready(...) && !pane_shows_blocking_modal(...)` conjunction
    /// inline.
    /// What: short-circuits to [`PaneReadiness::RuntimeNotReady`] without
    /// paying for a pane capture when `runtime_ready` is false; otherwise
    /// inspects the captured tail (pane-scoped via
    /// [`Self::capture_readiness_pane`] when `pane_id` is known — #2545
    /// convention) for a [`BLOCKING_MODAL_MARKERS`] hit.
    /// Test: `pane_readiness_ready_when_pid_up_and_no_modal`,
    /// `pane_readiness_not_ready_when_pid_absent`,
    /// `pane_readiness_blocking_modal_when_pid_up_but_modal_shown`.
    pub(super) fn pane_readiness(&self, name: &str, pane_id: Option<&str>) -> PaneReadiness {
        if !self.tmux.runtime_ready(name) {
            return PaneReadiness::RuntimeNotReady;
        }
        if pane_shows_blocking_modal(&self.capture_readiness_pane(name, pane_id)) {
            return PaneReadiness::BlockingModal;
        }
        PaneReadiness::Ready
    }

    /// Persist an [`InjectionStatus`] transition on the session record (#2364).
    ///
    /// Why: every exit path of [`Self::inject_task_when_ready`] must leave a
    /// caller-visible trace of what happened — this is the single write point
    /// so the status enum's meaning cannot drift between call sites. Mirrors
    /// [`Self::set_deliverable_id`]'s simple get-mutate-upsert shape (no retry
    /// loop): a lost status update degrades to a stale `Pending`/prior value,
    /// which is a visibility gap, not a correctness hazard (the actual
    /// delivery outcome — sent or not — is unaffected).
    /// What: fetches the current record, overwrites `injection_status`, and
    /// upserts. Errors are logged and swallowed (`fn` returns nothing) so a
    /// transient store failure never aborts injection itself — the caller
    /// already has its own `Result` for the substantive outcome.
    /// Test: `inject_when_ready_marks_success_status`,
    /// `inject_when_ready_marks_failed_session_died_status`,
    /// `inject_when_ready_skips_empty_task` (asserts status stays
    /// `NotApplicable` for the early-return empty-task path).
    async fn set_injection_status(&self, id: &ManagedSessionId, status: InjectionStatus) {
        let mut record = match self.get(id).await {
            Ok(r) => r,
            Err(e) => {
                warn!(id = %id, "set_injection_status({status}): record lookup failed: {e}");
                return;
            }
        };
        record.injection_status = status;
        if let Err(e) = self.store.write().await.upsert(record).await {
            warn!(id = %id, "set_injection_status({status}): persist failed: {e}");
        }
    }

    /// Refuse [`Self::send_input`] (`super::manager`) for a session that is
    /// not safe to type into (issue #3591).
    ///
    /// Why: lives beside the readiness/modal machinery it reuses (and out of
    /// `manager.rs`, which is at its 500-SLOC production cap) rather than a
    /// second copy inline in `send_input` — see [`PaneReadiness`]'s doc for
    /// why this reuses only the modal half, not the `runtime_ready` half.
    /// What: refuses `Provisioning`/`Errored` outright (the shell-execution
    /// risk this issue is about); for `RuntimeKind::ClaudeCode` also refuses
    /// a pane showing a [`BLOCKING_MODAL_MARKERS`] hit. Fail-fast: a single
    /// probe, no retry loop — see `send_input`'s doc for the block-vs-fail-fast
    /// rationale.
    /// Test: `manager_send_input_rejected_for_provisioning_and_errored`,
    /// `manager_send_input_rejected_when_pane_shows_blocking_modal`,
    /// `manager_send_input_skips_modal_probe_for_tcode` (in the sibling
    /// `send_input_gate_tests.rs`).
    pub(super) async fn check_send_input_ready(
        &self,
        id: &ManagedSessionId,
    ) -> Result<(), ManagedError> {
        let record = self.get(id).await?;
        if matches!(
            record.state,
            ManagedSessionState::Provisioning | ManagedSessionState::Errored
        ) {
            return Err(ManagedError::TmuxUnavailable(format!(
                "session {} is {}; cannot inject input",
                record.tmux_name, record.state
            )));
        }
        if record.runtime == RuntimeKind::ClaudeCode
            && pane_shows_blocking_modal(
                &self.capture_readiness_pane(&record.tmux_name, record.pane_id.as_deref()),
            )
        {
            return Err(ManagedError::TmuxUnavailable(format!(
                "session {} is showing a blocking onboarding/trust modal; \
                 cannot inject input until it is dismissed",
                record.tmux_name
            )));
        }
        Ok(())
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
        // `spawn_task_injection` only calls `inject_task_when_ready` once
        // `should_inject_task` has already confirmed the record reached
        // `Active` (see `lifecycle.rs::spawn_task_injection`) — mirror that
        // precondition here rather than leaving the record `Provisioning`
        // (#3591: `send_input`'s new state guard now refuses `Provisioning`).
        {
            let mut store = mgr.store.write().await;
            let mut r = store.get(&record.id).await.unwrap();
            r.state = ManagedSessionState::Active;
            store.upsert(r).await.unwrap();
        }

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
        // #2364: the early-return empty-task path never begins polling, so the
        // status must stay the untouched NotApplicable default — not Pending.
        let after = mgr.get(&record.id).await.expect("get");
        assert_eq!(after.injection_status, InjectionStatus::NotApplicable);
    }

    #[test]
    fn pane_shows_blocking_modal_true_for_trust_dialog() {
        let pane = "some banner text\nDo you trust the files in this folder?\n1. Yes  2. No";
        assert!(pane_shows_blocking_modal(pane));
    }

    #[test]
    fn pane_shows_blocking_modal_true_for_onboarding_welcome() {
        let pane = "Welcome to Claude Code!\nLet's get you set up.";
        assert!(pane_shows_blocking_modal(pane));
    }

    #[test]
    fn pane_shows_blocking_modal_false_for_normal_prompt() {
        let pane = "╭──────────────────────╮\n│ >                    │\n╰──────────────────────╯";
        assert!(!pane_shows_blocking_modal(pane));
    }

    #[test]
    fn pane_shows_blocking_modal_false_for_empty_pane() {
        assert!(!pane_shows_blocking_modal(""));
    }

    #[tokio::test]
    async fn pane_readiness_ready_when_pid_up_and_no_modal() {
        // #3591: the happy path both callers (poll loop and send_input's
        // fail-fast gate) rely on — PID up (FakeTmuxDriver's default
        // runtime_ready == session_exists, true right after create) and no
        // capture_responses seeded (empty tail, no marker match).
        let dir = TempDir::new().unwrap();
        let (mgr, _fake) = make_manager(&dir).await;
        let record = mgr
            .create("wire up SSO".into(), None, None, None, None, None)
            .await
            .expect("create");

        assert_eq!(
            mgr.pane_readiness(&record.tmux_name, record.pane_id.as_deref()),
            PaneReadiness::Ready
        );
    }

    #[tokio::test]
    async fn pane_readiness_not_ready_when_pid_absent() {
        // #3591: a pane with no live runtime PID (still booting, or crashed
        // out — modeled here via kill_session, which drops the fake's
        // session_exists/runtime_ready signal) must report RuntimeNotReady,
        // and must NOT pay for a modal-marker capture in that case.
        let dir = TempDir::new().unwrap();
        let (mgr, _fake) = make_manager(&dir).await;
        let record = mgr
            .create("wire up SSO".into(), None, None, None, None, None)
            .await
            .expect("create");
        mgr.tmux_driver()
            .kill_session(&record.tmux_name)
            .expect("kill_session");

        assert_eq!(
            mgr.pane_readiness(&record.tmux_name, record.pane_id.as_deref()),
            PaneReadiness::RuntimeNotReady
        );
    }

    #[tokio::test]
    async fn pane_readiness_blocking_modal_when_pid_up_but_modal_shown() {
        // #3591: PID present but the captured tail matches a known
        // onboarding/trust-dialog marker — must report BlockingModal, not Ready.
        let dir = TempDir::new().unwrap();
        let (mgr, fake) = make_manager(&dir).await;
        let record = mgr
            .create("wire up SSO".into(), None, None, None, None, None)
            .await
            .expect("create");
        fake.capture_responses.lock().unwrap().insert(
            record.tmux_name.clone(),
            "Do you trust the files in this folder?".into(),
        );

        assert_eq!(
            mgr.pane_readiness(&record.tmux_name, record.pane_id.as_deref()),
            PaneReadiness::BlockingModal
        );
    }

    #[tokio::test]
    async fn inject_when_ready_marks_success_status() {
        // #2364a: a successful delivery must leave the record's
        // injection_status as Success, not just return Ok(true).
        let dir = TempDir::new().unwrap();
        let (mgr, _fake) = make_manager(&dir).await;
        let record = mgr
            .create("ship the feature".into(), None, None, None, None, None)
            .await
            .expect("create");
        // See the matching comment in `inject_when_ready_sends_task_via_send_seam`:
        // mirror `spawn_task_injection`'s Active precondition (#3591).
        {
            let mut store = mgr.store.write().await;
            let mut r = store.get(&record.id).await.unwrap();
            r.state = ManagedSessionState::Active;
            store.upsert(r).await.unwrap();
        }

        let injected = mgr
            .inject_task_when_ready(&record.id, "ship the feature")
            .await
            .expect("inject");
        assert!(injected);

        let after = mgr.get(&record.id).await.expect("get");
        assert_eq!(after.injection_status, InjectionStatus::Success);
    }

    #[tokio::test]
    async fn inject_when_ready_marks_failed_session_died_status() {
        // #2364a: a session that dies mid-wait must report FailedSessionDied,
        // not just a silent Ok(false).
        let dir = TempDir::new().unwrap();
        let (mgr, fake) = make_manager(&dir).await;
        let record = mgr
            .create("do the thing".into(), None, None, None, None, None)
            .await
            .expect("create");
        mgr.mark_errored(&record.id, "spawn failed")
            .await
            .expect("mark_errored");

        let injected = mgr
            .inject_task_when_ready(&record.id, "do the thing")
            .await
            .expect("inject");

        assert!(!injected, "a dead session must never receive the task");
        assert!(
            fake.send_calls.lock().unwrap().is_empty(),
            "no send-line should fire once the session is Errored"
        );
        let after = mgr.get(&record.id).await.expect("get");
        assert_eq!(after.injection_status, InjectionStatus::FailedSessionDied);
    }

    #[tokio::test(start_paused = true)]
    async fn inject_when_ready_times_out_when_modal_never_clears() {
        // #2364b: a PID that exists but a pane permanently showing a
        // first-run/trust modal must NEVER receive the task — the readiness
        // loop must keep retrying (not silently inject into the modal) and
        // eventually report FailedTimeout once the backoff budget is spent.
        // `start_paused` fast-forwards Tokio's virtual clock through the
        // ~30-attempt backoff schedule without a real ~60s wall-clock wait.
        let dir = TempDir::new().unwrap();
        let (mgr, fake) = make_manager(&dir).await;
        let record = mgr
            .create("wire up SSO".into(), None, None, None, None, None)
            .await
            .expect("create");
        fake.capture_responses.lock().unwrap().insert(
            record.tmux_name.clone(),
            "Do you trust the files in this folder?".into(),
        );

        let injected = mgr
            .inject_task_when_ready(&record.id, "wire up SSO")
            .await
            .expect("inject");

        assert!(
            !injected,
            "a pane permanently blocked by a modal must never be injected"
        );
        assert!(
            fake.send_calls.lock().unwrap().is_empty(),
            "the modal-guarded loop must never call send_input while blocked"
        );
        let after = mgr.get(&record.id).await.expect("get");
        assert_eq!(after.injection_status, InjectionStatus::FailedTimeout);
    }

    #[tokio::test(start_paused = true)]
    async fn inject_when_ready_checks_pane_scoped_capture_when_pane_id_known() {
        // #2364b + #2545 convention: when the record captured a pane_id at
        // spawn time, the modal probe must read via the PANE-SCOPED
        // capture_pane (keyed by pane_id in the fake), never the
        // session-scoped capture — proves the hardening does not reintroduce
        // a session-scoped read.
        let dir = TempDir::new().unwrap();
        let (mgr, fake) = make_manager(&dir).await;
        *fake.pane_id_override.lock().unwrap() = Some("%7".to_string());
        let record = mgr
            .create("wire up SSO".into(), None, None, None, None, None)
            .await
            .expect("create");
        assert_eq!(
            record.pane_id.as_deref(),
            Some("%7"),
            "create() must have captured the overridden pane_id"
        );
        fake.capture_responses.lock().unwrap().insert(
            "%7".to_string(),
            "Do you trust the files in this folder?".into(),
        );

        let injected = mgr
            .inject_task_when_ready(&record.id, "wire up SSO")
            .await
            .expect("inject");

        assert!(
            !injected,
            "the pane-scoped modal marker must block injection"
        );
        assert!(
            !fake.pane_capture_calls.lock().unwrap().is_empty(),
            "the modal probe must have used the pane-scoped capture_pane path"
        );
        let after = mgr.get(&record.id).await.expect("get");
        assert_eq!(after.injection_status, InjectionStatus::FailedTimeout);
    }
}
