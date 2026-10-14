//! Burn rate, projection, the cutover plan and the `usage` alert texts: the
//! pure half of `usage.py`.
//!
//! Why: Bob (2026-10-07) asked to track usage "so we can intelligently plan
//! cutovers"; the poller raises `usage` alerts from these figures and the
//! fingerprints dedup each window once, so text and keys must match the
//! prototype byte for byte (#9801).
//! What: [`effective`] (a reading at `now`: a passed reset counts 0%),
//! [`burn_rate`], [`project`], [`plan`] (with [`runway`]) and
//! [`usage_alerts`], each a port of the function of the same name. One
//! ruled change: [`burn_rate`] returns `None` when no sample of the newest
//! window lies inside the burn span, where the prototype raises
//! `IndexError` (tests/fixtures/9801/README.md, `burn_rate_empty_run_is_no_rate`).
//! Test: `tests/parity_usage.rs`, `tests/usage_ported.rs`.

use std::cmp::Ordering;

use super::ledger::{Pct, Reading, latest, latest_by_account};
use super::utc::UtcTime;
use crate::error::UsageError;
use crate::poller::pyfmt::py_g;

/// Alert when the 5h window is projected to run out within this many minutes.
pub const WARN_5H_MIN: f64 = 45.0;
/// Weekly thresholds that alert.
pub const WEEK_WARN: [u32; 2] = [80, 90];
/// A next account needs its 5h window below this.
pub const ELIGIBLE_5H: f64 = 95.0;
const H5: f64 = 5.0 * 3600.0;
const STALE_AFTER: f64 = 24.0 * 3600.0;
const BURN_MIN_SPAN: f64 = 10.0 * 60.0;

/// One usage window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The 5-hour window (`pct5h`, `reset5h`).
    FiveHour,
    /// The weekly window (`pctwk`, `resetwk`).
    Week,
}

impl Window {
    fn keys(self) -> (&'static str, &'static str) {
        match self {
            Self::FiveHour => ("pct5h", "reset5h"),
            Self::Week => ("pctwk", "resetwk"),
        }
    }
    /// Samples older than this many seconds are ignored.
    fn span(self) -> f64 {
        match self {
            Self::FiveHour => 3.0 * 3600.0,
            Self::Week => 48.0 * 3600.0,
        }
    }
    /// A longer gap between samples ends the run.
    fn max_gap(self) -> f64 {
        match self {
            Self::FiveHour => 5400.0,
            Self::Week => 6.0 * 3600.0,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::FiveHour => "5-hour",
            Self::Week => "weekly",
        }
    }
}

/// A reading's usable figures at `now`.
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    /// 5-hour percent; `Int(0)` when inferred.
    pub pct5h: Option<Pct>,
    /// Weekly percent; `Int(0)` when inferred.
    pub pctwk: Option<Pct>,
    /// 5-hour reset, `None` when inferred.
    pub reset5h: Option<UtcTime>,
    /// Weekly reset, `None` when inferred.
    pub resetwk: Option<UtcTime>,
    /// The 5-hour window counts as 0% (reset passed, or no reset and over 5 h old).
    pub inferred5h: bool,
    /// The weekly reset has passed.
    pub inferredwk: bool,
    /// Over a day old and the weekly reset has not passed.
    pub stale: bool,
    /// Seconds since the reading.
    pub age: f64,
}

impl Effective {
    fn pct(&self, w: Window) -> Option<Pct> {
        match w {
            Window::FiveHour => self.pct5h,
            Window::Week => self.pctwk,
        }
    }
    fn reset(&self, w: Window) -> Option<UtcTime> {
        match w {
            Window::FiveHour => self.reset5h,
            Window::Week => self.resetwk,
        }
    }
    fn inferred(&self, w: Window) -> bool {
        match w {
            Window::FiveHour => self.inferred5h,
            Window::Week => self.inferredwk,
        }
    }
}

/// `effective(rec, now)`.
///
/// # Errors
/// A time or percent of the wrong form.
pub fn effective(rec: &Reading, now: UtcTime) -> Result<Effective, UsageError> {
    let age = now.seconds_since(rec.time()?);
    let (r5, rw) = (rec.reset("reset5h")?, rec.reset("resetwk")?);
    let (mut p5, mut pw) = (rec.pct("pct5h")?, rec.pct("pctwk")?);
    let (mut inf5, mut infw) = (false, false);
    if r5.is_some_and(|r| r <= now) {
        (p5, inf5) = (Some(Pct::Int(0)), true);
    } else if r5.is_none() && p5.is_none_or(|p| p.get() != 0.0) && age > H5 {
        // No reset on file and the reading is over 5 h old.
        (p5, inf5) = (Some(Pct::Int(0)), true);
    }
    if rw.is_some_and(|r| r <= now) {
        (pw, infw) = (Some(Pct::Int(0)), true);
    }
    Ok(Effective {
        pct5h: p5,
        pctwk: pw,
        reset5h: if inf5 { None } else { r5 },
        resetwk: if infw { None } else { rw },
        inferred5h: inf5,
        inferredwk: infw,
        stale: age > STALE_AFTER && !infw,
        age,
    })
}

/// A measured burn: `%/h` and the first and last `(time, percent)` samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burn {
    /// Percent per hour.
    pub rate: f64,
    /// The run's first sample.
    pub first: (UtcTime, f64),
    /// The run's last sample.
    pub last: (UtcTime, f64),
}

/// `burn_rate(rows, account, now, kind)`: the rate over the latest run of
/// samples that share the newest sample's reset (within 300 s), lie within
/// the window's span and are linked by gaps under its maximum. `None` under
/// two samples, under ten minutes apart, or not rising.
///
/// # Errors
/// A time or percent of the wrong form in this account's samples.
pub fn burn_rate(
    rows: &[Reading],
    account: &str,
    now: UtcTime,
    w: Window,
) -> Result<Option<Burn>, UsageError> {
    let (pk, rk) = w.keys();
    let mine: Vec<&Reading> = rows
        .iter()
        .filter(|r| r.account == account && r.has(pk))
        .collect();
    let Some(last) = mine.last() else {
        return Ok(None);
    };
    let lr = last.reset(rk)?;
    let mut pts = Vec::new();
    for r in &mine {
        let t = r.time()?;
        let rr = r.reset(rk)?;
        let same = match (rr, lr) {
            (None, None) => true,
            (Some(a), Some(b)) => a.seconds_since(b).abs() < 300.0,
            _ => false,
        };
        if same && now.seconds_since(t) <= w.span() {
            let pct = r.pct(pk)?.map_or(0.0, Pct::get);
            pts.push((t, pct));
        }
    }
    // #9801 (ruled): the prototype indexes pts[-1] here and raises IndexError.
    let Some(&newest) = pts.last() else {
        return Ok(None);
    };
    let mut run = vec![newest];
    for pt in pts[..pts.len() - 1].iter().rev() {
        if run[0].0.seconds_since(pt.0) > w.max_gap() {
            break;
        }
        run.insert(0, *pt);
    }
    let ((t0, p0), (t1, p1)) = (run[0], newest);
    let dt = t1.seconds_since(t0);
    if run.len() < 2 || dt < BURN_MIN_SPAN || p1 <= p0 {
        return Ok(None);
    }
    Ok(Some(Burn {
        rate: (p1 - p0) / (dt / 3600.0),
        first: run[0],
        last: newest,
    }))
}

/// Projected exhaustion of one window of one account.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    /// Percent per hour.
    pub rate: f64,
    /// When the window reaches 100% at that rate, from the last sample.
    pub at: UtcTime,
    /// The newest reading's reset for this window.
    pub reset: Option<UtcTime>,
    /// `at` comes before `reset` (or there is no reset).
    pub before_reset: bool,
    /// The last sample's percent.
    pub pct: f64,
}

/// `project(rows, account, now, kind)`.
///
/// # Errors
/// As [`burn_rate`]; [`UsageError::OutOfRange`] when the projected time is
/// past year 9999 (a near-zero rate).
pub fn project(
    rows: &[Reading],
    account: &str,
    now: UtcTime,
    w: Window,
) -> Result<Option<Projection>, UsageError> {
    let br = burn_rate(rows, account, now, w)?;
    let (Some(br), Some(last)) = (br, latest(rows, account)) else {
        return Ok(None);
    };
    let (t1, p1) = br.last;
    let at = t1.plus_hours((100.0 - p1) / br.rate)?;
    let reset = last.reset(w.keys().1)?;
    Ok(Some(Projection {
        rate: br.rate,
        at,
        reset,
        before_reset: reset.is_none_or(|r| at < r),
        pct: p1,
    }))
}

/// `_hm`: hours with one decimal.
fn hm(seconds: f64) -> String {
    let h = if seconds > 0.0 { seconds } else { 0.0 } / 3600.0;
    format!("{h:.1} h")
}

/// `_fmt`: `<iso> / <New York time> NY`, or `-`.
fn fmt(t: Option<UtcTime>) -> String {
    t.map_or_else(
        || "-".to_owned(),
        |t| format!("{} / {} NY", t.iso(), t.ny()),
    )
}

/// Other accounts usable for a cutover (best first) and the skipped ones.
type Ranked = (Vec<(String, Effective)>, Vec<(String, String)>);

/// `rank_candidates`: eligible when weekly is known and under 100 and the
/// 5h window is under [`ELIGIBLE_5H`]; lowest weekly first, within 5 points
/// the later weekly reset first.
fn rank_candidates(rows: &[Reading], now: UtcTime, current: &str) -> Result<Ranked, UsageError> {
    let (mut out, mut skipped) = (Vec::new(), Vec::new());
    for (acct, rec) in latest_by_account(rows) {
        if acct == current {
            continue;
        }
        let e = effective(rec, now)?;
        match (e.pctwk, e.pct5h) {
            (None, _) => skipped.push((acct.to_owned(), "no weekly reading".to_owned())),
            (Some(w), _) if w.get() >= 100.0 => {
                skipped.push((acct.to_owned(), "weekly limit reached".to_owned()));
            }
            (_, Some(p5)) if p5.get() >= ELIGIBLE_5H => {
                skipped.push((acct.to_owned(), format!("5h window at {}%", p5.py_str())));
            }
            _ => out.push((acct.to_owned(), e)),
        }
    }
    let key = |e: &Effective| {
        let w = e.pctwk.map_or(0.0, Pct::get);
        let reset = e.resetwk.map_or(0.0, UtcTime::timestamp);
        ((w / 5.0).round_ties_even(), -reset, w)
    };
    out.sort_by(|a, b| {
        let (x, y) = (key(&a.1), key(&b.1));
        x.0.partial_cmp(&y.0)
            .unwrap_or(Ordering::Equal)
            .then(x.1.partial_cmp(&y.1).unwrap_or(Ordering::Equal))
            .then(x.2.partial_cmp(&y.2).unwrap_or(Ordering::Equal))
    });
    Ok((out, skipped))
}

/// Weekly capacity left across accounts and the runway at the measured rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Runway {
    /// Weekly points left (100 = one account's week).
    pub left: f64,
    /// The highest weekly rate measured on any account.
    pub rate: Option<f64>,
    /// The account that rate was measured on.
    pub rate_account: Option<String>,
    /// Accounts with no weekly reading.
    pub missing: Vec<String>,
    /// The first weekly reset still ahead, and its account.
    pub first_reset: Option<(UtcTime, String)>,
    /// Hours of runway.
    pub runway_h: Option<f64>,
    /// Hours to the first reset.
    pub gap_h: Option<f64>,
    /// The runway reaches the first reset.
    pub covers_gap: Option<bool>,
}

/// `runway(rows, now, current)`; the prototype ignores `current`.
///
/// # Errors
/// As [`effective`] and [`project`] over every account.
pub fn runway(rows: &[Reading], now: UtcTime) -> Result<Runway, UsageError> {
    let (mut left, mut missing, mut resets) = (0.0, Vec::new(), Vec::new());
    let accounts = latest_by_account(rows);
    for (acct, rec) in &accounts {
        let e = effective(rec, now)?;
        let Some(w) = e.pctwk else {
            missing.push((*acct).to_owned());
            continue;
        };
        let room = 100.0 - w.get();
        left += if room > 0.0 { room } else { 0.0 };
        if let Some(r) = e.resetwk.filter(|r| *r > now) {
            resets.push((r, (*acct).to_owned()));
        }
    }
    let mut best: Option<(f64, String)> = None;
    for (acct, _) in &accounts {
        if let Some(p) = project(rows, acct, now, Window::Week)?
            && best.as_ref().is_none_or(|(r, _)| p.rate > *r)
        {
            best = Some((p.rate, (*acct).to_owned()));
        }
    }
    let rate = best.as_ref().map(|b| b.0);
    let first_reset = resets.into_iter().min();
    let runway_h = rate.filter(|r| *r != 0.0).map(|r| left / r);
    let gap_h = first_reset
        .as_ref()
        .map(|(t, _)| t.seconds_since(now) / 3600.0);
    let covers_gap = match (runway_h.filter(|h| *h != 0.0), gap_h) {
        (Some(h), Some(g)) => Some(h >= g),
        _ => None,
    };
    Ok(Runway {
        left,
        rate,
        rate_account: best.map(|b| b.1),
        missing,
        first_reset,
        runway_h,
        gap_h,
        covers_gap,
    })
}

/// A cutover recommendation.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// When the plan was made.
    pub now: UtcTime,
    /// The current account.
    pub current: Option<String>,
    /// A cutover is needed.
    pub need: bool,
    /// Each `(when, why)` that needs one.
    pub triggers: Vec<(UtcTime, String)>,
    /// The earliest trigger.
    pub switch_at: Option<UtcTime>,
    /// The best next account.
    pub next: Option<(String, Effective)>,
    /// The other eligible accounts, best first.
    pub alternatives: Vec<(String, Effective)>,
    /// Ineligible accounts and why.
    pub skipped: Vec<(String, String)>,
    /// Weekly runway; `None` when the current account has no reading.
    pub runway: Option<Runway>,
    /// Why the plan is empty, when it is.
    pub note: Option<String>,
}

/// `plan(rows, now, current)`: `current` defaults to the newest row's account.
///
/// # Errors
/// As [`effective`], [`project`] and [`runway`].
pub fn plan(rows: &[Reading], now: UtcTime, current: Option<&str>) -> Result<Plan, UsageError> {
    let current = current
        .filter(|c| !c.is_empty())
        .or_else(|| rows.last().map(|r| r.account.as_str()));
    let mut res = Plan {
        now,
        current: current.map(str::to_owned),
        need: false,
        triggers: Vec::new(),
        switch_at: None,
        next: None,
        alternatives: Vec::new(),
        skipped: Vec::new(),
        runway: None,
        note: None,
    };
    let Some((current, rec)) = current.and_then(|c| latest(rows, c).map(|r| (c, r))) else {
        res.note = Some("no reading for the current account".to_owned());
        return Ok(res);
    };
    let cur = effective(rec, now)?;
    for w in [Window::FiveHour, Window::Week] {
        let (pct, reset) = (cur.pct(w), cur.reset(w));
        let pr = project(rows, current, now, w)?;
        let label = w.label();
        if let Some(p) = pct.filter(|p| p.get() >= 100.0 && reset.is_none_or(|r| r > now)) {
            let tail = reset.map_or(String::new(), |r| {
                format!("; resets {}, in {}", fmt(Some(r)), hm(r.seconds_since(now)))
            });
            let text = format!("{label} limit reached ({}%){tail}", p.py_str());
            res.triggers.push((now, text));
        } else if let Some(pr) = pr.filter(|p| p.before_reset && !cur.inferred(w)) {
            let at = pr.at.max(now);
            let tail = pr.reset.map_or(String::new(), |r| {
                format!(", before its reset {}", fmt(Some(r)))
            });
            let text = format!(
                "{label} projected to reach 100% at {} (burn {:.1} %/h from {}%){tail}",
                fmt(Some(at)),
                pr.rate,
                py_g(pr.pct)
            );
            res.triggers.push((at, text));
        }
    }
    res.need = !res.triggers.is_empty();
    res.switch_at = res.triggers.iter().map(|(t, _)| *t).min();
    let (cands, skipped) = rank_candidates(rows, now, current)?;
    res.skipped = skipped;
    let mut cands = cands.into_iter();
    res.next = cands.next();
    res.alternatives = cands.collect();
    res.runway = Some(runway(rows, now)?);
    Ok(res)
}

/// `iso(dt)`, which prints `None` for no time.
fn iso_or_none(t: Option<UtcTime>) -> String {
    t.map_or_else(|| "None".to_owned(), UtcTime::iso)
}

/// `usage_alerts(rows, now, current)`: `(fingerprint, detail)` for the
/// current account. The fingerprint carries the reset time, so each window
/// alerts once.
///
/// # Errors
/// As [`plan`].
pub fn usage_alerts(
    rows: &[Reading],
    now: UtcTime,
    current: &str,
) -> Result<Vec<(String, String)>, UsageError> {
    let mut out = Vec::new();
    let Some(rec) = latest(rows, current) else {
        return Ok(out);
    };
    let cur = effective(rec, now)?;
    let p = plan(rows, now, Some(current))?;
    let nxt = p.next.as_ref().map_or(String::new(), |(acct, _)| {
        let sw = p.switch_at.unwrap_or(now).max(now);
        format!(" Next account: {acct}, switch by {}.", sw.iso())
    });
    let pr5 = project(rows, current, now, Window::FiveHour)?;
    if let Some(pr5) = pr5.filter(|p| {
        p.before_reset && !cur.inferred5h && p.at.seconds_since(now) <= WARN_5H_MIN * 60.0
    }) {
        out.push((
            format!("5h-exhaust:{current}:{}", iso_or_none(cur.reset5h)),
            format!(
                "{current} 5h window projected to hit 100% at {} ({:.0} %/h, now {}%).{nxt}",
                pr5.at.max(now).iso(),
                pr5.rate,
                py_g(pr5.pct)
            ),
        ));
    }
    if let Some(wk) = cur.pctwk.filter(|_| !cur.inferredwk) {
        for th in WEEK_WARN {
            if wk.get() >= f64::from(th) {
                let reset = iso_or_none(cur.resetwk);
                out.push((
                    format!("wk>={th}:{current}:{reset}"),
                    format!(
                        "{current} weekly usage {}% (resets {reset}).{nxt}",
                        py_g(wk.get())
                    ),
                ));
            }
        }
    }
    if p.need {
        let next = p.next.as_ref().map(|(a, _)| a.as_str());
        let reasons: Vec<&str> = p.triggers.iter().map(|(_, r)| r.as_str()).collect();
        out.push((
            format!("cutover:{current}:{}", next.unwrap_or("none")),
            format!(
                "Cutover recommended: {current} -> {} by {}. {}",
                next.unwrap_or("no eligible account"),
                p.switch_at.unwrap_or(now).max(now).iso(),
                reasons.join("; ")
            ),
        ));
    }
    Ok(out)
}
