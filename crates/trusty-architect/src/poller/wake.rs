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
//! #9167: a submitted pointer is recorded once through the waker's
//! [`MessageSink`]; a sink error is a note on the phrase, never a failed wake.
//! Test: `tests/wake_ported.rs` (the prototype's tests, by name),
//! `tests/wake_send_path.rs` (AC5-7), `tests/parity_wake.rs` (goldens),
//! `tests/wake_messages.rs` (#9167).

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Map;

use super::SUPERVISOR;
use super::box_text::{box_full_text, own_pointer};
use super::digest::{WakeItem, bound_wake_alerts, cap_wake, held_since, read_items, wake_digest};
use super::listing::{Pane, capture};
use super::messages::{MessageSink, PaneMessage, PaneMessageKind};
use super::state::{PollState, StoredCount};
use crate::classify::alerts::iso;
use crate::classify::{input_state_for_send, parse_pane};
use crate::clock::Clock;
use crate::error::WakeError;
use crate::input::classify_input;
use crate::pointer::{Confirmation, Erased, type_pointer, verify_owner};
use crate::pyjson::to_python_json;
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
/// names, whether this is a dry run, and where a submitted wake is recorded.
pub struct Waker<'a, T: Tmux + ?Sized, C: Clock + ?Sized> {
    /// The tmux server holding the Architect's pane.
    pub tmux: &'a T,
    /// The clock for the pointer's stamp and the waits.
    pub clock: &'a C,
    /// The alerts file the pointer points at.
    pub alerts: &'a Path,
    /// When true, report what would be sent and send nothing.
    pub dry_run: bool,
    /// Records each submitted pointer (#9167).
    pub messages: &'a dyn MessageSink,
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
    /// (`failed: ...`), or sends nothing more ([`BOX_CHANGED`]). #9167: once
    /// Enter is sent, the pointer is recorded through `messages`; only then,
    /// so a skipped, unconfirmed or erased pointer is never recorded.
    /// Test: `wake_refuses_table`, `wake_enter_only_when_box_equals_message`,
    /// `wake_rechecks_owner_before_every_send`, `wake_matches_python_goldens`,
    /// `a_submitted_wake_is_recorded_once_with_the_typed_text_9167`.
    ///
    /// # Errors
    /// [`WakeError::NotOwner`] when the pane's live session is not
    /// `tm-architect`; [`WakeError::Tmux`] when a tmux call fails. The
    /// prototype raises for both; [`dispatch_wake`] reports them as `failed`.
    pub fn wake(&self, panes: &[Pane], n: u64, logged: Option<u64>) -> Result<String, WakeError> {
        self.wake_as(SUPERVISOR, panes, n, logged, "")
    }

    /// [`Waker::wake`] for the session `owner`. Production passes the
    /// configured supervisor ([`SUPERVISOR`] unless `ARCHITECT_SESSION` names
    /// another, #9822); the scratch-server live test passes its own session.
    fn wake_as(
        &self,
        owner: &str,
        panes: &[Pane],
        n: u64,
        logged: Option<u64>,
        digest: &str,
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
        // df-T7i P1b: the digest rides the same line, capped (cap_wake).
        let msg = cap_wake(
            &pointer_text(self.clock.now_unix(), n, logged, self.alerts),
            digest,
        );
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
                // #9167: the pointer is delivered; record it, never undo it.
                let note = self.record(owner, target, &msg);
                self.clock.sleep(AFTER_ENTER);
                let (_, left) = classify_input(&capture(self.tmux, target)?);
                let head: String = msg.chars().take(40).collect();
                let still = if left.contains(&head) {
                    " (still in box after Enter)"
                } else {
                    ""
                };
                Ok(format!("sent to {target}{still}{note}"))
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

    /// Records the submitted pointer `text`; returns the phrase's suffix.
    ///
    /// Why: #9167 — the sink belongs to the caller and may fail; the wake it
    /// describes already happened, so a failure must neither undo it nor be
    /// lost. `poll.log` is the poller's log, so the reason rides the phrase.
    /// What: one [`MessageSink::record`] call; `""` when it succeeds, else
    /// `" (message event not recorded: <reason>)"`, which keeps the phrase's
    /// `sent` prefix so [`dispatch_wake`] still clears the count.
    /// Test: `a_sink_error_never_stops_the_wake_and_is_logged_9167`.
    fn record(&self, owner: &str, target: &PaneId, text: &str) -> String {
        let message = PaneMessage {
            kind: PaneMessageKind::PollWake,
            session: owner,
            pane: target.as_target(),
            text,
        };
        match self.messages.record(&message) {
            Ok(()) => String::new(),
            Err(e) => {
                tracing::warn!(error = %e, pane = %target, "wake message event not recorded");
                format!(" (message event not recorded: {e})")
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
        let ctx = WakeContext::default();
        self.dispatch_as(SUPERVISOR, state, panes, new_count, logged, &ctx)
    }

    /// [`Waker::dispatch`] waking the session `owner` (the config's
    /// supervisor, #9822).
    ///
    /// Test: `a_configured_supervisor_is_skipped_and_the_only_pane_woken`.
    pub fn dispatch_as(
        &self,
        owner: &str,
        state: &mut PollState,
        panes: &[Pane],
        new_count: u64,
        logged: Option<u64>,
        ctx: &WakeContext<'_>,
    ) -> String {
        dispatch_wake_with(
            state,
            new_count,
            logged,
            self.dry_run,
            self.clock,
            ctx,
            |n, l, digest| self.wake_as(owner, panes, n, l, digest),
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

/// A stored count: 0 when absent, `unreadable` when not a count.
fn counter_or(stored: Option<&StoredCount>, key: &str, unreadable: u64) -> u64 {
    stored.map_or(0, |c| c.or_unreadable(key, unreadable))
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
/// `failed: <message>`. A stored `wake_pending` that is not a non-negative
/// integer counts as 1. Only a result starting `sent` zeroes `wake_pending`
/// and drops `wake_logged`, and outside a dry run stamps `wake_sent_at` with
/// `clock.now_unix()`. The keys are [`PollState`]'s typed `wake_*` fields
/// (#9171 A3b-2).
/// Test: `dispatch_wake_clears_only_on_sent`,
/// `dispatch_wake_unreadable_pending_reads_as_owed`,
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
    let ctx = WakeContext::default();
    dispatch_wake_with(state, new_count, logged, dry_run, clock, &ctx, |n, l, _| {
        wake(n, l)
    })
}

/// What a wake reads and writes besides the pane (df-T7i P1b). A `None`
/// path turns its step off.
#[derive(Debug, Default, Clone, Copy)]
pub struct WakeContext<'a> {
    /// The alerts woken this cycle, carried until a wake is sent.
    pub woken: &'a [WakeItem],
    /// `pm-messages.jsonl`: held PM messages join the digest.
    pub messages: Option<&'a Path>,
    /// `alerts.jsonl`, named by the digest's `+N more -> path`.
    pub alerts: Option<&'a Path>,
    /// The self-clear marker: while it is fresh, no wake is typed (ar-T0c).
    pub suppress: Option<&'a Path>,
    /// `pm-wake-alert.json`: failed wakes counted, a sent one clears it (D39).
    pub wake_alert: Option<&'a Path>,
}

/// Seconds after which a self-clear marker is stale (self-clear was killed
/// without its trap) and ignored.
pub const WAKE_SUPPRESS_MAX_S: f64 = 3.0 * 3600.0;

/// True while `marker` exists and is younger than [`WAKE_SUPPRESS_MAX_S`].
fn wake_suppressed(marker: &Path, now: f64) -> bool {
    let Ok(modified) = fs::metadata(marker).and_then(|m| m.modified()) else {
        return false;
    };
    let mtime = modified
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    now - mtime < WAKE_SUPPRESS_MAX_S
}

/// `build_wake_digest`: carries `woken` into `wake_alerts`, bounds the list,
/// and builds the digest of it plus the held PM messages since the last
/// confirmed wake.
fn build_wake_digest(state: &mut PollState, ctx: &WakeContext<'_>, now: f64) -> String {
    let mut pend = read_items(&state.other);
    pend.extend(ctx.woken.iter().cloned());
    let pend = bound_wake_alerts(&mut state.other, pend);
    let cutoff = state
        .other
        .get("digest_cursor")
        .and_then(serde_json::Value::as_f64)
        .filter(|c| *c != 0.0)
        .or(state.wake_sent_at.filter(|c| *c != 0.0))
        .unwrap_or(now - 1800.0);
    let msgs = ctx
        .messages
        .map(|p| held_since(p, cutoff))
        .unwrap_or_default();
    wake_digest(&pend, &msgs, ctx.alerts.unwrap_or(Path::new("")))
}

/// `pm_cursor.record_wake_result`: a `failed` phrase counts an attempt in
/// `path`; a `sent` one removes the file; anything else changes nothing.
///
/// # Errors
/// The file cannot be written or removed (a missing file is not an error).
pub fn record_wake_result(path: &Path, result: &str, now: f64) -> io::Result<()> {
    if result.starts_with("failed") {
        let mut st: Map<String, serde_json::Value> = fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        st.entry("first_ts").or_insert_with(|| iso(now).into());
        let attempts = st
            .get("attempts")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        st.insert("attempts".to_owned(), (attempts + 1).into());
        st.insert("last_ts".to_owned(), iso(now).into());
        let text = to_python_json(&st, true).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?;
    } else if result.starts_with("sent") {
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// [`dispatch_wake`] with the digest, the self-clear marker and the
/// wake-alert record (df-T7i P1b): `wake` gets the digest as its third
/// argument.
///
/// Why: the prototype's live `dispatch_wake`; a wake that cannot be typed
/// must keep its alerts and reach the Architect's hook (D39).
/// What: as [`dispatch_wake`], plus: a fresh `ctx.suppress` marker returns
/// `suppressed: self-clear waiting for idle` with everything still pending;
/// the woken alerts are carried in `wake_alerts` and the digest is built
/// from them and the held messages; outside a dry run the result is
/// recorded in `ctx.wake_alert` (an error is logged, never fatal); a `sent`
/// result drops `wake_alerts` and `wake_alerts_dropped` and, outside a dry
/// run, stamps `digest_cursor`.
/// Test: `dispatch_wake_matches_python_goldens`,
/// `test_dispatch_wake_suppressed_while_self_clear_marker_fresh`,
/// `a_failed_wake_alert_record_never_changes_the_wake`.
pub fn dispatch_wake_with<C: Clock + ?Sized>(
    state: &mut PollState,
    new_count: u64,
    logged: Option<u64>,
    dry_run: bool,
    clock: &C,
    ctx: &WakeContext<'_>,
    wake: impl FnOnce(u64, Option<u64>, &str) -> Result<String, WakeError>,
) -> String {
    // #9171 AC7: an unreadable stored count is a wake still owed, never 0.
    let pending =
        counter_or(state.wake_pending.as_ref(), "wake_pending", 1).saturating_add(new_count);
    state.wake_pending = Some(StoredCount::Count(pending));
    let mut logged_total = None;
    if let Some(l) = logged {
        let total = counter_or(state.wake_logged.as_ref(), "wake_logged", 0).saturating_add(l);
        state.wake_logged = Some(StoredCount::Count(total));
        logged_total = Some(total);
    }
    if pending == 0 {
        state.wake_logged = None;
        return "none".to_owned();
    }
    let now = clock.now_unix();
    // ar-T0c: alerts stay pending; the first wake after the marker carries them.
    if ctx.suppress.is_some_and(|m| wake_suppressed(m, now)) {
        return "suppressed: self-clear waiting for idle".to_owned();
    }
    let digest = build_wake_digest(state, ctx, now);
    let result = wake(pending, logged_total, &digest).unwrap_or_else(|e| format!("failed: {e}"));
    if let Some(path) = ctx.wake_alert.filter(|_| !dry_run) {
        // D39: a failing wake must reach the Architect's hook, not only poll.log.
        if let Err(e) = record_wake_result(path, &result, now) {
            tracing::warn!(error = %e, path = %path.display(), "wake-alert record failed");
        }
    }
    // #9171 AC7: only a confirmed send clears the count.
    if result.starts_with("sent") {
        state.wake_pending = Some(StoredCount::Count(0));
        state.wake_logged = None;
        state.other.remove("wake_alerts");
        state.other.remove("wake_alerts_dropped");
        if !dry_run {
            state.other.insert("digest_cursor".to_owned(), now.into());
            state.wake_sent_at = Some(clock.now_unix());
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
    use crate::poller::NoMessages;
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
            messages: &NoMessages,
        };
        let panes = [Pane {
            session: SUPERVISOR.to_owned(),
            pane: PaneId::parse("%1")?,
            cmd: String::new(),
            active: "11".to_owned(),
            path: String::new(),
            dead: String::new(),
        }];
        let mut state = PollState {
            wake_pending: Some(StoredCount::Count(2)),
            ..PollState::default()
        };
        // The box stays empty: the pointer is typed once, never confirmed.
        assert_eq!(waker.dispatch(&mut state, &panes, 3, None), BOX_CHANGED);
        assert_eq!(tmux.sends()[0][5], "[poll 00:00Z] 5 new alerts -> /a");
        assert_eq!(state.wake_pending, Some(StoredCount::Count(5)));
        Ok(())
    }
}
