//! Alert evaluation: `fleet-poll.py`'s `alert`, `evaluate` and
//! `fleet_plan_alerts`, plus the `clean`/`fp`/`iso` helpers they use.
//!
//! Why: an alert's fingerprint and detail are dedup keys and owner-facing
//! text, so both must match the prototype byte for byte (#9171 A2).
//! What: pure functions over [`PaneInfo`]; `now` (seconds since the epoch) is
//! always passed in, never read from the clock.
//! Test: `test_no_agents_still_idle`, `idle_prompt_alerts_after_ten_minutes_at_live_bar`,
//! `ctx_crossing_22_and_25_at_live_bar`, `test_ctx_thresholds_follow_ruling_b7`,
//! `test_alert_display_and_redaction`, `evaluate_matches_python_goldens`.

use serde::{Deserialize, Serialize};

use super::PaneInfo;
use super::matchers::boundary_word_run;
use crate::input::InputState;
use crate::pystr::{collapse_space, contains_ci, is_digit, is_space};
use crate::sha1::sha1_hex;

/// Seconds an idle pane waits before it alerts.
pub const IDLE_AFTER: f64 = 600.0;
// #9171: fleet-poll.py's live bar (ruling b7, 2026-10-07), not fleet-classify.py's 45/50.
/// Context percent that raises a `ctx` alert.
pub const CTX_WARN: u64 = 22;
/// Context percent whose `ctx` alert carries the higher fingerprint.
pub const CTX_HIGH: u64 = 25;
/// Plan usage percent that raises the fleet-wide `plan-limit` alert.
pub const PLAN_HIGH: u64 = 90;
/// Characters an alert detail keeps.
pub const DETAIL_MAX: usize = 200;

/// One alert, in the key order `alerts.jsonl` rows carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Alert {
    /// UTC `YYYY-MM-DDTHH:MM:SSZ`.
    pub ts: String,
    /// tmux session name, or `""` for a fleet-wide alert.
    pub session: String,
    /// tmux pane id, or `""`.
    pub pane: String,
    /// `dialog`, `question`, `idle`, `ctx`, `plan-limit`, `error`, ...
    pub kind: String,
    /// Stable identity within `(session, pane, kind)` for dedup.
    pub fingerprint: String,
    /// One redacted line, at most [`DETAIL_MAX`] characters.
    pub detail: String,
    /// `[session]: kind: detail` (no prefix when `session` is empty).
    pub display: String,
}

/// One pane's state across cycles (`fleet-poll.py`'s `ps` dict).
// #9171 A3a: Deserialize so poll-state.json's `panes` map reads back typed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    /// When the pane was first seen idle in the current idle stretch.
    #[serde(default)]
    pub idle_since: Option<f64>,
    /// Error lines seen last cycle; `None` before the first sight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
}

/// Short stable fingerprint: the first 12 hex digits of SHA-1.
pub fn fingerprint(text: &str) -> String {
    let mut hex = sha1_hex(text.as_bytes());
    hex.truncate(12);
    hex
}

/// `SECRET.sub(r"\1=<redacted>", text)`, case-insensitive:
/// `\b(\w*(?:key|token|secret|passw(?:or)?d|auth)\w*)\s*[=:]\s*\S+`.
fn redact_secrets(s: &[char]) -> Vec<char> {
    const WORDS: [&str; 6] = ["key", "token", "secret", "passwd", "password", "auth"];
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some(end) = boundary_word_run(s, i) {
            let word = &s[i..end];
            let mut j = end + s[end..].iter().take_while(|c| is_space(**c)).count();
            if WORDS.iter().any(|w| contains_ci(word, w)) && matches!(s.get(j), Some('=' | ':')) {
                j += 1;
                j += s[j..].iter().take_while(|c| is_space(**c)).count();
                let value = s[j..].iter().take_while(|c| !is_space(**c)).count();
                if value > 0 {
                    out.extend_from_slice(word);
                    out.extend("=<redacted>".chars());
                    i = j + value;
                    continue;
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `LONG_TOKEN.sub("<redacted>", text)`: `\b(?=\w*\d)\w{32,}\b` — a word of
/// 32+ characters holding a digit (API keys, hex digests).
fn redact_long_tokens(s: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some(end) = boundary_word_run(s, i)
            && end - i >= 32
            && s[i..end].iter().any(|c| is_digit(*c))
        {
            out.extend("<redacted>".chars());
            i = end;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// One-line detail: whitespace collapsed, secret-looking values redacted,
/// capped at [`DETAIL_MAX`] characters.
pub(crate) fn clean(text: &str) -> String {
    let collapsed: Vec<char> = collapse_space(text).chars().collect();
    let redacted = redact_long_tokens(&redact_secrets(&collapsed));
    redacted.into_iter().take(DETAIL_MAX).collect()
}

/// `datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")`.
pub(crate) fn iso(t: f64) -> String {
    let mut secs = t.trunc();
    let micros = ((t - secs) * 1e6).round_ties_even();
    if micros >= 1e6 {
        secs += 1.0;
    } else if micros < 0.0 {
        secs -= 1.0;
    }
    // Truncation to i64 is exact here: `secs` is integral and in date range.
    let secs = secs as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Builds one alert. `detail` is cleaned; `display` prefixes the session.
pub fn alert(
    kind: &str,
    fingerprint: &str,
    detail: &str,
    session: &str,
    pane: &str,
    now: f64,
) -> Alert {
    let detail = clean(detail);
    let prefix = if session.is_empty() {
        String::new()
    } else {
        format!("[{session}]: ")
    };
    Alert {
        ts: iso(now),
        session: session.to_owned(),
        pane: pane.to_owned(),
        kind: kind.to_owned(),
        fingerprint: fingerprint.to_owned(),
        display: format!("{prefix}{kind}: {detail}"),
        detail,
    }
}

/// Alerts for one parsed pane; updates `ps` (idle clock, error baseline).
///
/// Why: one place decides which facts need the Architect.
/// What: `dialog`, `question`, `idle` (after [`IDLE_AFTER`] s with an empty or
/// suggestion box, no dialog, not busy), `ctx` (at [`CTX_WARN`]%, fingerprint
/// `22`/`25`), `plan-limit`, and `error` for error lines new since last cycle
/// (the first sight only seeds the baseline).
/// Test: `idle_prompt_alerts_after_ten_minutes_at_live_bar`,
/// `error_alerts_only_for_lines_new_since_last_cycle_at_live_bar`,
/// `evaluate_matches_python_goldens`.
pub fn evaluate(
    info: &PaneInfo,
    ps: &mut PaneState,
    now: f64,
    session: &str,
    pane: &str,
) -> Vec<Alert> {
    let mut out = Vec::new();
    let mut add =
        |kind: &str, f: &str, detail: &str| out.push(alert(kind, f, detail, session, pane, now));
    if let Some((norm, detail)) = &info.dialog {
        add("dialog", &fingerprint(norm), detail);
    }
    if let Some(q) = &info.question {
        add("question", &fingerprint(q), q);
    }
    let idle_box = matches!(
        info.box_reading,
        super::BoxReading::Input(InputState::Empty | InputState::Suggestion)
    );
    if info.has_box && idle_box && info.dialog.is_none() && !info.busy {
        let since = *ps.idle_since.get_or_insert(now);
        if now - since >= IDLE_AFTER {
            // Python's int() truncates toward zero; `//` floors.
            let mins = ((now - since) / 60.0).floor() as i64;
            add(
                "idle",
                &format!("since-{}", since.trunc() as i64),
                &format!("idle {mins} min, no agents running"),
            );
        }
    } else {
        ps.idle_since = None;
    }
    if let Some(ctx) = info.ctx.filter(|c| *c >= CTX_WARN) {
        let level = if ctx >= CTX_HIGH { CTX_HIGH } else { CTX_WARN };
        add("ctx", &level.to_string(), &format!("ctx {ctx}%"));
    }
    if let Some(limit) = &info.limit {
        add("plan-limit", &fingerprint(limit), limit);
    }
    let prev = ps.errors.replace(info.errors.clone());
    if let Some(prev) = prev {
        // First sight of a pane seeds the set; no alert flood at start.
        let new: Vec<&String> = info.errors.iter().filter(|e| !prev.contains(e)).collect();
        if let Some(first) = new.first() {
            let joined = new
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            add(
                "error",
                &fingerprint(&joined),
                &format!("{} new: {first}", new.len()),
            );
        }
    }
    out
}

/// Plan usage is account-wide: one alert for the fleet, not one per pane.
///
/// Test: `plan_limit_text_and_status_line_at_live_bar`, `plan_alerts_match_python_goldens`.
pub fn fleet_plan_alerts(infos: &[(String, PaneInfo)], now: f64) -> Vec<Alert> {
    type PlanField = (&'static str, &'static str, fn(&PaneInfo) -> Option<u64>);
    let keys: [PlanField; 2] = [
        ("plan5h", "5-hour", |i| i.plan5h),
        ("planwk", "weekly", |i| i.planwk),
    ];
    keys.into_iter()
        .filter_map(|(key, label, get)| {
            let (v, s) = infos
                .iter()
                .filter_map(|(s, i)| get(i).map(|v| (v, s)))
                .max()?;
            (v >= PLAN_HIGH).then(|| {
                let detail = format!("{label} plan usage {v}% (status line of {s})");
                alert(
                    "plan-limit",
                    &format!("{key}>={PLAN_HIGH}"),
                    &detail,
                    "",
                    "",
                    now,
                )
            })
        })
        .collect()
}
