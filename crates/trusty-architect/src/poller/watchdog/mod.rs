//! The Architect watchdog: `architect-watchdog.py`'s `classify` and `step`.
//!
//! Why: the poller outlives the Architect, so it is the one process that can
//! see the Architect dead, frozen or silent (#9779, Bob ruling f4). A dead
//! Architect is relaunched at most once per [`RELAUNCH_DEBOUNCE`] and given
//! up on after [`MAX_FAILURES`]; frozen and silent are alerts only, never a
//! relaunch and never keys.
//! What: [`classify`] and [`step`] are pure: panes, clock, transcript mtime
//! and the relauncher come in as arguments. The book is the prototype's
//! `state["watchdog"]` JSON object (`last_relaunch`, `failures`, `pending`,
//! `spinner`, `wait_since`), so Python and Rust read each other's state. A
//! background-agent wait is not frozen until [`WAIT_MAX_MIN`] (#11826). [`relaunch`]
//! holds the one real relaunch path (the Python `session-relaunch.sh`, run as
//! a subprocess); [`wiring`] runs one cycle's step against the poll state.
//! Test: `tests/watchdog_ported.rs` (the prototype's tests, by name).
pub mod relaunch;
pub mod wiring;

use serde_json::{Map, Value};

use super::architect::is_shell;
use super::listing::Pane;
use super::pyfmt::py_g;

/// Minutes after a wake with no transcript write before `architect_silent`.
pub const SILENT_MINUTES_DEFAULT: f64 = 45.0;
/// Minutes of transcript silence under a running turn before `architect_frozen`.
pub const FROZEN_MIN_DEFAULT: f64 = 15.0;
/// Seconds between relaunch attempts.
pub const RELAUNCH_DEBOUNCE: f64 = 1800.0;
/// Failed relaunches before the watchdog gives up.
pub const MAX_FAILURES: u64 = 2;

/// The Architect's state, as the watchdog sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// The tmux session is missing, or every pane is at a shell.
    Dead,
    /// A running turn whose transcript or spinner stopped moving.
    Frozen,
    /// A wake went unanswered past the silent limit.
    Silent,
    /// None of the above.
    Ok,
}

/// What one poll observed about the Architect.
#[derive(Debug, Clone, Copy)]
pub struct Observed<'a> {
    /// Every pane tmux listed.
    pub panes: &'a [Pane],
    /// The Architect's tmux session.
    pub session: &'a str,
    /// Epoch time of the last confirmed wake (`wake_sent_at`).
    pub wake_at: Option<f64>,
    /// mtime of the newest interactive transcript.
    pub transcript_mtime: Option<f64>,
    /// Now, epoch seconds.
    pub now: f64,
    /// The silent limit in minutes (`SILENT_MINUTES`).
    pub silent_min: f64,
    /// The frozen limit in minutes (`FROZEN_MIN`).
    pub frozen_min: f64,
    /// True while the pane shows a running turn.
    pub busy: bool,
    /// The pane's status (spinner) line.
    pub spinner: &'a str,
}

/// Whole minutes in `secs`, as Python's `int(secs // 60)`.
fn minutes(secs: f64) -> i64 {
    (secs / 60.0).floor() as i64
}

/// Minutes a background-agent wait may sit unchanged before it is frozen
/// (`WAIT_MAX_MIN`, architect-watchdog.py:39).
pub const WAIT_MAX_MIN: f64 = 60.0;

/// `WAIT_RE.search(spinner)`: `Waiting for \d+ background agents? to finish`
/// (architect-watchdog.py:40), matched by hand (the crate takes no `regex`).
pub fn is_agent_wait(spinner: &str) -> bool {
    spinner.match_indices("Waiting for ").any(|(i, m)| {
        let rest = &spinner[i + m.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let rest = &rest[digits..];
        let Some(rest) = rest.strip_prefix(" background agent") else {
            return false;
        };
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        digits > 0 && rest.starts_with(" to finish")
    })
}

/// `(class, detail)` for the Architect; `prev_spinner` is last poll's text.
///
/// Silent needs a wake (with none outstanding a quiet Architect is idle);
/// frozen needs `busy`. A background-agent wait is [`classify_waiting`]
/// with no clock: never frozen.
pub fn classify(o: &Observed<'_>, prev_spinner: Option<&str>) -> (Class, String) {
    classify_waiting(o, prev_spinner, None)
}

/// [`classify`] with the background-agent wait rule.
///
/// Why: an Architect that dispatched an agent sits idle on "Waiting for N
/// background agent(s) to finish", so its transcript goes stale and its
/// spinner repeats; the Rust port called that frozen where Python did not
/// (#11826, parity window 6, class C; architect-watchdog.py:85-88).
/// What: a busy pane whose spinner matches [`is_agent_wait`] skips both
/// frozen tests; it is frozen only when `wait_since` (the book's clock,
/// started by [`step`]) is over [`WAIT_MAX_MIN`] minutes old. Silent still
/// applies.
/// Test: `test_step_background_wait_is_not_frozen`,
/// `test_step_background_wait_alerts_after_60_min`,
/// `test_step_other_unchanged_spinner_still_frozen`.
pub fn classify_waiting(
    o: &Observed<'_>,
    prev_spinner: Option<&str>,
    wait_since: Option<f64>,
) -> (Class, String) {
    let mine: Vec<&Pane> = o.panes.iter().filter(|p| p.session == o.session).collect();
    let Some(first) = mine.first() else {
        return (
            Class::Dead,
            format!("tmux session {} is missing", o.session),
        );
    };
    if mine.iter().all(|p| is_shell(&p.cmd)) {
        return (Class::Dead, format!("pane runs {}, not claude", first.cmd));
    }
    if o.busy && is_agent_wait(o.spinner) {
        // #11826: an expected wait, frozen only past WAIT_MAX_MIN.
        if let Some(w) = wait_since
            && o.now - w > WAIT_MAX_MIN * 60.0
        {
            let head: String = o.spinner.chars().take(80).collect();
            let detail = format!(
                "waiting on background agent for {} min: {head}",
                minutes(o.now - w)
            );
            return (Class::Frozen, detail);
        }
    } else if o.busy
        && let Some(t) = o.transcript_mtime
        && o.now - t > o.frozen_min * 60.0
    {
        let detail = format!(
            "running turn, no transcript write for {} min (limit {})",
            minutes(o.now - t),
            py_g(o.frozen_min)
        );
        return (Class::Frozen, detail);
    } else if o.busy && !o.spinner.is_empty() && Some(o.spinner) == prev_spinner {
        let head: String = o.spinner.chars().take(80).collect();
        let detail = format!("running turn, spinner text unchanged across polls: {head}");
        return (Class::Frozen, detail);
    }
    // Python `if wake_at and ...`: a zero wake time is no wake.
    if let Some(wake) = o.wake_at.filter(|w| *w != 0.0)
        && o.now - wake > o.silent_min * 60.0
        && o.transcript_mtime.is_none_or(|t| t < wake)
    {
        let detail = format!(
            "no transcript write for {} min since the wake (limit {})",
            minutes(o.now - wake),
            py_g(o.silent_min)
        );
        return (Class::Silent, detail);
    }
    (Class::Ok, String::new())
}

/// What a relaunch attempt reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relaunch {
    /// The script ran and the pane left the shell.
    Ok(String),
    /// The script failed, timed out, or left the pane at a shell.
    Failed(String),
    /// A live Architect appeared first; not a failure.
    Aborted(String),
}

/// The one relaunch path, injected so the decision logic stays pure.
pub trait Relauncher {
    /// Relaunches the Architect session once.
    fn relaunch(&self) -> Relaunch;
}

/// One watchdog alert, before it becomes an [`crate::classify::Alert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// `architect_dead`, `architect_frozen`, `architect_silent` or
    /// `architect_relaunch_failed`.
    pub kind: &'static str,
    /// The dedup fingerprint.
    pub fingerprint: String,
    /// The alert text.
    pub detail: String,
}

/// What [`step`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// The alerts this poll raises.
    pub found: Vec<Found>,
    /// The `architect=...` phrase for the cycle's log line.
    pub phrase: String,
    /// True when a relaunch reported success.
    pub relaunched: bool,
}

fn found(kind: &'static str, fingerprint: &str, detail: String) -> Found {
    Found {
        kind,
        fingerprint: fingerprint.to_owned(),
        detail,
    }
}

fn result(found: Vec<Found>, phrase: String, relaunched: bool) -> StepResult {
    StepResult {
        found,
        phrase,
        relaunched,
    }
}

fn failures(book: &Map<String, Value>) -> u64 {
    book.get("failures").and_then(Value::as_u64).unwrap_or(0)
}

fn set_alive(book: &mut Map<String, Value>) {
    book.insert("failures".to_owned(), Value::from(0));
    book.insert("pending".to_owned(), Value::Bool(false));
}

/// One poll's decision. `book` is the mutable `state["watchdog"]` object.
/// `dry_run` (a dry run or a shadow run) logs `would relaunch` and never
/// calls `relauncher`.
pub fn step(
    book: &mut Map<String, Value>,
    o: &Observed<'_>,
    relauncher: &dyn Relauncher,
    dry_run: bool,
) -> StepResult {
    let prev = book
        .get("spinner")
        .and_then(Value::as_str)
        .map(str::to_owned);
    // #11826: the wait clock (architect-watchdog.py:117-121) starts on a new
    // or changed wait text and clears when the pane is not waiting.
    let wait_since = if o.busy && is_agent_wait(o.spinner) {
        let held = book.get("wait_since").and_then(Value::as_f64);
        match held {
            Some(w) if prev.as_deref() == Some(o.spinner) => Some(w),
            _ => Some(o.now),
        }
    } else {
        None
    };
    book.insert(
        "wait_since".to_owned(),
        wait_since.map_or(Value::Null, Value::from),
    );
    let (class, detail) = classify_waiting(o, prev.as_deref(), wait_since);
    let spinner = if o.busy && class != Class::Dead {
        Value::String(o.spinner.to_owned())
    } else {
        Value::Null
    };
    book.insert("spinner".to_owned(), spinner);
    match class {
        Class::Ok => {
            set_alive(book);
            return result(Vec::new(), "architect=ok".to_owned(), false);
        }
        Class::Frozen => {
            set_alive(book); // alive, so a past relaunch worked
            let phrase = format!("architect=frozen {detail}; alert only, no relaunch, no keys");
            return result(
                vec![found("architect_frozen", "frozen", detail)],
                phrase,
                false,
            );
        }
        Class::Silent => {
            let wake = o.wake_at.unwrap_or(0.0).trunc() as i64;
            let phrase = format!("architect=silent {detail}; alert only, no relaunch, no keys");
            let fp = format!("wake-{wake}");
            return result(vec![found("architect_silent", &fp, detail)], phrase, false);
        }
        Class::Dead => {}
    }
    let mut alerts = vec![found("architect_dead", "dead", detail.clone())];
    if book.get("pending").and_then(Value::as_bool) == Some(true) {
        // The last relaunch reported success, yet it is dead again.
        let n = failures(book) + 1;
        book.insert("failures".to_owned(), Value::from(n));
        book.insert("pending".to_owned(), Value::Bool(false));
    }
    let n = failures(book);
    if n >= MAX_FAILURES {
        let why = format!("{n} relaunches failed; watchdog stopped");
        alerts.push(found("architect_relaunch_failed", "gave-up", why));
        let phrase = format!("architect=dead {detail}; relaunch suspended after {n} failures");
        return result(alerts, phrase, false);
    }
    let last = book
        .get("last_relaunch")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let wait = RELAUNCH_DEBOUNCE - (o.now - last);
    if wait > 0.0 {
        let phrase = format!(
            "architect=dead {detail}; relaunch debounced {}s",
            wait as i64
        );
        return result(alerts, phrase, false);
    }
    if dry_run {
        let phrase = format!("architect=dead {detail}; dry-run: would relaunch");
        return result(alerts, phrase, false);
    }
    book.insert("last_relaunch".to_owned(), Value::from(o.now));
    match relauncher.relaunch() {
        Relaunch::Ok(why) => {
            book.insert("pending".to_owned(), Value::Bool(true));
            let phrase = format!("architect=dead {detail}; relaunched ({why})");
            result(alerts, phrase, true)
        }
        Relaunch::Aborted(why) => {
            let phrase = format!("architect=dead {detail}; relaunch aborted ({why})");
            result(alerts, phrase, false)
        }
        Relaunch::Failed(why) => {
            let n = n + 1;
            book.insert("failures".to_owned(), Value::from(n));
            if n >= MAX_FAILURES {
                let text = format!("{n} relaunches failed: {why}");
                alerts.push(found("architect_relaunch_failed", "gave-up", text));
            }
            let phrase = format!("architect=dead {detail}; relaunch FAILED ({why})");
            result(alerts, phrase, false)
        }
    }
}
