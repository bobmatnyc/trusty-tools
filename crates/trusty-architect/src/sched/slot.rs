//! Slots: when a job is due, jitter and the UTC day.
//!
//! Why: the contract fixes cron semantics but leaves `every` undefined and
//! asks for jitter and catch-up; the engine needs these as pure functions it
//! can test against a fixed clock.
//! What: [`parse_every`] reads `<N>[smhd]`. [`Sched`] is `every` or cron;
//! [`Sched::next_after`] gives the first slot after `now`. `every` is a grid
//! anchored on the job's previous slot (no drift: a late run does not shift
//! the next one). [`jitter`] is a deterministic 0..=max delay from an FNV-1a
//! hash of job id and slot, so a restart computes the same fire time.
//! [`missed_slots`] counts slots passed between a slot and now.
//! [`day_key`] is the UTC `YYYY-MM-DD` that the daily cap and `{date}` use.
//! Test: `every_parses_units`, `every_grid_does_not_drift`,
//! `jitter_is_deterministic_and_bounded`, `missed_slots_counts_the_gap`.

use super::config::Schedule;
use super::cron::Cron;
use crate::classify::alerts::{civil_from_days, days_from_civil};

/// `<N>[smhd]` in seconds; `None` for anything else (including `0m`).
pub fn parse_every(text: &str) -> Option<i64> {
    let unit = text.chars().last()?;
    let n: i64 = text[..text.len() - unit.len_utf8()].parse().ok()?;
    let secs = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return None,
    };
    (n >= 1 && !text.starts_with('+')).then_some(n * secs)
}

/// A job's schedule, parsed.
#[derive(Debug, Clone)]
pub enum Sched {
    /// A fixed interval in seconds.
    Every(i64),
    /// Cron, UTC.
    Cron(Cron),
}

impl Sched {
    /// Parses a validated [`Schedule`].
    ///
    /// # Errors
    /// A message when neither or both of `every` and `cron` are set, or one
    /// does not parse.
    pub fn from_config(s: &Schedule) -> Result<Self, String> {
        match (&s.every, &s.cron) {
            (Some(e), None) => parse_every(e)
                .map(Self::Every)
                .ok_or_else(|| format!("bad every {e:?}")),
            (None, Some(c)) => Cron::parse(c).map(Self::Cron),
            _ => Err("schedule needs exactly one of every or cron".to_owned()),
        }
    }

    /// The first slot strictly after `now`. `anchor` is the previous slot,
    /// the origin of an `every` grid; cron ignores it.
    pub fn next_after(&self, anchor: i64, now: i64) -> Option<i64> {
        match self {
            Self::Every(_) if now < anchor => Some(anchor),
            Self::Every(e) => Some(anchor + ((now - anchor) / e + 1) * e),
            Self::Cron(c) => c.next_after(now),
        }
    }
}

/// A deterministic delay in `0..=max` seconds for `slot` of job `id`.
pub fn jitter(id: &str, slot: i64, max: u64) -> i64 {
    if max == 0 {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes().chain(slot.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % (max + 1)) as i64
}

/// Slots in `(slot, now]`: how many the scheduler missed after `slot`.
/// Counts at most `CAP` so a years-long gap stays cheap.
pub fn missed_slots(sched: &Sched, slot: i64, now: i64) -> u32 {
    const CAP: u32 = 100_000;
    match sched {
        Sched::Every(e) => {
            u32::try_from(((now - slot) / e).clamp(0, i64::from(CAP))).unwrap_or(CAP)
        }
        Sched::Cron(c) => {
            let (mut n, mut t) = (0, slot);
            while n < CAP {
                match c.next_after(t) {
                    Some(next) if next <= now => {
                        n += 1;
                        t = next;
                    }
                    _ => break,
                }
            }
            n
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SS` with a `Z` or nothing after it, as Unix seconds.
pub fn parse_iso(text: &str) -> Option<i64> {
    let t = text.trim_end_matches('Z');
    let (date, time) = t.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut c = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, mi, s) = (c.next()??, c.next()??, c.next()??);
    let ok = (1..=12).contains(&m) && (1..=31).contains(&day) && h < 24 && mi < 60 && s < 61;
    ok.then(|| days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + s)
}

/// The UTC date of `t` as `YYYY-MM-DD`.
pub fn day_key(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}
