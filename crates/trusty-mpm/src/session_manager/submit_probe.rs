//! Post-send submit observation for `session_send` / `tm session send` (#5699).
//!
//! Why: [`SessionManager::send_input`](super::manager::SessionManager::send_input)
//! reports success as soon as the two tmux subprocesses exit 0, and the
//! `session_send` MCP tool turned that into `{"sent": true}`. Characters
//! reaching the pane is NOT the same fact as the message being submitted: a
//! long single-line send crosses Claude Code's bracketed-paste threshold, the
//! harness collapses it into a `[Pasted text #1]` placeholder, and the trailing
//! Enter is consumed as paste content instead of committing the buffer. The
//! text then sits in the input buffer indefinitely while the caller believes it
//! was delivered — the tool reported success on the exact failure it exists to
//! catch. Observed twice on live sessions (issue #5699, 2026-08 and 2026-09-16).
//! What: a pure classifier over captured pane text
//! ([`classify_submit`]) plus the manager wrapper
//! [`SessionManager::send_input_observed`] that sends, re-reads the pane, and
//! returns the [`SubmitState`] it can actually observe. The classification is
//! deliberately three-valued — a pane that could not be read reports
//! [`SubmitState::Unverified`] rather than claiming either outcome, so the
//! caller is never told "submitted" on no evidence.
//! Test: `classify_submit_*` in this module's `tests`;
//! `send_input_observed_reports_unsubmitted_paste` and
//! `send_input_observed_reports_submitted_on_a_clean_prompt` in
//! `send_input_gate_tests.rs`; `session_send_reports_an_unsubmitted_paste` in
//! `daemon::mcp_session`.

use super::manager::{ManagedError, SessionManager};
use super::record::ManagedSessionId;

/// Number of trailing pane lines the submit probe inspects.
///
/// Why: a collapsed-paste placeholder renders on the prompt line at the bottom
/// of the pane. A short tail is enough to see it and keeps the extra capture
/// cheap, matching `task_inject`'s `MODAL_PROBE_LINES` reasoning.
const SUBMIT_PROBE_LINES: usize = 20;

/// Pane copy that means the injected text is sitting UNSUBMITTED in Claude
/// Code's input buffer as a collapsed bracketed paste (#5699).
///
/// Why: the harness does not echo long pasted input verbatim — it renders a
/// placeholder on the prompt line and waits for an explicit submit. Both
/// recorded incidents showed one of these two shapes: the numbered placeholder
/// (`❯ [Pasted text #1][Pasted text #2]`) and the collapsed-paste hint. Seeing
/// either AFTER a send is direct evidence the Enter did not commit.
const UNSUBMITTED_PASTE_MARKERS: &[&str] = &["[Pasted text #", "paste again to expand"];

/// What a post-send pane read could establish about the message's submission.
///
/// Why: the pre-#5699 `sent: true` boolean conflated three distinct facts —
/// "committed", "still in the input buffer", and "could not tell". Only the
/// middle one is a failure, and only the first one is a success; collapsing the
/// third into either is what made the tool fail open.
/// What: a three-valued verdict, serialized on the wire as [`Self::as_str`].
/// Test: `classify_submit_*` in this module's `tests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitState {
    /// The pane showed no pending paste buffer after the send.
    Submitted,
    /// The pane still shows a collapsed bracketed paste — the text was typed
    /// but never committed.
    UnsubmittedPaste,
    /// The pane could not be read, so neither outcome was established.
    Unverified,
}

impl SubmitState {
    /// Wire spelling, used by the MCP `session_send` response's `submit_state`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::UnsubmittedPaste => "unsubmitted_paste",
            Self::Unverified => "unverified",
        }
    }

    /// Whether the message may be treated as delivered.
    ///
    /// Why: `false` ONLY for the positively-observed failure. An unreadable
    /// pane keeps the pre-#5699 answer (`true`) rather than inventing a
    /// failure — every hermetic caller and every driver without a capture
    /// would otherwise start reporting a delivery problem that was never
    /// observed. The `submit_state` field carries the distinction the boolean
    /// cannot.
    /// Test: `classify_submit_unreadable_pane_is_unverified`.
    pub fn delivered(self) -> bool {
        !matches!(self, Self::UnsubmittedPaste)
    }
}

/// Pure classifier: what does this post-send pane capture prove? (#5699)
///
/// Why: split from the manager wrapper so the marker match is unit-testable
/// with no tmux driver, mirroring `task_inject::pane_shows_blocking_modal`.
/// What: an empty/whitespace capture is [`SubmitState::Unverified`] (the
/// capture failed, or the driver has none); a capture containing any
/// [`UNSUBMITTED_PASTE_MARKERS`] entry is
/// [`SubmitState::UnsubmittedPaste`]; anything else is
/// [`SubmitState::Submitted`].
/// Test: `classify_submit_flags_a_collapsed_paste`,
/// `classify_submit_flags_the_expand_hint`,
/// `classify_submit_clean_prompt_is_submitted`,
/// `classify_submit_unreadable_pane_is_unverified`.
pub fn classify_submit(pane_text: &str) -> SubmitState {
    if pane_text.trim().is_empty() {
        return SubmitState::Unverified;
    }
    if UNSUBMITTED_PASTE_MARKERS
        .iter()
        .any(|m| pane_text.contains(m))
    {
        return SubmitState::UnsubmittedPaste;
    }
    SubmitState::Submitted
}

impl SessionManager {
    /// Send a line into a session's pane and report the submit state that a
    /// pane read can actually establish (#5699).
    ///
    /// Why: the MCP `session_send` tool's caller is a PM coordinating another
    /// live session — it acts on the answer, so an answer that cannot fail is
    /// worse than no answer. This is the observing wrapper; plain
    /// [`Self::send_input`] is left untouched for the callers (task injection,
    /// the HTTP route, the proxy backend) whose contract is `Result<(), _>`.
    /// What: delegates the dispatch and every readiness/modal refusal to
    /// [`Self::send_input`], then re-captures the pane tail through the SAME
    /// pane-scoped `capture_pane`/`capture` fallback `observe` uses (#2545
    /// convention) and runs [`classify_submit`] over it. A send that was
    /// REFUSED never reaches the probe — the `Err` already says nothing was
    /// typed.
    /// Test: `send_input_observed_reports_unsubmitted_paste`,
    /// `send_input_observed_reports_submitted_on_a_clean_prompt` in
    /// `send_input_gate_tests.rs`.
    pub async fn send_input_observed(
        &self,
        id: &ManagedSessionId,
        text: &str,
    ) -> Result<SubmitState, ManagedError> {
        self.send_input(id, text).await?;
        let record = self.get(id).await?;
        let pane = self.capture_submit_pane(&record.tmux_name, record.pane_id.as_deref());
        Ok(classify_submit(&pane))
    }

    /// Capture the pane tail the submit probe classifies.
    ///
    /// Why: a one-line helper rather than a direct `capture_readiness_pane`
    /// call so the probe owns its own line budget — the modal probe's 60 lines
    /// are more than a prompt-line check needs.
    /// What: pane-scoped when a `pane_id` is known, session-scoped otherwise
    /// (matching #2467's legacy-record fallback); a driver error yields an
    /// empty string, which [`classify_submit`] reads as
    /// [`SubmitState::Unverified`].
    /// Test: covered through [`Self::send_input_observed`].
    fn capture_submit_pane(&self, name: &str, pane_id: Option<&str>) -> String {
        match pane_id {
            Some(p) => self.tmux.capture_pane(name, p, SUBMIT_PROBE_LINES),
            None => self.tmux.capture(name, SUBMIT_PROBE_LINES),
        }
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact prompt line issue #5699 recorded on the live session.
    /// Test: itself.
    #[test]
    fn classify_submit_flags_a_collapsed_paste() {
        let pane = "❯ [Pasted text #1][Pasted text #2]";
        assert_eq!(classify_submit(pane), SubmitState::UnsubmittedPaste);
        assert!(!classify_submit(pane).delivered());
    }

    /// The 2026-09-16 recurrence's shape — the collapsed-paste hint rather
    /// than the numbered placeholder.
    /// Test: itself.
    #[test]
    fn classify_submit_flags_the_expand_hint() {
        assert_eq!(
            classify_submit("ctrl+r to paste again to expand"),
            SubmitState::UnsubmittedPaste
        );
    }

    /// A pane back at its ready prompt proves the buffer committed.
    /// Test: itself.
    #[test]
    fn classify_submit_clean_prompt_is_submitted() {
        let state = classify_submit("● Running the tests…\n❯ ");
        assert_eq!(state, SubmitState::Submitted);
        assert!(state.delivered());
    }

    /// An unreadable pane must claim NEITHER outcome — and must not start
    /// reporting a delivery failure it never observed.
    /// Test: itself.
    #[test]
    fn classify_submit_unreadable_pane_is_unverified() {
        let state = classify_submit("   \n  ");
        assert_eq!(state, SubmitState::Unverified);
        assert!(state.delivered());
        assert_eq!(state.as_str(), "unverified");
    }
}
