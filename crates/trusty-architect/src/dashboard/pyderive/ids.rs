//! `dashboard_derive.py`'s recycled id space: base-34, 1,156 values per prefix (#12029 P2b).
//!
//! Why: ids recycle, so the next id and the retire list come from the
//! store itself; the digits are base 34 (owner 2026-11-12), never hex, and
//! `00` is never issued. The serde [`crate::dashboard::ids`] still does hex
//! arithmetic over 256 ids; moving it to these rules is #12029 P2d.
//! What: over [`ID_COLLECTIONS`], ids matching `^[A-Z][0-9a-km-np-z]{2}$`
//! (Python's `$` also before one final `\n`): [`used_ids`], [`free_count`],
//! [`next_id`] (lowest free after the issue point, the higher of the top id
//! and `last_issued`, wrapping; never `00`), [`retire_candidates`] (oldest
//! finished first so `keep_free` stay free, plus everything finished over
//! `age_days` ago; nothing finished or updated within 24 h, nothing with
//! `keep` true) and [`kept_ids`]. Digits go through [`crate::ids36`].
//! Test: `dashboard_derive_recordings_replay_clean_against_the_port` (`ids`).

use std::collections::BTreeSet;

use super::{PySnap, Time, coll, ts};
use crate::Clock;
use crate::classify::alerts::days_from_civil;
use crate::dashboard::derive::now_micros;
use crate::ids36;
use crate::pyjson_load::PyValue;
use crate::pyval::{get, has, is_str};
use crate::tasks::store_read::PyDoc;

/// Collections whose ids share the per-prefix space (`ID_COLLECTIONS`).
pub(crate) const ID_COLLECTIONS: [&str; 6] = [
    "items",
    "goals",
    "priorities",
    "sessions",
    "questions",
    "media",
];
/// Values per prefix (`SPACE`).
const SPACE: i64 = ids36::SPACE as i64;
/// The 25% free floor (`KEEP_FREE`).
pub(crate) const KEEP_FREE: i64 = SPACE / 4;
const DAY_US: f64 = 86_400_000_000.0;

/// `ID_RE.match(id)`: the prefix letter and the number of its two digits.
fn id_number(id: &str) -> Option<(char, i64)> {
    let id = id.strip_suffix('\n').unwrap_or(id);
    let mut c = id.chars();
    let p = c.next().filter(char::is_ascii_uppercase)?;
    let digits = c.as_str();
    let ok = digits.chars().count() == 2 && digits.chars().all(|d| ids36::DIGITS.contains(d));
    let n = ok.then(|| ids36::dec(digits).ok()).flatten()?;
    Some((p, i64::try_from(n).ok()?))
}

fn id_docs<'a>(snap: &'a PySnap) -> impl Iterator<Item = &'a PyDoc> + 'a {
    ID_COLLECTIONS
        .iter()
        .flat_map(move |c| coll(snap, c).iter())
}

fn numbered<'a>(snap: &'a PySnap, prefix: &'a str) -> impl Iterator<Item = (&'a PyDoc, i64)> + 'a {
    id_docs(snap).filter_map(move |d| {
        let (p, n) = id_number(&d.id)?;
        (p.to_string() == prefix).then_some((d, n))
    })
}

fn enc(prefix: &str, n: i64) -> String {
    format!("{prefix}{}", ids36::enc(n).unwrap_or_default())
}

/// `used_ids(snap, prefix)`.
pub(crate) fn used_ids(snap: &PySnap, prefix: &str) -> BTreeSet<i64> {
    numbered(snap, prefix).map(|(_, n)| n).collect()
}

/// `free_count(snap, prefix)`: issuable ids not in use (`00` is never free).
pub(crate) fn free_count(snap: &PySnap, prefix: &str) -> i64 {
    let used = used_ids(snap, prefix);
    SPACE - 1 - used.iter().filter(|n| **n != 0).count() as i64
}

/// Why: a new item takes the lowest free id after the issue point.
/// What: `next_id(snap, prefix, last_issued)`; `None` when the space is full.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub(crate) fn next_id(snap: &PySnap, prefix: &str, last_issued: Option<i64>) -> Option<String> {
    let used = used_ids(snap, prefix);
    let mut top = used.last().copied().unwrap_or(-1);
    if let Some(l) = last_issued {
        top = top.max(l);
    }
    (1..=SPACE)
        .map(|k| (top + k).rem_euclid(SPACE))
        .find(|c| *c != 0 && !used.contains(c))
        .map(|c| enc(prefix, c))
}

/// `_finished_at(data)`: when a finished item finished; `None` when it is not retire-eligible.
fn finished_at(d: &PyValue) -> Option<Time> {
    let keys: &[&str] = match get(d, "status") {
        Some(PyValue::Str(s)) if s == "done" => &["done_at"],
        Some(PyValue::Str(s)) if s == "answered" && has(d, "relayed") => {
            &["relayed_at", "answered_at"]
        }
        Some(PyValue::Str(s)) if s == "cancelled" || s == "wont_do" => &[
            "cancelled_at",
            "wont_do_at",
            "answered_at",
            "relayed_at",
            "done_at",
        ],
        _ => return None,
    };
    let stamps: Vec<Time> = keys.iter().filter_map(|k| ts(get(d, k))).collect();
    let first_kind = is_str(get(d, "status"), "done") || is_str(get(d, "status"), "answered");
    let pick = if first_kind {
        stamps.first().copied()
    } else {
        stamps
            .iter()
            .copied()
            .fold(None, |best: Option<Time>, t| match best {
                Some(b) if b.utc >= t.utc => Some(b),
                _ => Some(t),
            })
    };
    let min = days_from_civil(1, 1, 1) * 86_400_000_000;
    pick.or_else(|| ts(get(d, "updated")))
        .or_else(|| ts(get(d, "created")))
        .or(Some(Time {
            utc: min,
            wall: min,
        }))
}

/// The retire rule's inputs besides the snapshot.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Retire {
    /// `keep_free`.
    pub(crate) keep_free: i64,
    /// `age_days`.
    pub(crate) age_days: Option<f64>,
    /// `now` as Unix seconds; `None` reads the clock.
    pub(crate) now: Option<f64>,
}

/// Why: done items must be retired before the space runs out (LDG-2.2.4).
/// What: `retire_candidates(snap, prefix, keep_free, age_days, now)`; see the
/// module doc. Without `now` the time is `clock.now_unix()`.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub(crate) fn retire_candidates(
    snap: &PySnap,
    prefix: &str,
    r: Retire,
    clock: &dyn Clock,
) -> Vec<String> {
    let now = now_micros(r.now.unwrap_or_else(|| clock.now_unix()));
    let guard = now - 24 * 3_600_000_000;
    let old = r
        .age_days
        .map(|a| now - (a * DAY_US).round_ties_even() as i64);
    let need = (r.keep_free - free_count(snap, prefix)).max(0);
    if need <= 0 && old.is_none() {
        return Vec::new();
    }
    let mut fin: Vec<(i64, i64)> = Vec::new();
    for (doc, n) in numbered(snap, prefix) {
        let d = &doc.data;
        if get(d, "keep") == Some(&PyValue::Bool(true)) {
            continue;
        }
        let Some(t) = finished_at(d) else { continue };
        let recent = ts(get(d, "updated")).is_some_and(|u| u.utc > guard);
        if t.utc > guard || recent {
            continue;
        }
        fin.push((t.utc, n));
    }
    fin.sort();
    let need = usize::try_from(need).unwrap_or(usize::MAX);
    let mut picked: BTreeSet<i64> = fin
        .iter()
        .filter(|x| x.1 != 0)
        .take(need)
        .map(|x| x.1)
        .collect();
    if let Some(old) = old {
        picked.extend(fin.iter().filter(|x| x.0 < old).map(|x| x.1));
    }
    fin.iter()
        .filter(|x| picked.contains(&x.1))
        .map(|x| enc(prefix, x.1))
        .collect()
}

/// `kept_ids(snap, prefix)`: ids pinned with `keep` true, sorted.
pub(crate) fn kept_ids(snap: &PySnap, prefix: &str) -> Vec<String> {
    let mut out: Vec<String> = id_docs(snap)
        .filter(|d| {
            id_number(&d.id).is_some()
                && d.id.chars().next().map(String::from).as_deref() == Some(prefix)
        })
        .filter(|d| get(&d.data, "keep") == Some(&PyValue::Bool(true)))
        .map(|d| d.id.clone())
        .collect();
    out.sort();
    out
}
