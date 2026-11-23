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
use crate::core::sm::control::Submit;

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

/// Interval between input-box reads while waiting for the typed text to echo.
const ECHO_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Longest wait for the typed text to echo before the send is abandoned.
///
/// Why (#10747): a busy pane can take a moment to render a paste; a short
/// message that echoes at once costs one [`ECHO_POLL`] of added latency.
const ECHO_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Rows the echo check reads; the input box sits at the bottom of the pane.
const ECHO_PROBE_LINES: usize = 40;

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
    /// #10747: the input box never showed the typed text (or a paste
    /// placeholder for it), so Enter was NOT pressed and the box was cleared.
    NotEchoed,
    /// #10747: the input box already held text (a user draft or a stale
    /// paste placeholder) or could not be read, so nothing was typed.
    InputNotEmpty,
}

impl SubmitState {
    /// Wire spelling, used by the MCP `session_send` response's `submit_state`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::UnsubmittedPaste => "unsubmitted_paste",
            Self::Unverified => "unverified",
            Self::NotEchoed => "not_echoed",
            Self::InputNotEmpty => "input_not_empty",
        }
    }

    /// Why the message was not delivered; `None` when it may have been.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::UnsubmittedPaste => Some("the text sits unsubmitted as a collapsed paste"),
            Self::NotEchoed => Some(
                "the input box did not show the full text before Enter; nothing was \
                 submitted and the box was cleared",
            ),
            Self::InputNotEmpty => Some(
                "input_not_empty: the input box was not provably empty before typing; \
                 nothing was typed",
            ),
            Self::Submitted | Self::Unverified => None,
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
        !matches!(
            self,
            Self::UnsubmittedPaste | Self::NotEchoed | Self::InputNotEmpty
        )
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
    /// What: runs [`Self::send_input`]'s readiness/modal refusals, then types
    /// `text` WITHOUT Enter through the same owned-pane gate. #10747: only for
    /// a Claude Code runtime (other runtimes have no `❯` box and keep the
    /// type-plus-Enter send): it first reads the box with escapes and answers
    /// [`SubmitState::InputNotEmpty`], typing nothing, unless
    /// [`trusty_architect::input_state_for_send`] accepts input. It then polls
    /// the input box every [`ECHO_POLL`] for up to [`ECHO_DEADLINE`] until
    /// [`trusty_architect::box_holds_text`] holds; only then is Enter pressed.
    /// No echo by the deadline: `C-u` clears the box and the answer is
    /// [`SubmitState::NotEchoed`]. After Enter it re-captures the tail of the
    /// pane that gate proved (#9101: never a fresh, ungated lookup) and runs
    /// [`classify_submit`] over it. A send that was REFUSED never reaches the
    /// probe — the `Err` already says nothing was typed. A capture error
    /// yields an empty string, which [`classify_submit`] reads as
    /// [`SubmitState::Unverified`].
    /// Test: `send_input_observed_reports_unsubmitted_paste`,
    /// `send_input_observed_reports_submitted_on_a_clean_prompt`,
    /// `send_input_observed_withholds_enter_when_the_head_is_dropped`,
    /// `send_input_observed_presses_enter_after_a_full_echo` in
    /// `send_input_gate_tests.rs`.
    pub async fn send_input_observed(
        &self,
        id: &ManagedSessionId,
        text: &str,
    ) -> Result<SubmitState, ManagedError> {
        self.check_send_input_ready(id).await?;
        let record = self.get(id).await?;
        // #10747: the echo check reads Claude Code's `❯` box; other runtimes
        // have none, so they keep the type-plus-Enter send.
        if record.runtime != crate::runtime::RuntimeKind::ClaudeCode {
            let (name, pane) = self.inject_into_owned(id, text, Submit::Enter).await?;
            return Ok(self.probe_after_enter(&name, &pane));
        }
        let (record, pane) = self.live_owned_pane(id, record).await?;
        let tmux = super::tmux_route::route(self.tmux.as_ref(), &record)?;
        // #10747: never type into a box holding a draft or a stale paste
        // placeholder; the echo check would pass on it and C-u would erase it.
        let before = tmux
            .capture_pane_escaped(&record.tmux_name, &pane, ECHO_PROBE_LINES)
            .unwrap_or_default();
        if !trusty_architect::input_state_for_send(&before).accepts_input() {
            tracing::warn!(session = %id, "session_send: input box not empty; not typed (#10747)");
            return Ok(SubmitState::InputNotEmpty);
        }
        // #10747: type without Enter; Enter waits for the echo check below.
        let (name, pane) = self.inject_into_owned(id, text, Submit::NoSubmit).await?;
        let key = |k: &str| tmux.send_key_to_pane(&name, &pane, k);
        let deadline = tokio::time::Instant::now() + ECHO_DEADLINE;
        loop {
            tokio::time::sleep(ECHO_POLL).await;
            let cap = tmux
                .capture_pane(&name, &pane, ECHO_PROBE_LINES)
                .unwrap_or_default();
            if trusty_architect::box_holds_text(&cap, text) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                // #10747: an unverified box is never submitted; clear it.
                key("C-u")?;
                tracing::warn!(session = %id, "session_send: text did not echo; not submitted (#10747)");
                return Ok(SubmitState::NotEchoed);
            }
        }
        key("Enter")?;
        Ok(self.probe_after_enter(&name, &pane))
    }

    /// The #5699 read after Enter: [`classify_submit`] over the pane's tail.
    fn probe_after_enter(&self, name: &str, pane: &str) -> SubmitState {
        let tail = self
            .tmux
            .capture_pane(name, pane, SUBMIT_PROBE_LINES)
            .unwrap_or_default();
        classify_submit(&tail)
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
