//! The deterministic half of `alert_triage.py`: its `decide` rules, as the
//! poller's [`Triage`].
//!
//! Why: the Rust poller woke for every new alert ([`super::WakeAll`]), so a
//! `ctx` alert below the refresh bar woke the shadow while the live poller
//! logged it (`rule:ctx_below_bar`, MB1595).
//! What: [`decide`] is `alert_triage.decide`: always-wake kinds, the `ctx`
//! bar and task-complete rules, `token` `ctx_main`, the `rc_*` run counter
//! (`state["triage_rc"]`) and the hold file `local/triage-rules.json`.
//! [`RuleTriage`] applies it; an alert no rule decides goes to the Haiku gate
//! in the prototype, which this port does not run: it wakes, the prototype's
//! own fail-open answer and what its `--dry-run` does. The records the
//! prototype appends to `triage.jsonl` are not written.
//! Test: `ctx_below_the_bar_is_logged_and_at_the_bar_wakes`,
//! `held_idle_is_logged_and_unruled_idle_wakes`, `rc_failed_wakes_on_the_third_in_a_row`
//! (tests/triage_rules.rs).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use super::ctx_bar::{PM_BAR, architect_bar, bar_pct};
use super::state::PollState;
use super::triage::Triage;
use crate::classify::Alert;
use crate::pystr::{is_digit, parse_digits};
use crate::sched::slot::parse_iso;

/// Kinds that always wake (`WAKE_KINDS`), besides `owner_*` and `architect_*`.
pub const WAKE_KINDS: [&str; 18] = [
    "permission_prompt",
    "dialog",
    "question",
    "exited",
    "session_exited",
    "self_ctx",
    "band",
    "status_request",
    "user_state",
    "effort",
    "pm_message",
    "il_cron",
    "reminder",
    "ctxwatch",
    "ctxcycle",
    "host_zone",
    "morning_brief",
    "dream",
];
/// `token` sub-kinds that only feed the optimizer (`TOKEN_LOG`).
const TOKEN_LOG: [&str; 4] = ["hit_low", "miss_idle", "miss_unexpected", "ctx_sub"];
/// `rc_failed` alerts in a row on one session before one wakes.
const RC_FAIL_WAKE: u64 = 3;
/// The context window `ctx_bar.bar_pct` divides by.
const WINDOW: i64 = 1_000_000;

/// One triage result: `wake` or `log`, the rule path and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// True to wake the Architect.
    pub wake: bool,
    /// `rule:<name>`, or `haiku` for the fail-open default.
    pub path: String,
    /// One line.
    pub reason: String,
}

fn res(wake: bool, name: &str, reason: String) -> Decision {
    Decision {
        wake,
        path: format!("rule:{name}"),
        reason,
    }
}

/// `alert_triage.decide`'s inputs other than the alert.
pub struct Rules<'a> {
    /// `holds` from the rules file.
    pub holds: &'a [Value],
    /// `local/il-schedule.json`, for the Architect's own bar.
    pub schedule: &'a std::path::Path,
}

fn find_hold<'a>(holds: &'a [Value], session: &str) -> Option<&'a Map<String, Value>> {
    holds.iter().filter_map(Value::as_object).find(|h| {
        let name = h.get("session").and_then(Value::as_str).unwrap_or_default();
        !name.is_empty() && (session == name || session == format!("tm-{name}"))
    })
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn hold_active(h: &Map<String, Value>, now: f64) -> bool {
    if !truthy(h.get("until")) {
        return truthy(h.get("until_event"));
    }
    // An unparseable `until` must not silence anything.
    h.get("until")
        .and_then(Value::as_str)
        .and_then(parse_iso)
        .is_some_and(|t| now < t as f64)
}

fn hold_covers(h: &Map<String, Value>, kind: &str) -> bool {
    match h.get("kinds").and_then(Value::as_array) {
        Some(kinds) => kinds.iter().any(|k| k.as_str() == Some(kind)),
        None => kind == "idle",
    }
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "None".to_owned(),
        Some(other) => other.to_string(),
    }
}

/// The hold that applies to `kind` on `session` now, if any.
fn active_hold<'a>(
    r: &'a Rules<'_>,
    session: &str,
    kind: &str,
    now: f64,
) -> Option<&'a Map<String, Value>> {
    find_hold(r.holds, session).filter(|h| hold_active(h, now) && hold_covers(h, kind))
}

/// `re.search(r"ctx (\d+)" + suffix, detail)`'s number.
fn ctx_number(detail: &str, percent: bool) -> Option<u64> {
    let c: Vec<char> = detail.chars().collect();
    let key: Vec<char> = "ctx ".chars().collect();
    (0..c.len()).find_map(|i| {
        if c.get(i..i + 4) != Some(&key[..]) {
            return None;
        }
        let start = i + 4;
        let end = (start..=c.len())
            .find(|j| c.get(*j).is_none_or(|x| !is_digit(*x)))
            .unwrap_or(c.len());
        if end == start || (percent && c.get(end) != Some(&'%')) {
            return None;
        }
        parse_digits(&c[start..end])
    })
}

fn hold_reason(h: &Map<String, Value>, until: bool) -> String {
    let reason = h.get("reason").and_then(Value::as_str).unwrap_or_default();
    if !until {
        return format!("hold: {reason}");
    }
    let u = if truthy(h.get("until")) {
        h.get("until")
    } else {
        h.get("until_event")
    };
    format!("hold: {reason} (until {})", text(u))
}

/// `alert_triage.decide(alert, rules, now, rc_state)`; `None` is the
/// prototype's `ask_haiku`.
pub fn decide(a: &Alert, r: &Rules<'_>, now: f64, rc: &mut Map<String, Value>) -> Option<Decision> {
    let (kind, sess, detail) = (a.kind.as_str(), a.session.as_str(), a.detail.as_str());
    if kind.starts_with("owner_") || kind.starts_with("architect_") || WAKE_KINDS.contains(&kind) {
        return Some(res(
            true,
            "always_wake",
            format!("{kind} needs a decision or action"),
        ));
    }
    if kind == "ctx" {
        if a.fingerprint == "complete" {
            return Some(match active_hold(r, sess, "ctx", now) {
                Some(h) => res(false, "ctx_hold", hold_reason(h, false)),
                None => res(
                    true,
                    "ctx_complete",
                    "task complete: refresh from the IL checkpoint".to_owned(),
                ),
            });
        }
        let bar = bar_pct(sess, WINDOW, r.schedule).unwrap_or(35) as u64;
        let pct = ctx_number(detail, true).unwrap_or(bar);
        if pct >= bar {
            return Some(match active_hold(r, sess, "ctx", now) {
                Some(h) => res(false, "ctx_hold", hold_reason(h, true)),
                None => res(
                    true,
                    "ctx_at_bar",
                    format!("ctx {pct}% at or above the {bar}% refresh bar"),
                ),
            });
        }
        return Some(res(
            false,
            "ctx_below_bar",
            format!("ctx {pct}% below the {bar}% bar"),
        ));
    }
    if kind == "token" {
        let sub = a.fingerprint.split(':').next().unwrap_or_default();
        if sub == "ctx_main" {
            let level = if sess == super::ctx_bar::ARCHITECT_SESSION {
                architect_bar(r.schedule).unwrap_or(PM_BAR)
            } else {
                PM_BAR
            } as u64;
            let tokens = ctx_number(detail, false).unwrap_or(level);
            return Some(if tokens >= level {
                let why = format!("main ctx {tokens} at or above {level}");
                res(true, "token_ctx_main_at_bar", why)
            } else {
                res(
                    false,
                    "token_ctx_main_below_bar",
                    format!("main ctx {tokens} below {level}"),
                )
            });
        }
        if TOKEN_LOG.contains(&sub) {
            let why = format!("token {sub} feeds the daily optimizer");
            return Some(res(false, "token_info", why));
        }
    }
    if kind == "rc_restored" {
        rc.remove(sess);
        return Some(res(
            false,
            "rc_restored",
            "Remote Control restored".to_owned(),
        ));
    }
    if kind == "rc_failed" {
        let n = rc.get(sess).and_then(Value::as_u64).unwrap_or(0) + 1;
        rc.insert(sess.to_owned(), Value::from(n));
        if n >= RC_FAIL_WAKE {
            let why = format!("rc_failed {n} times in a row");
            return Some(res(true, "rc_failed_repeat", why));
        }
    }
    if kind == "idle"
        && let Some(h) = active_hold(r, sess, "idle", now)
    {
        return Some(res(false, "idle_hold", hold_reason(h, true)));
    }
    None
}

/// `load_rules(path)["holds"]`: a missing or bad file is no holds.
fn load_holds(path: &std::path::Path) -> Vec<Value> {
    let doc: Option<Value> = fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    match doc {
        Some(Value::Object(m)) => match m.get("holds") {
            None => Vec::new(),
            Some(Value::Array(h)) => h.clone(),
            Some(_) => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The poller's triage: [`decide`], with the Haiku gate failing open.
#[derive(Debug, Default)]
pub struct RuleTriage {
    /// `TRIAGE_RULES`, else `<root>/local/triage-rules.json`.
    pub rules: PathBuf,
    /// `IL_SCHEDULE`, else `<root>/local/il-schedule.json`.
    pub schedule: PathBuf,
    /// Woken alerts of the last call that no rule decided.
    pub haiku: AtomicU64,
}

impl Triage for RuleTriage {
    fn triage(&self, new: &[Alert], now: f64, state: &mut PollState, _: bool) -> (Vec<Alert>, u64) {
        self.haiku.store(0, Ordering::Relaxed);
        if new.is_empty() {
            return (Vec::new(), 0);
        }
        let holds = load_holds(&self.rules);
        let rules = Rules {
            holds: &holds,
            schedule: &self.schedule,
        };
        let mut rc = state
            .other
            .get("triage_rc")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut haiku = 0;
        let mut woken = Vec::new();
        for a in new {
            match decide(a, &rules, now, &mut rc) {
                Some(d) if !d.wake => {}
                Some(_) => woken.push(a.clone()),
                None => {
                    haiku += 1;
                    woken.push(a.clone());
                }
            }
        }
        self.haiku.store(haiku, Ordering::Relaxed);
        state
            .other
            .insert("triage_rc".to_owned(), Value::Object(rc));
        let logged = (new.len() - woken.len()) as u64;
        (woken, logged)
    }

    fn haiku_woken(&self) -> u64 {
        self.haiku.load(Ordering::Relaxed)
    }
}
