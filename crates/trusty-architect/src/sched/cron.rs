//! A five-field cron parser and next-fire search, UTC only.
//!
//! Why: no cron crate is in the workspace and the leaf crate takes no new
//! dependency; the contract needs lists, ranges, steps, dow 0 or 7 as Sunday
//! and the Vixie day-of-month / day-of-week OR rule, nothing more.
//! What: [`Cron::parse`] reads `min hour dom month dow` into bit masks.
//! [`Cron::next_after`] returns the first matching minute strictly after a
//! Unix time, walking whole days and then hours, so it stays fast for a rare
//! date; it gives up (`None`) after eight years (a 29 February schedule fits).
//! Test: `cron_parses_lists_ranges_and_steps`, `cron_rejects_bad_fields`,
//! `cron_next_after_finds_the_next_slot`, `cron_dom_or_dow_when_both_restricted`,
//! `cron_dow_seven_is_sunday`.

use crate::classify::alerts::civil_from_days;

/// A parsed cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    min: u64,
    hour: u32,
    dom: u32,
    month: u32,
    dow: u32,
    dom_star: bool,
    dow_star: bool,
}

/// One field: the allowed-value mask and whether the text began with `*`.
fn field(text: &str, lo: u32, hi: u32, name: &str) -> Result<(u64, bool), String> {
    if text.is_empty() {
        return Err(format!("{name}: empty field"));
    }
    let star = text.starts_with('*');
    let mut mask = 0u64;
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|n| *n >= 1)),
            None => (item, Some(1)),
        };
        let step = step.ok_or_else(|| format!("{name}: bad step in {item:?}"))?;
        let num = |s: &str| {
            s.parse::<u32>()
                .ok()
                .filter(|n| (lo..=hi).contains(n))
                .ok_or_else(|| format!("{name}: {s:?} is outside {lo}-{hi}"))
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (num(a)?, num(b)?)
        } else {
            let a = num(range)?;
            (a, if item.contains('/') { hi } else { a })
        };
        if a > b {
            return Err(format!("{name}: range {range:?} runs backwards"));
        }
        let mut v = a;
        while v <= b {
            mask |= 1 << v;
            v += step;
        }
    }
    Ok((mask, star))
}

impl Cron {
    /// Parses `min hour dom month dow`.
    ///
    /// # Errors
    /// A message naming the field when there are not five fields or one is
    /// out of range, empty, backwards or has a bad step.
    pub fn parse(text: &str) -> Result<Self, String> {
        let parts: Vec<&str> = text.split_whitespace().collect();
        let [min, hour, dom, month, dow] = parts[..] else {
            return Err(format!("{} fields, need 5", parts.len()));
        };
        let (min, _) = field(min, 0, 59, "minute")?;
        let (hour, _) = field(hour, 0, 23, "hour")?;
        let (dom, dom_star) = field(dom, 1, 31, "day-of-month")?;
        let (month, _) = field(month, 1, 12, "month")?;
        let (dow7, dow_star) = field(dow, 0, 7, "day-of-week")?;
        // 7 is Sunday too.
        let dow = (dow7 | (dow7 >> 7)) & 0x7f;
        Ok(Self {
            min,
            hour: hour as u32,
            dom: dom as u32,
            month: month as u32,
            dow: dow as u32,
            dom_star,
            dow_star,
        })
    }

    fn day_ok(&self, days: i64) -> bool {
        let (_, month, dom) = civil_from_days(days);
        if self.month >> month & 1 == 0 {
            return false;
        }
        // 1970-01-01 was a Thursday (4).
        let dow = (days + 4).rem_euclid(7);
        let dom_ok = self.dom >> dom & 1 == 1;
        let dow_ok = self.dow >> dow & 1 == 1;
        if !self.dom_star && !self.dow_star {
            dom_ok || dow_ok
        } else {
            dom_ok && dow_ok
        }
    }

    /// True when the minute holding `t` matches.
    pub fn matches(&self, t: i64) -> bool {
        let m = t.div_euclid(60);
        self.day_ok(m.div_euclid(1440))
            && self.hour >> (m.rem_euclid(1440) / 60) & 1 == 1
            && self.min >> (m.rem_euclid(60)) & 1 == 1
    }

    /// The first matching minute strictly after `t`, as Unix seconds.
    pub fn next_after(&self, t: i64) -> Option<i64> {
        let start = t.div_euclid(60) + 1;
        let limit = start + 8 * 366 * 1440;
        let mut m = start;
        while m < limit {
            let day = m.div_euclid(1440);
            if !self.day_ok(day) {
                m = (day + 1) * 1440;
                continue;
            }
            let hour = m.rem_euclid(1440) / 60;
            if self.hour >> hour & 1 == 0 {
                m = (m.div_euclid(60) + 1) * 60;
                continue;
            }
            if self.min >> m.rem_euclid(60) & 1 == 0 {
                m += 1;
                continue;
            }
            return Some(m * 60);
        }
        None
    }
}
