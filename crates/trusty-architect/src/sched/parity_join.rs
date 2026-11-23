//! The parity join's matcher: event identity first, then a time tolerance
//! (MB1595 fix round, class A).
//!
//! Why: the 2nd parity run joined each shadow record to live rows only
//! inside `[slot, slot + interval)`; a live alert raised one cycle earlier or
//! later (phase skew between the two pollers) counted twice as a mismatch.
//! What: [`join`] pairs shadow events with live events. Pass 1 pairs equal
//! identities (session, kind, fingerprint: the dedupe key; a PM message's id
//! and an owner item's id are their fingerprints) within
//! [`IDENTITY_CYCLES`] cycles of the record; pass 1b pairs what is left of
//! `idle` by [`idle_screen`] (session, kind, screen hash: the two pollers keep
//! separate idle timers, so the `since-<epoch>` part differs) in the same
//! window; pass 2, when asked, pairs what
//! is left by session and kind, only when at least one side has no identity
//! (two keyed events with different identities never pair) within [`FALLBACK_CYCLES`] cycles. Each pass takes the nearest unused live
//! event. A shadow event left over is `scheduler_only`; a live event left
//! over counts as `poller_only` only when it falls inside some joined
//! record's own window. [`IL_CRON`] is left out of both sides.
//! Test: `join_pairs_identity_across_phase_skew`,
//! `idle_pairs_on_screen_hash_across_epoch_skew`,
//! `idle_with_different_screen_hashes_never_pairs`,
//! `join_falls_back_to_two_cycles_without_identity`,
//! `fallback_never_pairs_two_keyed_events_with_different_fingerprints`,
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

/// What [`join`] found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Joined {
    /// Matched (both passes), scheduler-only and poller-only.
    pub counts: Counts,
    /// Pairs made by identity.
    pub by_identity: u64,
    /// Pairs made by the fallback.
    pub by_fallback: u64,
    /// Of `by_identity`, the `idle` pairs made on the relaxed key (pass 1b).
    pub by_relaxed: u64,
}

/// An `idle` key's (session, kind, screen hash), its `since-<epoch>-` prefix
/// dropped; `None` for any other kind or fingerprint shape.
pub fn idle_screen(k: &EventKey) -> Option<(&str, &str, &str)> {
    let rest = k.fingerprint.strip_prefix("since-")?;
    let (epoch, hash) = rest.split_once('-')?;
    let numeric = !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit());
    (k.kind == "idle" && numeric).then_some((&k.session, &k.kind, hash))
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
    if s.key.is_some() && l.key.is_some() {
        return false;
    }
    match (session_kind(s), session_kind(l)) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
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
    // Pass 1b, MB1595 window 4: idle pairs on the screen hash, epoch ignored.
    for (n, s) in shadow.iter().enumerate() {
        let Some(want) = s.key.as_ref().and_then(idle_screen) else {
            continue;
        };
        if paired[n] {
            continue;
        }
        let same = |l: &Event| l.key.as_ref().and_then(idle_screen) == Some(want);
        if let Some(i) = nearest(s, live, &used, IDENTITY_CYCLES, &same) {
            (used[i], paired[n]) = (true, true);
            out.by_identity += 1;
            out.by_relaxed += 1;
        }
    }
    if fallback {
        for (n, s) in shadow.iter().enumerate() {
            if paired[n] {
                continue;
            }
            let fits = |l: &Event| fallback_fits(s, l);
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
        // One to one: the second live idle of the same screen stays unpaired.
        assert_eq!(j.counts.poller_only, 1);
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
