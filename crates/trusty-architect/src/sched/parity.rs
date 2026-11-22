//! The parity report: shadow records against the live poller's output.
//!
//! Why: the Architect decides the cutover from numbers. The report answers
//! two questions over a time range: would the scheduler have raised the same
//! alerts, and would it have woken the Architect for the same events?
//! What: [`compute_full`] turns each ok [`ShadowRecord`] and the live rows
//! into events and pairs them with [`super::parity_join::join`]: by event
//! identity (session, kind, fingerprint) first, then by session and kind
//! within two cycles of the record (MB1595 fix round). Alerts: the record's
//! alerts against `inbox/alerts.jsonl`. Wakes: the record's non-Haiku
//! `woken_keys` (or, for a record written before them, one event when its
//! own triage woke by a rule: `woken` above `haiku_woken`) against the live
//! rule wakes of `inbox/triage.jsonl`, whose `id` is `alert_triage.alert_id`
//! of an `alerts.jsonl` row and so gives the fingerprint; with no triage
//! record, the `poll.log` cycle lines with `woken=N`, N > 0 (or `wake=sent`).
//! The wake phrase is not the decision: a dry run never sends, so its carried
//! `wake_pending` makes every later cycle print `would send`.
//! Haiku-path wakes count only in `haiku_excluded`; `il_cron` alerts and
//! wakes only in `il_cron_excluded` (unmeasurable in a dry run). The report
//! keeps both numbers, identity-only and identity plus fallback, and the
//! pairs each method made; [`Report::passes`] uses identity plus fallback
//! against [`Thresholds`] (default 120 cycles and 95% for alerts and wakes).
//! [`load`] reads the files from a [`Layout`] since a time; [`render`]
//! prints it. Rows with no readable timestamp are counted in `unread`.
//! Test: `parity_joins_matched_scheduler_only_and_poller_only`,
//! `parity_counts_wakes_per_window`, `parity_ignores_failed_shadow_records`,
//! `parity_counts_only_cycles_whose_triage_woke`,
//! `parity_render_states_percentages_and_a_verdict`,
//! `parity_load_reads_the_three_files`, `parity_reports_identity_and_fallback_numbers`,
//! `parity_excludes_il_cron_with_its_reason`.

use std::collections::HashMap;

use serde_json::Value;

use super::cutover::Thresholds;
use super::parity_join::{Event, IL_CRON, Joined, join};
use super::shadow::{EventKey, ShadowAlert, ShadowRecord};
use super::slot::parse_iso;
use super::{Layout, SchedError};
use crate::sha1::sha1_hex;

/// Matched / scheduler-only / poller-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// In both.
    pub matched: u64,
    /// Only the scheduler's shadow.
    pub scheduler_only: u64,
    /// Only the live poller.
    pub poller_only: u64,
}

/// The report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Shadow records joined.
    pub cycles: u64,
    /// Shadow records that failed and were left out.
    pub failed_cycles: u64,
    /// Alert counts, identity plus fallback: the verdict's numbers.
    pub alerts: Counts,
    /// Wake counts, identity plus fallback: the verdict's numbers.
    pub wakes: Counts,
    /// Alert counts by identity alone.
    pub alerts_identity: Counts,
    /// Wake counts by identity alone.
    pub wakes_identity: Counts,
    /// Alert pairs made by identity and by the fallback.
    pub alert_pairs: (u64, u64),
    /// Wake pairs made by identity and by the fallback.
    pub wake_pairs: (u64, u64),
    /// Live rows without a readable timestamp.
    pub unread: u64,
    /// Wakes no triage rule decided (the Haiku path), on either side, left
    /// out of the wake counts and the verdict (MB1595).
    pub haiku_excluded: u64,
    /// `il_cron` alerts and wakes on either side, left out (MB1595 fix round).
    pub il_cron_excluded: u64,
}

/// One live `alerts.jsonl` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAlert {
    /// Unix seconds.
    pub ts: i64,
    /// Session and kind.
    pub key: ShadowAlert,
    /// The row's fingerprint, when it has one.
    pub fingerprint: Option<String>,
}

/// One live rule wake: its time and, when known, the alert it woke for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWake {
    /// Unix seconds.
    pub ts: i64,
    /// Session, kind and fingerprint, when the alert row is known.
    pub key: Option<EventKey>,
    /// Session and kind, when the wake row names them.
    pub sk: Option<(String, String)>,
}

/// Joins `records` to the live rows; `live_wakes` are the times of live
/// rule-decided wakes.
pub fn compute(records: &[ShadowRecord], live: &[LiveAlert], live_wakes: &[i64]) -> Report {
    compute_with(records, live, live_wakes, &[])
}

/// [`compute`], with `live_haiku` the times of live Haiku-path wakes.
pub fn compute_with(
    records: &[ShadowRecord],
    live: &[LiveAlert],
    live_wakes: &[i64],
    live_haiku: &[i64],
) -> Report {
    let wakes: Vec<LiveWake> = live_wakes
        .iter()
        .map(|&ts| LiveWake {
            ts,
            key: None,
            sk: None,
        })
        .collect();
    compute_full(records, live, &wakes, live_haiku)
}

fn key_event(ts: i64, interval: i64, key: EventKey) -> Event {
    Event::keyed(ts, interval, key)
}

/// The report from records, live alerts, live rule wakes and live Haiku
/// wake times; see the module doc.
pub fn compute_full(
    records: &[ShadowRecord],
    live: &[LiveAlert],
    live_wakes: &[LiveWake],
    live_haiku: &[i64],
) -> Report {
    let mut rep = Report::default();
    let ok: Vec<&ShadowRecord> = records.iter().filter(|r| r.ok).collect();
    rep.failed_cycles = (records.len() - ok.len()) as u64;
    rep.cycles = ok.len() as u64;
    let windows: Vec<(i64, i64)> = ok
        .iter()
        .map(|r| (r.slot, r.slot + r.interval_s as i64))
        .collect();
    let in_scope = |t: i64| windows.iter().any(|(a, b)| (*a..*b).contains(&t));
    let (mut sa, mut sw) = (Vec::new(), Vec::new());
    for r in &ok {
        let (t, iv) = (r.slot, r.interval_s as i64);
        match &r.alert_keys {
            Some(keys) => sa.extend(keys.iter().cloned().map(|k| key_event(t, iv, k))),
            None => sa.extend(r.alerts.iter().map(|a| Event {
                ts: t,
                interval: iv,
                key: None,
                sk: Some((a.session.clone(), a.kind.clone())),
            })),
        }
        match &r.woken_keys {
            Some(keys) => {
                rep.haiku_excluded += keys.iter().filter(|w| w.haiku).count() as u64;
                let rule = keys.iter().filter(|w| !w.haiku);
                sw.extend(rule.map(|w| key_event(t, iv, w.key.clone())));
            }
            None => {
                // See MB1595: the cycle's own triage decision, not the backlog.
                let haiku = r.haiku_woken.unwrap_or(0);
                rep.haiku_excluded += haiku;
                let woke = r.woken.map_or_else(
                    || {
                        r.wake_phrase
                            .as_deref()
                            .is_some_and(|w| w.starts_with("dry-run: would send"))
                    },
                    |w| w > haiku,
                );
                if woke {
                    sw.push(Event {
                        ts: t,
                        interval: iv,
                        key: None,
                        sk: None,
                    });
                }
            }
        }
    }
    rep.haiku_excluded += live_haiku.iter().filter(|t| in_scope(**t)).count() as u64;
    let la: Vec<Event> = live
        .iter()
        .map(|a| {
            let sk = Some((a.key.session.clone(), a.key.kind.clone()));
            let key = a.fingerprint.as_ref().map(|f| EventKey {
                session: a.key.session.clone(),
                kind: a.key.kind.clone(),
                fingerprint: f.clone(),
            });
            Event {
                ts: a.ts,
                interval: 0,
                key,
                sk,
            }
        })
        .collect();
    let lw: Vec<Event> = live_wakes
        .iter()
        .map(|w| Event {
            ts: w.ts,
            interval: 0,
            key: w.key.clone(),
            sk: w.sk.clone(),
        })
        .collect();
    // MB1595 fix round: il_cron is unmeasurable in a dry run (il-cron.py:249).
    let mut excluded = 0;
    let mut drop_il = |v: Vec<Event>, live_side: bool| -> Vec<Event> {
        let (il, rest): (Vec<Event>, Vec<Event>) = v
            .into_iter()
            .partition(|e| e.sk.as_ref().is_some_and(|(_, k)| k == IL_CRON));
        excluded += il.iter().filter(|e| !live_side || in_scope(e.ts)).count() as u64;
        rest
    };
    let (sa, la) = (drop_il(sa, false), drop_il(la, true));
    let (sw, lw) = (drop_il(sw, false), drop_il(lw, true));
    rep.il_cron_excluded = excluded;
    let both = |s: &[Event], l: &[Event]| -> (Joined, Joined) {
        (join(s, l, &in_scope, false), join(s, l, &in_scope, true))
    };
    let (ai, af) = both(&sa, &la);
    let (wi, wf) = both(&sw, &lw);
    (rep.alerts_identity, rep.alerts) = (ai.counts, af.counts);
    (rep.wakes_identity, rep.wakes) = (wi.counts, wf.counts);
    rep.alert_pairs = (af.by_identity, af.by_fallback);
    rep.wake_pairs = (wf.by_identity, wf.by_fallback);
    rep
}

/// `alert_triage.alert_id`: `sha1("session|kind|fingerprint|ts")[:10]`.
pub fn alert_id(session: &str, kind: &str, fingerprint: &str, ts: &str) -> String {
    let hex = sha1_hex(format!("{session}|{kind}|{fingerprint}|{ts}").as_bytes());
    hex.chars().take(10).collect()
}

/// Reads the shadow record, `alerts.jsonl`, `triage.jsonl` and `poll.log`
/// for rows at or after `since` (Unix seconds) and joins them.
///
/// # Errors
/// [`SchedError::Io`] when the shadow record exists but cannot be read;
/// [`SchedError::Parse`] when one of its lines is not a record. Missing
/// live files read as empty (the report then shows poller-only zero).
pub fn load(layout: &Layout, since: i64) -> Result<Report, SchedError> {
    let shadow_path = layout.shadow_log();
    let text = std::fs::read_to_string(&shadow_path)
        .map_err(|e| SchedError::io(format!("read {}", shadow_path.display()), e))?;
    let mut records = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let r: ShadowRecord = serde_json::from_str(line).map_err(|e| SchedError::Parse {
            what: shadow_path.display().to_string(),
            detail: e.to_string(),
        })?;
        if r.slot >= since {
            records.push(r);
        }
    }
    let mut unread = 0;
    let mut live = Vec::new();
    let mut ids: HashMap<String, EventKey> = HashMap::new();
    for line in read_lines(&layout.alerts()) {
        let row: Option<Value> = serde_json::from_str(&line).ok();
        let parsed = row.as_ref().and_then(|v| {
            let raw_ts = v["ts"].as_str()?;
            let key = ShadowAlert {
                session: v["session"].as_str()?.to_owned(),
                kind: v["kind"].as_str()?.to_owned(),
            };
            let fp = v["fingerprint"].as_str().map(str::to_owned);
            Some((raw_ts.to_owned(), parse_iso(raw_ts)?, key, fp))
        });
        match parsed {
            Some((raw, ts, key, fp)) if ts >= since => {
                if let Some(f) = &fp {
                    let id = alert_id(&key.session, &key.kind, f, &raw);
                    let ek = EventKey {
                        session: key.session.clone(),
                        kind: key.kind.clone(),
                        fingerprint: f.clone(),
                    };
                    ids.insert(id, ek);
                }
                live.push(LiveAlert {
                    ts,
                    key,
                    fingerprint: fp,
                });
            }
            Some(_) => {}
            None => unread += 1,
        }
    }
    let (mut wakes, mut haiku) = (Vec::new(), Vec::new());
    for line in read_lines(&layout.root.join("inbox/triage.jsonl")) {
        // A wake row: `"decision": "wake"`; its `path` says rule or Haiku.
        if !line.contains("\"decision\": \"wake\"") {
            continue;
        }
        let row: Option<Value> = serde_json::from_str(&line).ok();
        let at = row
            .as_ref()
            .and_then(|v| Some((parse_iso(v["ts"].as_str()?)?, v)));
        match at {
            Some((t, v)) if t >= since => {
                if v["path"].as_str() == Some("haiku") {
                    haiku.push(t);
                    continue;
                }
                let key = v["id"].as_str().and_then(|i| ids.get(i)).cloned();
                let sk = match (v["session"].as_str(), v["kind"].as_str()) {
                    (Some(s), Some(k)) => Some((s.to_owned(), k.to_owned())),
                    _ => None,
                };
                wakes.push(LiveWake { ts: t, key, sk });
            }
            Some(_) => {}
            None => unread += 1,
        }
    }
    // No triage record: the poll.log cycle lines, with no Haiku split.
    let lines = if wakes.is_empty() && haiku.is_empty() {
        read_lines(&layout.poll_log())
    } else {
        Vec::new()
    };
    for line in lines {
        if !(line.contains(" panes=") && live_woke(&line)) {
            continue;
        }
        match line.split(' ').next().and_then(parse_iso) {
            Some(t) if t >= since => wakes.push(LiveWake {
                ts: t,
                key: None,
                sk: None,
            }),
            Some(_) => {}
            None => unread += 1,
        }
    }
    let mut rep = compute_full(&records, &live, &wakes, &haiku);
    rep.unread = unread;
    Ok(rep)
}

/// A live cycle line whose triage woke: `woken=N` with N > 0, or, on a line
/// from before the `woken=` field, `wake=sent`.
fn live_woke(line: &str) -> bool {
    match line.split(' ').find_map(|f| f.strip_prefix("woken=")) {
        Some(n) => n.parse::<u64>().is_ok_and(|n| n > 0),
        None => line.contains(" wake=sent"),
    }
}

fn read_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The report as text against the default [`Thresholds`].
pub fn render(r: &Report) -> String {
    render_with(r, &Thresholds::default())
}

fn counts_line(what: &str, c: &Counts) -> String {
    format!(
        "{what}: matched {} / scheduler-only {} / poller-only {}\n",
        c.matched, c.scheduler_only, c.poller_only
    )
}

/// The report as text: the identity-plus-fallback counts the verdict uses,
/// the identity-only counts, the pairs per method, the exclusions, the match
/// percentages and the verdict against `th`.
pub fn render_with(r: &Report, th: &Thresholds) -> String {
    let verdict = if r.passes(th) { "PASS" } else { "FAIL" };
    let mut s = format!(
        "cycles joined: {} (failed shadow runs left out: {})\n",
        r.cycles, r.failed_cycles
    );
    s += &counts_line("alerts", &r.alerts);
    s += &counts_line("wakes", &r.wakes);
    s += &counts_line("alerts (identity only)", &r.alerts_identity);
    s += &counts_line("wakes (identity only)", &r.wakes_identity);
    s += &format!(
        "pairs by identity / by +-2-cycle fallback: alerts {} / {}, wakes {} / {}\n",
        r.alert_pairs.0, r.alert_pairs.1, r.wake_pairs.0, r.wake_pairs.1
    );
    s += &format!("live rows unread: {}\n", r.unread);
    s += &format!("haiku-path wakes excluded: {}\n", r.haiku_excluded);
    s += &format!(
        "il_cron excluded: {} (unmeasurable in dry run: il-cron.py:249)\n",
        r.il_cron_excluded
    );
    s += &format!(
        "alert match: {:.1}% (identity only {:.1}%)\nwake match: {:.1}% (identity only {:.1}%)\n",
        r.alert_pct(),
        pct(&r.alerts_identity),
        r.wake_pct(),
        pct(&r.wakes_identity),
    );
    s += &format!(
        "verdict: {verdict} (identity plus fallback; needs at least {} cycles and {:.1}% for alerts and for wakes)\n",
        th.min_cycles, th.min_match_pct,
    );
    s
}

/// `matched` as a percent of every compared item; 100 when none was compared.
fn pct(c: &Counts) -> f64 {
    let total = c.matched + c.scheduler_only + c.poller_only;
    if total == 0 {
        100.0
    } else {
        c.matched as f64 * 100.0 / total as f64
    }
}

impl Report {
    /// Alerts matched (identity plus fallback), percent of compared alerts.
    pub fn alert_pct(&self) -> f64 {
        pct(&self.alerts)
    }

    /// Wakes matched (identity plus fallback), percent of compared wakes.
    pub fn wake_pct(&self) -> f64 {
        pct(&self.wakes)
    }

    /// PASS: at least `th.min_cycles` joined cycles, and both the alert and
    /// the wake match (identity plus fallback) at least `th.min_match_pct`.
    pub fn passes(&self, th: &Thresholds) -> bool {
        self.cycles >= th.min_cycles
            && self.alert_pct() >= th.min_match_pct
            && self.wake_pct() >= th.min_match_pct
    }

    /// Alerts and wakes matched, as a percentage of every compared item
    /// (matched plus either side's extras); 100 when nothing was compared.
    pub fn match_pct(&self) -> f64 {
        let total: u64 = [self.alerts, self.wakes]
            .iter()
            .map(|c| c.matched + c.scheduler_only + c.poller_only)
            .sum();
        let matched = self.alerts.matched + self.wakes.matched;
        if total == 0 {
            100.0
        } else {
            matched as f64 * 100.0 / total as f64
        }
    }
}
