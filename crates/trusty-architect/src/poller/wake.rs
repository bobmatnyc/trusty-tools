//! Waking the Architect: `fleet-poll.py`'s `wake` and `dispatch_wake`
//! (988152c lines 1211-1296).
//!
//! Why: the poller wakes the Architect by typing a one-line pointer into its
//! input box. That is the only place it sends keys, into a pane where a human
//! may be typing, so every step fails closed (#9171 A3b-1).
//! What: [`Waker::wake`] picks the `tm-architect` pane, reads it, and types
//! the pointer only when `input_state_for_send` reads Empty or Suggestion; it
//! then submits with Enter only when the box text equals the pointer, or
//! erases only its own trailing text, through the typed steps in
//! `crate::pointer`. [`dispatch_wake`] carries the pending count in
//! `poll-state.json` and clears it only on a result starting `sent`.
//! Divergences from the prototype, by Architect ruling: Enter only on
//! equality, never on a substring (A3 Q3, rule 4); an empty box after typing
//! sends nothing (A3b Q1); the skip phrase names the send reading, which can
//! be stricter than `parse_pane`'s `box`.
//! Test: `tests/wake_ported.rs` (the prototype's tests, by name),
//! `tests/wake_send_path.rs` (AC5-7), `tests/parity_wake.rs` (goldens).

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::SUPERVISOR;
use super::box_text::{box_full_text, own_pointer};
use super::listing::{Pane, capture};
use super::state::PollState;
use crate::classify::{input_state_for_send, parse_pane};
use crate::clock::Clock;
use crate::error::WakeError;
use crate::input::classify_input;
use crate::pointer::{Confirmation, Erased, type_pointer, verify_owner};
use crate::tmux::{PaneId, Tmux};

/// The phrase for a pointer that did not land and was not ours to erase
/// (A3b Q1; the prototype erases in this case).
pub const BOX_CHANGED: &str =
    "failed: text not confirmed in the input box; box changed, nothing erased";
const NOT_CONFIRMED: &str = "failed: text not confirmed in the input box";
/// The prototype's waits: after typing, after Enter, after BSpace.
const AFTER_TYPING: Duration = Duration::from_millis(500);
const AFTER_ENTER: Duration = Duration::from_secs(1);
const AFTER_ERASE: Duration = Duration::from_millis(300);

/// What one wake needs: tmux, a clock, the `alerts.jsonl` path the pointer
/// names, and whether this is a dry run.
#[derive(Debug)]
pub struct Waker<'a, T: Tmux + ?Sized, C: Clock + ?Sized> {
    /// The tmux server holding the Architect's pane.
    pub tmux: &'a T,
    /// The clock for the pointer's stamp and the waits.
    pub clock: &'a C,
    /// The alerts file the pointer points at.
    pub alerts: &'a Path,
    /// When true, report what would be sent and send nothing.
    pub dry_run: bool,
}

impl<T: Tmux + ?Sized, C: Clock + ?Sized> Waker<'_, T, C> {
    /// `wake(n, panes, dry_run, logged)`: types the pointer into the
    /// Architect's box and returns the log phrase.
    ///
    /// Why: see the module docs; this is the send path the poller exposes.
    /// What: no `tm-architect` pane, an open dialog, no rule-bounded box, or a
    /// send reading other than Empty/Suggestion returns a `skipped: ...`
    /// phrase after one capture and sends nothing. Otherwise it re-checks the
    /// owner, types the pointer (`[poll HH:MMZ] N new alerts -> path`, or the
    /// "need you" form when `logged` is set), waits, re-reads the box, and
    /// either re-checks the owner and presses Enter (`sent to %N`), or, when
    /// the box ends with the pointer, re-checks the owner and erases it
    /// (`failed: ...`), or sends nothing more ([`BOX_CHANGED`]).
    /// Test: `wake_refuses_table`, `wake_enter_only_when_box_equals_message`,
    /// `wake_rechecks_owner_before_every_send`, `wake_matches_python_goldens`.
    ///
    /// # Errors
    /// [`WakeError::NotOwner`] when the pane's live session is not
    /// `tm-architect`; [`WakeError::Tmux`] when a tmux call fails. The
    /// prototype raises for both; [`dispatch_wake`] reports them as `failed`.
    pub fn wake(&self, panes: &[Pane], n: u64, logged: Option<u64>) -> Result<String, WakeError> {
        self.wake_as(SUPERVISOR, panes, n, logged)
    }

    /// [`Waker::wake`] for the session `owner`. Production passes only
    /// [`SUPERVISOR`]; the scratch-server live test passes its own session.
    fn wake_as(
        &self,
        owner: &str,
        panes: &[Pane],
        n: u64,
        logged: Option<u64>,
    ) -> Result<String, WakeError> {
        let Some(target) = owner_pane(panes, owner) else {
            return Ok(format!("skipped: no {owner} pane"));
        };
        let raw = capture(self.tmux, target)?;
        let info = parse_pane(&raw);
        if info.dialog.is_some() {
            return Ok("skipped: dialog open".to_owned());
        }
        if !info.has_box {
            return Ok("skipped: no input box found".to_owned());
        }
        // #9171: the stricter send reading gates the send, not parse_pane's box.
        let reading = input_state_for_send(&raw);
        if !reading.accepts_input() {
            if own_pointer(&box_full_text(&raw)) {
                return Ok("skipped: own stale pointer in box".to_owned());
            }
            return Ok(format!("skipped: input {reading}"));
        }
        let msg = pointer_text(self.clock.now_unix(), n, logged, self.alerts);
        if self.dry_run {
            return Ok(format!("dry-run: would send to {target}: {msg}"));
        }
        let typed = type_pointer(
            self.tmux,
            verify_owner(self.tmux, target, owner)?,
            reading,
            &msg,
        )?;
        self.clock.sleep(AFTER_TYPING);
        let after = capture(self.tmux, target)?;
        match typed.confirm(&box_full_text(&after)) {
            Confirmation::Confirmed(confirmed) => {
                confirmed.submit(self.tmux, verify_owner(self.tmux, target, owner)?)?;
                self.clock.sleep(AFTER_ENTER);
                let (_, left) = classify_input(&capture(self.tmux, target)?);
                let head: String = msg.chars().take(40).collect();
                let still = if left.contains(&head) {
                    " (still in box after Enter)"
                } else {
                    ""
                };
                Ok(format!("sent to {target}{still}"))
            }
            // #9171 A3b Q1 / A3 rule 4: not ours to erase, so send nothing.
            Confirmation::Mismatch(m) if !m.ours_at_end() => Ok(BOX_CHANGED.to_owned()),
            Confirmation::Mismatch(m) => {
                if m.erase(self.tmux, verify_owner(self.tmux, target, owner)?)? == Erased::Left {
                    return Ok(BOX_CHANGED.to_owned());
                }
                self.clock.sleep(AFTER_ERASE);
                let (state, _) = classify_input(&capture(self.tmux, target)?);
                Ok(if state.accepts_input() {
                    format!("{NOT_CONFIRMED}; cleaned up, box empty")
                } else {
                    format!("{NOT_CONFIRMED}; cleanup left box non-empty")
                })
            }
        }
    }

    /// [`dispatch_wake`] with this waker's [`Waker::wake`] over `panes`.
    ///
    /// Test: `wake_dispatch_types_the_carried_total`.
    pub fn dispatch(
        &self,
        state: &mut PollState,
        panes: &[Pane],
        new_count: u64,
        logged: Option<u64>,
    ) -> String {
        dispatch_wake(
            state,
            new_count,
            logged,
            self.dry_run,
            self.clock,
            |n, l| self.wake(panes, n, l),
        )
    }
}

/// The `owner` session's pane to wake: the active one (`"11"`) first, else the
/// first listed (the prototype's stable sort).
fn owner_pane<'a>(panes: &'a [Pane], owner: &str) -> Option<&'a PaneId> {
    let mut sup = panes.iter().filter(|p| p.session == owner);
    let first = sup.clone().next();
    sup.find(|p| p.active == "11").or(first).map(|p| &p.pane)
}

/// The pointer: `[poll HH:MMZ] N new alerts -> path`, or with `logged`,
/// `[poll HH:MMZ] N alert(s) need(s) you (L logged) -> path`.
fn pointer_text(now: f64, n: u64, logged: Option<u64>, alerts: &Path) -> String {
    // Seconds of the UTC day; `now` before 1970 wraps like a clock face.
    let day = (now.floor() as i64).rem_euclid(86_400);
    let stamp = format!("[poll {:02}:{:02}Z]", day / 3600, day % 3600 / 60);
    let alerts = alerts.display();
    match logged {
        None => format!("{stamp} {n} new alerts -> {alerts}"),
        Some(l) => {
            let need = if n == 1 { " needs" } else { "s need" };
            format!("{stamp} {n} alert{need} you ({l} logged) -> {alerts}")
        }
    }
}

/// An integer counter in `state.other`, 0 when absent.
fn counter(state: &PollState, key: &str) -> u64 {
    match state.other.get(key) {
        None => 0,
        Some(v) => v.as_u64().unwrap_or_else(|| {
            tracing::warn!(key, value = %v, "poll-state counter is not a count; reading 0");
            0
        }),
    }
}

/// `dispatch_wake(state, panes, new_count, dry_run, logged)`: carries
/// `new_count` into `wake_pending` and calls `wake` once when anything is
/// pending.
///
/// Why: a skipped or failed wake must leave the whole total for the next
/// cycle, or alerts go unannounced (#9171 A3b-1, AC7).
/// What: adds `new_count` to `wake_pending` (and `logged`, when given, to
/// `wake_logged`). Nothing pending: drops `wake_logged` and returns `none`
/// without calling `wake`. Otherwise calls `wake(pending, logged_total)`
/// (`logged_total` is `None` when `logged` is) and maps an error to
/// `failed: <message>`. Only a result starting `sent` zeroes `wake_pending`
/// and drops `wake_logged`, and outside a dry run stamps `wake_sent_at` with
/// `clock.now_unix()`. The keys live in [`PollState::other`] until A3b-2
/// types them.
/// Test: `dispatch_wake_clears_only_on_sent`,
/// `test_dispatch_wake_carries_pending_across_a_skip_then_wakes_once`,
/// `dispatch_wake_matches_python_goldens`.
pub fn dispatch_wake<C: Clock + ?Sized>(
    state: &mut PollState,
    new_count: u64,
    logged: Option<u64>,
    dry_run: bool,
    clock: &C,
    wake: impl FnOnce(u64, Option<u64>) -> Result<String, WakeError>,
) -> String {
    let pending = counter(state, "wake_pending").saturating_add(new_count);
    state
        .other
        .insert("wake_pending".to_owned(), Value::from(pending));
    if let Some(l) = logged {
        let total = counter(state, "wake_logged").saturating_add(l);
        state
            .other
            .insert("wake_logged".to_owned(), Value::from(total));
    }
    if pending == 0 {
        state.other.remove("wake_logged");
        return "none".to_owned();
    }
    let logged_total = logged.map(|_| counter(state, "wake_logged"));
    let result = wake(pending, logged_total).unwrap_or_else(|e| format!("failed: {e}"));
    // #9171 AC7: only a confirmed send clears the count.
    if result.starts_with("sent") {
        state
            .other
            .insert("wake_pending".to_owned(), Value::from(0));
        state.other.remove("wake_logged");
        if !dry_run {
            state
                .other
                .insert("wake_sent_at".to_owned(), Value::from(clock.now_unix()));
        }
    }
    result
}

#[cfg(test)]
#[path = "wake_live_tests.rs"]
mod live_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FakeClock, ScriptedTmux};

    #[test]
    fn pointer_text_stamps_utc_minutes() {
        let alerts = Path::new("/a.jsonl");
        assert_eq!(
            pointer_text(1_760_000_000.5, 3, None, alerts),
            "[poll 08:53Z] 3 new alerts -> /a.jsonl"
        );
        assert_eq!(
            pointer_text(86_399.9, 1, Some(0), alerts),
            "[poll 23:59Z] 1 alert needs you (0 logged) -> /a.jsonl"
        );
    }

    #[test]
    fn wake_dispatch_types_the_carried_total() -> Result<(), WakeError> {
        let empty = "────────────\n❯\u{a0}\n────────────\n";
        let tmux = ScriptedTmux::new()
            .fallback("capture-pane", empty)
            .fallback("display-message", "tm-architect\n")
            .fallback("send-keys", "");
        let clock = FakeClock::new(0.0);
        let waker = Waker {
            tmux: &tmux,
            clock: &clock,
            alerts: Path::new("/a"),
            dry_run: false,
        };
        let panes = [Pane {
            session: SUPERVISOR.to_owned(),
            pane: PaneId::parse("%1")?,
            cmd: String::new(),
            active: "11".to_owned(),
            path: String::new(),
            dead: String::new(),
        }];
        let mut state = PollState::default();
        state
            .other
            .insert("wake_pending".to_owned(), Value::from(2));
        // The box stays empty: the pointer is typed once, never confirmed.
        assert_eq!(waker.dispatch(&mut state, &panes, 3, None), BOX_CHANGED);
        assert_eq!(tmux.sends()[0][5], "[poll 00:00Z] 5 new alerts -> /a");
        assert_eq!(state.other.get("wake_pending"), Some(&Value::from(5)));
        Ok(())
    }
}
