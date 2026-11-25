//! The parity join's matcher: event identity first, then a time tolerance
//! (MB1595 fix round, class A).
//!
//! Why: the 2nd parity run joined each shadow record to live rows only
//! inside `[slot, slot + interval)`; a live alert raised one cycle earlier or
//! later (phase skew between the two pollers) counted twice as a mismatch.
//! What: [`join`] pairs shadow events with live events. Pass 1 pairs equal
//! identities (session, kind, fingerprint: the dedupe key; a PM message's id
//! and an owner item's id are their fingerprints) within
//! [`IDENTITY_CYCLES`] cycles of the record (a `session_exited` fingerprint
//! without its `@<time>` suffix, #11826); pass 1b pairs `idle` per episode:
//! each side's idle events of one (session, since-epoch) that follow each
//! other within two polls fold into the first (`parity_pair::idle_episodes`,
//! #11826; the others are [`Fate::Absorbed`], counted in `folded`),
//! and two episodes pair when they share a screen hash ([`idle_screen`]: the
//! pollers keep separate idle timers, so the epochs differ) and the epochs
//! are within [`IDLE_EPOCH_TOLERANCE_S`]; pass 2, when asked, pairs what
//! is left by session and kind, only when at least one side has no identity
//! (two keyed events never pair, except one `token` event under the Python
//! `project:sid` label, #11826) within [`FALLBACK_CYCLES`] cycles. Each pass
//! takes the nearest unused live event. A shadow event left over is
//! `scheduler_only`; a live event left over counts as `poller_only` only when
//! it falls inside some joined record's own window. The excluded kinds are
//! dropped before the join, in `parity::compute_full`.
//! Test: `join_pairs_identity_across_phase_skew`,
//! `idle_pairs_on_screen_hash_across_epoch_skew`,
//! `idle_with_different_screen_hashes_never_pairs`,
//! `idle_episodes_beyond_the_epoch_tolerance_never_pair`,
//! `join_falls_back_to_two_cycles_without_identity`,
//! `fallback_never_pairs_two_keyed_events_with_different_fingerprints`,
//! `parity_reports_identity_and_fallback_numbers`.

use std::collections::BTreeSet;

use super::parity::Counts;
use super::parity_pair::{idle_episodes, same_identity, token_label_pair};
use super::shadow::EventKey;

/// Identity pairs within this many cycles of the record: below the shortest
/// re-emit (30 cycles at 60 s), so a re-emit never pairs with the first.
pub const IDENTITY_CYCLES: i64 = 15;
/// The fallback's tolerance, in cycles, on each side of the record window.
pub const FALLBACK_CYCLES: i64 = 2;
/// Most seconds two `idle` epochs of one screen may differ and still pair.
///
/// Why: the pollers keep separate idle timers, so one episode carries two
/// epochs; two episodes of one screen must still not pair (MB1595 review).
/// What: measured skew, same session and screen, all logs to 2026-11-23: at
/// most 189 s (window 4: 3, 28, 98, 189 s). The shortest gap between two
/// live idle episodes of one session, any screen, is 820 s, and an idle alert
/// needs 600 s of idle. 300 s sits between both.
/// Test: `idle_episodes_beyond_the_epoch_tolerance_never_pair`,
/// `idle_pairs_on_screen_hash_across_epoch_skew`.
pub const IDLE_EPOCH_TOLERANCE_S: i64 = 300;
/// The kind left out of parity: `il-cron.py` only prints `would run` in a dry
/// run (il-cron.py:249), so the shadow can never raise it.
pub const IL_CRON: &str = "il_cron";
/// Left out too: both pollers debounce on the one `ctx-watch-state.json`.
pub const CTXWATCH: &str = "ctxwatch";

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

/// What became of one event in [`join`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Paired with an event on the other side.
    Matched,
    /// Left over: scheduler-only or poller-only.
    Unmatched,
    /// An `idle` event folded into its episode's first event (#11826).
    Absorbed,
}

/// What [`join`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Joined {
    /// Matched (both passes), scheduler-only and poller-only.
    pub counts: Counts,
    /// Pairs made by identity.
    pub by_identity: u64,
    /// Pairs made by the fallback.
    pub by_fallback: u64,
    /// Of `by_identity`, the `idle` episode pairs whose first keys differ.
    pub by_relaxed: u64,
    /// Per shadow event, in input order.
    pub shadow: Vec<Fate>,
    /// Per live event, in input order.
    pub live: Vec<Fate>,
    /// [`Fate::Absorbed`] events: shadow, and live inside a record's window.
    pub folded: (u64, u64),
}

/// An `idle` key's (session, kind, screen hash) and its since-epoch;
/// `None` for any other kind or fingerprint shape.
/// Test: `idle_with_different_screen_hashes_never_pairs`,
/// `idle_episodes_beyond_the_epoch_tolerance_never_pair`.
pub fn idle_screen(k: &EventKey) -> Option<((&str, &str, &str), i64)> {
    let rest = k.fingerprint.strip_prefix("since-")?;
    let (epoch, hash) = rest.split_once('-')?;
    let numeric = !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit());
    let epoch: i64 = epoch.parse().ok().filter(|_| numeric)?;
    (k.kind == "idle").then_some(((&k.session, &k.kind, hash), epoch))
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
    ok: &dyn Fn(usize, &Event) -> bool,
) -> Option<usize> {
    let (from, to) = (s.ts, s.ts + s.interval);
    let (lo, hi) = (from - cycles * s.interval, to + cycles * s.interval);
    live.iter()
        .enumerate()
        .filter(|(i, l)| !used[*i] && (lo..hi).contains(&l.ts) && ok(*i, l))
        .min_by_key(|(_, l)| distance(l.ts, from, to))
        .map(|(i, _)| i)
}

/// An event's (session, kind): its `sk`, else the one its identity key carries.
fn session_kind(e: &Event) -> Option<(&str, &str)> {
    match (&e.sk, &e.key) {
        (Some((s, k)), _) => Some((s, k)),
        (None, Some(k)) => Some((&k.session, &k.kind)),
        (None, None) => None,
    }
}

/// Whether the fallback may pair `s` with `l`.
///
/// Why: a keyed shadow event whose identity pass failed paired with any keyed
/// live event of the same session and kind, so `token` `ctx_main` matched
/// `token` `hit_low` and inflated the match rate.
/// What: two keyed events never pair here (their identities differ, or pass 1
/// would have paired them); a legacy event without a key pairs when its
/// (session, kind) equals the other's. A side with neither `sk` nor a key
/// knows nothing and pairs with anything; once both sides know a (session,
/// kind) they must be equal, so a known kind never pairs across kinds.
/// Test: `fallback_never_pairs_two_keyed_events_with_different_fingerprints`,
/// `fallback_pairs_keyed_with_legacy_unkeyed_event`,
/// `fallback_never_pairs_different_known_kinds`.
fn fallback_fits(s: &Event, l: &Event) -> bool {
    if let (Some(a), Some(b)) = (&s.key, &l.key) {
        // #11826: one token event under the Python fallback label.
        return token_label_pair(a, b);
    }
    match (session_kind(s), session_kind(l)) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Whether two idle episode heads are one episode: same session, epochs
/// within [`IDLE_EPOCH_TOLERANCE_S`], and a screen hash in common.
fn same_episode(
    s: &Event,
    l: &Event,
    ss: Option<&BTreeSet<String>>,
    ls: Option<&BTreeSet<String>>,
) -> bool {
    let (Some((want, epoch)), Some((got, e))) = (
        s.key.as_ref().and_then(idle_screen),
        l.key.as_ref().and_then(idle_screen),
    ) else {
        return false;
    };
    let shared = ss.zip(ls).is_some_and(|(a, b)| !a.is_disjoint(b));
    want.0 == got.0 && (e - epoch).abs() <= IDLE_EPOCH_TOLERANCE_S && shared
}

/// Pairs `shadow` with `live`; `in_scope(t)` says a live time falls inside a
/// joined record's window. `fallback` false gives the identity-only numbers.
pub fn join(
    shadow: &[Event],
    live: &[Event],
    in_scope: &dyn Fn(i64) -> bool,
    fallback: bool,
) -> Joined {
    // #11826: an idle episode counts once per side, at its first event.
    let (es, el) = (idle_episodes(shadow), idle_episodes(live));
    let mut used: Vec<bool> = (0..live.len()).map(|i| el.absorbed(i)).collect();
    let mut paired: Vec<bool> = (0..shadow.len()).map(|n| es.absorbed(n)).collect();
    let mut out = Joined::default();
    for (n, s) in shadow.iter().enumerate() {
        let Some(k) = s.key.as_ref().filter(|_| es.rep[n].is_none()) else {
            continue;
        };
        // #11826: session_exited compares without its first-seen time.
        let same = |_: usize, l: &Event| l.key.as_ref().is_some_and(|lk| same_identity(k, lk));
        if let Some(i) = nearest(s, live, &used, IDENTITY_CYCLES, &same) {
            (used[i], paired[n]) = (true, true);
            out.by_identity += 1;
        }
    }
    // Pass 1b, MB1595 window 4 and #11826: idle pairs per episode, on a
    // shared screen hash, epochs within the tolerance.
    for (n, s) in shadow.iter().enumerate() {
        if !es.is_rep(n) || paired[n] {
            continue;
        }
        let ss = es.screens.get(&n);
        let same = |i: usize, l: &Event| el.is_rep(i) && same_episode(s, l, ss, el.screens.get(&i));
        if let Some(i) = nearest(s, live, &used, IDENTITY_CYCLES, &same) {
            (used[i], paired[n]) = (true, true);
            out.by_identity += 1;
            out.by_relaxed += u64::from(s.key != live[i].key);
        }
    }
    if fallback {
        for (n, s) in shadow.iter().enumerate() {
            if paired[n] {
                continue;
            }
            let fits = |_: usize, l: &Event| fallback_fits(s, l);
            if let Some(i) = nearest(s, live, &used, FALLBACK_CYCLES, &fits) {
                (used[i], paired[n]) = (true, true);
                out.by_fallback += 1;
            }
        }
    }
    let fate = |absorbed: bool, hit: bool| match (absorbed, hit) {
        (true, _) => Fate::Absorbed,
        (false, true) => Fate::Matched,
        (false, false) => Fate::Unmatched,
    };
    out.shadow = (0..shadow.len())
        .map(|n| fate(es.absorbed(n), paired[n]))
        .collect();
    out.live = (0..live.len())
        .map(|i| fate(el.absorbed(i), used[i]))
        .collect();
    // #11826 review: the fold is reported, never silent.
    out.folded = (
        out.shadow.iter().filter(|f| **f == Fate::Absorbed).count() as u64,
        live.iter()
            .zip(&out.live)
            .filter(|(l, f)| **f == Fate::Absorbed && in_scope(l.ts))
            .count() as u64,
    );
    out.counts = Counts {
        matched: out.by_identity + out.by_fallback,
        scheduler_only: out.shadow.iter().filter(|f| **f == Fate::Unmatched).count() as u64,
        poller_only: live
            .iter()
            .zip(&out.live)
            .filter(|(l, f)| **f == Fate::Unmatched && in_scope(l.ts))
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

    #[test]
    fn fallback_never_pairs_two_keyed_events_with_different_fingerprints() {
        let shadow = [Event::keyed(
            1000,
            60,
            key("tm-x", "token", "ctx_main:350k"),
        )];
        // One cycle later, same session and kind, different fingerprint.
        let live = [Event::keyed(1060, 0, key("tm-x", "token", "hit_low:1"))];
        let j = join(&shadow, &live, &|_| true, true);
        assert_eq!((j.counts.matched, j.by_fallback), (0, 0));
        assert_eq!((j.counts.scheduler_only, j.counts.poller_only), (1, 1));
    }

    #[test]
    fn fallback_pairs_keyed_with_legacy_unkeyed_event() {
        let sk = Some(("tm-x".to_owned(), "token".to_owned()));
        let legacy = Event {
            ts: 1060,
            interval: 0,
            key: None,
            sk,
        };
        let shadow = [Event::keyed(
            1000,
            60,
            key("tm-x", "token", "ctx_main:350k"),
        )];
        let j = join(&shadow, std::slice::from_ref(&legacy), &|_| true, true);
        assert_eq!((j.counts.matched, j.by_fallback), (1, 1));
        // Legacy shadow against a keyed live event pairs the same way.
        let shadow = [Event {
            ts: 1000,
            interval: 60,
            ..legacy
        }];
        let live = [Event::keyed(1060, 0, key("tm-x", "token", "hit_low:1"))];
        assert_eq!(join(&shadow, &live, &|_| true, true).by_fallback, 1);
    }

    /// Window 4: same session, kind and screen, idle-since 98 s apart.
    #[test]
    fn idle_pairs_on_screen_hash_across_epoch_skew() {
        let fp = |e: i64| format!("since-{e}-8e42f0ed439d");
        let shadow = [Event::keyed(1000, 60, key("tm-a", "idle", &fp(900)))];
        let live = [
            Event::keyed(1130, 0, key("tm-a", "idle", &fp(998))),
            Event::keyed(1190, 0, key("tm-a", "idle", &fp(998))),
        ];
        let j = join(&shadow, &live, &|_| true, false);
        assert_eq!((j.counts.matched, j.by_identity, j.by_relaxed), (1, 1, 1));
        // #11826: the second live idle of the same episode folds into the first.
        assert_eq!(j.counts.poller_only, 0);
        assert_eq!(j.live, [Fate::Matched, Fate::Absorbed]);
    }

    /// Two idle episodes of one screen, epochs 600 s apart: no pair.
    #[test]
    fn idle_episodes_beyond_the_epoch_tolerance_never_pair() {
        let ev = |ts, iv, fp: &str| Event::keyed(ts, iv, key("tm-a", "idle", fp));
        let shadow = [ev(1000, 60, "since-400-8e42f0ed439d")];
        let live = [ev(1060, 0, "since-1000-8e42f0ed439d")];
        let j = join(&shadow, &live, &|_| true, true);
        assert_eq!(j.counts.matched, 0);
        assert_eq!((j.counts.scheduler_only, j.counts.poller_only), (1, 1));
    }

    #[test]
    fn idle_with_different_screen_hashes_never_pairs() {
        let ev = |ts, iv, kind: &str, fp: &str| Event::keyed(ts, iv, key("tm-a", kind, fp));
        let shadow = [ev(1000, 60, "idle", "since-900-aaaa")];
        let live = [ev(1030, 0, "idle", "since-998-bbbb")];
        let j = join(&shadow, &live, &|_| true, true);
        assert_eq!((j.counts.matched, j.by_relaxed), (0, 0));
        assert_eq!((j.counts.scheduler_only, j.counts.poller_only), (1, 1));
        // A non-idle kind with the same shape keeps exact identity.
        let shadow = [ev(1000, 60, "token", "since-900-aaaa")];
        let live = [ev(1030, 0, "token", "since-998-aaaa")];
        assert_eq!(join(&shadow, &live, &|_| true, true).counts.matched, 0);
    }

    #[test]
    fn fallback_never_pairs_different_known_kinds() {
        let anon = |kind: &str, k: Option<EventKey>| Event {
            ts: 1000,
            interval: 60,
            key: k,
            sk: Some(("tm-x".to_owned(), kind.to_owned())),
        };
        let all = |_| true;
        let j = join(&[anon("idle", None)], &[anon("token", None)], &all, true);
        assert_eq!(j.counts.matched, 0);
        // The kind known only through the key still blocks a cross-kind pair.
        let mut keyed = anon("token", Some(key("tm-x", "token", "f")));
        keyed.sk = None;
        let j = join(&[keyed], &[anon("idle", None)], &all, true);
        assert_eq!(j.counts.matched, 0);
    }
}
