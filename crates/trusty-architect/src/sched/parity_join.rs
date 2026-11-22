//! The parity join's matcher: event identity first, then a time tolerance
//! (MB1595 fix round, class A).
//!
//! Why: the 2nd parity run joined each shadow record to live rows only
//! inside `[slot, slot + interval)`; a live alert raised one cycle earlier or
//! later (phase skew between the two pollers) counted twice as a mismatch.
//! What: [`join`] pairs shadow events with live events. Pass 1 pairs equal
//! identities (session, kind, fingerprint: the dedupe key; a PM message's id
//! and an owner item's id are their fingerprints) within
//! [`IDENTITY_CYCLES`] cycles of the record; pass 2, when asked, pairs what
//! is left by session and kind (an event with no identity pairs with any)
//! within [`FALLBACK_CYCLES`] cycles. Each pass takes the nearest unused live
//! event. A shadow event left over is `scheduler_only`; a live event left
//! over counts as `poller_only` only when it falls inside some joined
//! record's own window. [`IL_CRON`] is left out of both sides.
//! Test: `join_pairs_identity_across_phase_skew`,
//! `join_falls_back_to_two_cycles_without_identity`,
//! `parity_reports_identity_and_fallback_numbers`.

use super::parity::Counts;
use super::shadow::EventKey;

/// Identity pairs within this many cycles of the record: below the shortest
/// re-emit (30 cycles at 60 s), so a re-emit never pairs with the first.
pub const IDENTITY_CYCLES: i64 = 15;
/// The fallback's tolerance, in cycles, on each side of the record window.
pub const FALLBACK_CYCLES: i64 = 2;
/// The kind left out of parity: `il-cron.py` only prints `would run` in a dry
/// run (il-cron.py:249), so the shadow can never raise it.
pub const IL_CRON: &str = "il_cron";

/// One side's event: when, its identity if known, and its session and kind
/// if known (`None` pairs with anything in the fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Shadow: the record's slot; live: the row's time. Unix seconds.
    pub ts: i64,
    /// Shadow: the record's interval; live: unused.
    pub interval: i64,
    /// Session, kind and fingerprint.
    pub key: Option<EventKey>,
    /// Session and kind.
    pub sk: Option<(String, String)>,
}

impl Event {
    /// An event with full identity.
    pub fn keyed(ts: i64, interval: i64, key: EventKey) -> Self {
        let sk = Some((key.session.clone(), key.kind.clone()));
        Self {
            ts,
            interval,
            key: Some(key),
            sk,
        }
    }
}

/// What [`join`] found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Joined {
    /// Matched (both passes), scheduler-only and poller-only.
    pub counts: Counts,
    /// Pairs made by identity.
    pub by_identity: u64,
    /// Pairs made by the fallback.
    pub by_fallback: u64,
}

/// Seconds from `t` to the window `[from, to)`; 0 inside.
fn distance(t: i64, from: i64, to: i64) -> i64 {
    if t < from {
        from - t
    } else if t >= to {
        t - to + 1
    } else {
        0
    }
}

fn nearest(
    s: &Event,
    live: &[Event],
    used: &[bool],
    cycles: i64,
    ok: &dyn Fn(&Event) -> bool,
) -> Option<usize> {
    let (from, to) = (s.ts, s.ts + s.interval);
    let (lo, hi) = (from - cycles * s.interval, to + cycles * s.interval);
    live.iter()
        .enumerate()
        .filter(|(i, l)| !used[*i] && (lo..hi).contains(&l.ts) && ok(l))
        .min_by_key(|(_, l)| distance(l.ts, from, to))
        .map(|(i, _)| i)
}

/// Pairs `shadow` with `live`; `in_scope(t)` says a live time falls inside a
/// joined record's window. `fallback` false gives the identity-only numbers.
pub fn join(
    shadow: &[Event],
    live: &[Event],
    in_scope: &dyn Fn(i64) -> bool,
    fallback: bool,
) -> Joined {
    let mut used = vec![false; live.len()];
    let mut paired = vec![false; shadow.len()];
    let mut out = Joined::default();
    for (n, s) in shadow.iter().enumerate() {
        let Some(k) = &s.key else { continue };
        let same = |l: &Event| l.key.as_ref() == Some(k);
        if let Some(i) = nearest(s, live, &used, IDENTITY_CYCLES, &same) {
            (used[i], paired[n]) = (true, true);
            out.by_identity += 1;
        }
    }
    if fallback {
        for (n, s) in shadow.iter().enumerate() {
            if paired[n] {
                continue;
            }
            let fits = |l: &Event| match (&s.sk, &l.sk) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            };
            if let Some(i) = nearest(s, live, &used, FALLBACK_CYCLES, &fits) {
                (used[i], paired[n]) = (true, true);
                out.by_fallback += 1;
            }
        }
    }
    out.counts = Counts {
        matched: out.by_identity + out.by_fallback,
        scheduler_only: paired.iter().filter(|p| !**p).count() as u64,
        poller_only: live
            .iter()
            .zip(&used)
            .filter(|(l, u)| !**u && in_scope(l.ts))
            .count() as u64,
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str, k: &str, f: &str) -> EventKey {
        EventKey {
            session: s.to_owned(),
            kind: k.to_owned(),
            fingerprint: f.to_owned(),
        }
    }

    #[test]
    fn join_pairs_identity_across_phase_skew() {
        let shadow = [Event::keyed(1000, 60, key("tm-a", "pm_message", "f1"))];
        // The live poller raised it five cycles later: outside the fallback.
        let live = [Event::keyed(1300, 0, key("tm-a", "pm_message", "f1"))];
        let all = |_| true;
        let j = join(&shadow, &live, &all, true);
        assert_eq!((j.counts.matched, j.by_identity, j.by_fallback), (1, 1, 0));
        // A different fingerprint is no identity, and too far to fall back.
        let other = [Event::keyed(1300, 0, key("tm-a", "pm_message", "f2"))];
        let j = join(&shadow, &other, &all, true);
        assert_eq!((j.counts.scheduler_only, j.counts.poller_only), (1, 1));
    }

    #[test]
    fn join_falls_back_to_two_cycles_without_identity() {
        let anon = |ts| Event {
            ts,
            interval: 60,
            key: None,
            sk: Some(("tm-a".to_owned(), "idle".to_owned())),
        };
        let shadow = [anon(1000)];
        let all = |_| true;
        // One cycle early: the fallback pairs it, identity alone cannot.
        let live = [anon(950)];
        assert_eq!(join(&shadow, &live, &all, true).by_fallback, 1);
        assert_eq!(join(&shadow, &live, &all, false).counts.matched, 0);
        // Three cycles late: no pair; a live row outside scope is not counted.
        let late = [anon(1000 + 3 * 60 + 61)];
        let j = join(&shadow, &late, &|t| t < 1100, true);
        assert_eq!((j.counts.matched, j.counts.poller_only), (0, 0));
    }
}
