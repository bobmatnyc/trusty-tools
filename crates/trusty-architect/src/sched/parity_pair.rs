//! Pairing rules the parity join adds on top of exact identity (#11826,
//! parity window 6).
//!
//! Why: window 6 counted the same event twice as a mismatch when the two
//! pollers stamped a per-poller detail into it: the `session_exited` first-seen
//! time, the Python token watcher's `project:sid` label after a session exits,
//! or a mid-episode idle screen that only one poller sampled.
//! What: [`identity_fp`] drops the `@<time>` suffix of a `session_exited`
//! fingerprint; [`token_label_pair`] pairs two `token` keys whose
//! fingerprints are equal when exactly one session is the `<project>:<sid8>`
//! fallback label; [`idle_episodes`] folds one side's `idle` events of one
//! (session, since-epoch) that follow each other within [`fold_gap`] (a
//! transient screen), so a fold counts once, and keeps each fold's screen
//! hashes for the join's episode pass. The join reports the folded counts.
//! Test: `session_exited_identity_drops_the_first_seen_suffix`,
//! `token_label_pair_needs_one_fallback_label_and_equal_fingerprints`,
//! `idle_episodes_group_by_session_and_epoch`.

use std::collections::{BTreeSet, HashMap};

use super::parity_join::{Event, FALLBACK_CYCLES, idle_screen};
use super::shadow::EventKey;
use super::slot::parse_iso;

/// The kind whose fingerprint carries each poller's own first-seen time.
pub const SESSION_EXITED: &str = "session_exited";

/// The fingerprint the identity pass compares: a `session_exited`
/// fingerprint `<id>@<ISO time>` loses its `@<time>` part.
///
/// Why: `fleet-poll.py:1304,1312` fingerprints `f"{id}@{at}"` with `at` each
/// poller's own first-seen time, so the same exit never matched (window 6, E).
/// What: any other kind, or a suffix that is not an ISO time, is unchanged.
/// Test: `session_exited_identity_drops_the_first_seen_suffix`.
pub fn identity_fp(k: &EventKey) -> &str {
    if k.kind != SESSION_EXITED {
        return &k.fingerprint;
    }
    match k.fingerprint.rsplit_once('@') {
        Some((id, at)) if !id.is_empty() && parse_iso(at).is_some() => id,
        _ => &k.fingerprint,
    }
}

/// Two keys are the same event for the identity pass.
pub fn same_identity(a: &EventKey, b: &EventKey) -> bool {
    a.session == b.session && a.kind == b.kind && identity_fp(a) == identity_fp(b)
}

/// True for `token_watch.py`'s fallback label `<project>:<first 8 hex of sid>`.
fn is_fallback_label(session: &str) -> bool {
    session.rsplit_once(':').is_some_and(|(project, sid)| {
        !project.is_empty()
            && sid.len() == 8
            && sid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Whether two keyed `token` events are one event under two labels.
///
/// Why: Python `token_watch.py:278-280` labels a transcript from live tmux
/// names only, so a scan after the session exits says `<project>:<sid8>`;
/// the Rust port keeps the remembered name (window 6, F). Rust is right; the
/// report pairs them instead of changing either side's naming.
/// What: both `token`, fingerprints equal, sessions differ, and exactly one
/// session has the fallback-label shape.
/// Test: `token_label_pair_needs_one_fallback_label_and_equal_fingerprints`.
pub fn token_label_pair(a: &EventKey, b: &EventKey) -> bool {
    a.kind == "token"
        && b.kind == "token"
        && a.fingerprint == b.fingerprint
        && a.session != b.session
        && is_fallback_label(&a.session) != is_fallback_label(&b.session)
}

/// One side's idle episodes: for each event, the index of its episode's
/// representative (its earliest event) or `None` when it is no keyed `idle`
/// event; and, per representative, the episode's screen hashes.
pub struct Episodes {
    /// Per event: its episode's representative.
    pub rep: Vec<Option<usize>>,
    /// Per representative: the screen hashes seen in the episode.
    pub screens: HashMap<usize, BTreeSet<String>>,
}

impl Episodes {
    /// True when `i` is an idle event folded into an earlier one.
    pub fn absorbed(&self, i: usize) -> bool {
        self.rep[i].is_some_and(|r| r != i)
    }

    /// True when `i` represents an idle episode.
    pub fn is_rep(&self, i: usize) -> bool {
        self.rep[i] == Some(i)
    }
}

/// The poll interval assumed for a live event, which carries none (the
/// live poller runs every 60 s, `sched/config.rs` `default_poll_every`).
pub const LIVE_INTERVAL_S: i64 = 60;

/// Most seconds between an idle event and its episode's previous event for
/// the two to fold: [`FALLBACK_CYCLES`] intervals of the event.
pub fn fold_gap(e: &Event) -> i64 {
    let interval = if e.interval > 0 {
        e.interval
    } else {
        LIVE_INTERVAL_S
    };
    FALLBACK_CYCLES * interval
}

/// Groups `events` into idle episodes by (session, since-epoch), each a
/// chain of events no more than [`fold_gap`] apart.
///
/// Why: the idle fingerprint is `since-<epoch>-<screen>`, so a screen change
/// mid-episode is a new alert; the pollers sample ~40 s apart and see
/// different transient screens (window 6, D). Only a transient screen folds:
/// a re-alert minutes later in the same epoch is its own event, so a dedupe
/// regression still shows as a mismatch (#11826 review).
/// What: per (session, epoch), events in time order (ties: input order); an
/// event within [`fold_gap`] of the previous one joins its chain, else it
/// starts a new one. The representative is the chain's first event; every
/// screen hash of the chain is kept.
/// Test: `idle_episodes_group_by_session_and_epoch`,
/// `window6_idle_re_alert_after_17_minutes_is_its_own_event`.
pub fn idle_episodes(events: &[Event]) -> Episodes {
    let mut groups: HashMap<(String, i64), Vec<usize>> = HashMap::new();
    let mut hashes = vec![None; events.len()];
    for (i, e) in events.iter().enumerate() {
        let Some(((session, _, hash), epoch)) = e.key.as_ref().and_then(idle_screen) else {
            continue;
        };
        hashes[i] = Some(hash.to_owned());
        groups
            .entry((session.to_owned(), epoch))
            .or_default()
            .push(i);
    }
    let mut rep = vec![None; events.len()];
    let mut screens: HashMap<usize, BTreeSet<String>> = HashMap::new();
    for mut members in groups.into_values() {
        members.sort_by_key(|&i| (events[i].ts, i));
        let mut head = members[0];
        let mut prev = head;
        for i in members {
            if events[i].ts - events[prev].ts > fold_gap(&events[i]) {
                head = i;
            }
            rep[i] = Some(head);
            if let Some(h) = hashes[i].take() {
                screens.entry(head).or_default().insert(h);
            }
            prev = i;
        }
    }
    Episodes { rep, screens }
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
    fn session_exited_identity_drops_the_first_seen_suffix() {
        let shadow = key("", "session_exited", "S19@2026-11-24T23:59:44Z");
        let live = key("", "session_exited", "S19@2026-11-24T23:59:01Z");
        assert_eq!(identity_fp(&shadow), "S19");
        assert!(same_identity(&shadow, &live));
        // Another exit id, another kind, or a suffix that is no time: unchanged.
        let other = key("", "session_exited", "S20@2026-11-24T23:59:01Z");
        assert!(!same_identity(&shadow, &other));
        let meta = key("", "meta_due", "M0c@2026-11-24T23:56:38Z");
        assert_eq!(identity_fp(&meta), "M0c@2026-11-24T23:56:38Z");
        let odd = key("", "session_exited", "S19@later");
        assert_eq!(identity_fp(&odd), "S19@later");
    }

    #[test]
    fn token_label_pair_needs_one_fallback_label_and_equal_fingerprints() {
        let named = key("tm-proj-04", "token", "ctx_main:350k");
        let label = key("proj-01:0001abcd", "token", "ctx_main:350k");
        assert!(token_label_pair(&named, &label) && token_label_pair(&label, &named));
        let other_fp = key("proj-01:0001abcd", "token", "ctx_main:200k");
        assert!(!token_label_pair(&named, &other_fp));
        let two_names = key("tm-other", "token", "ctx_main:350k");
        assert!(!token_label_pair(&named, &two_names));
        let not_hex = key("mcp:tm-ta-0001", "token", "ctx_main:350k");
        assert!(!token_label_pair(&named, &not_hex));
        let idle = key("proj-01:0001abcd", "idle", "ctx_main:350k");
        assert!(!token_label_pair(&named, &idle));
    }

    #[test]
    fn idle_episodes_group_by_session_and_epoch() {
        let ev = |ts, s: &str, fp: &str| Event::keyed(ts, 60, key(s, "idle", fp));
        let events = [
            ev(200, "tm-a", "since-100-aaaa"),
            ev(150, "tm-a", "since-100-bbbb"),
            ev(300, "tm-a", "since-900-aaaa"),
            ev(160, "tm-b", "since-100-aaaa"),
            Event::keyed(170, 60, key("tm-a", "token", "since-100-aaaa")),
            // Same episode, 17 minutes after the last: its own event.
            ev(200 + 17 * 60, "tm-a", "since-100-cccc"),
        ];
        let ep = idle_episodes(&events);
        assert_eq!(ep.rep, [Some(1), Some(1), Some(2), Some(3), None, Some(5)]);
        assert!(ep.absorbed(0) && ep.is_rep(1) && !ep.absorbed(4) && ep.is_rep(5));
        let screens: Vec<&str> = ep.screens[&1].iter().map(String::as_str).collect();
        assert_eq!(screens, ["aaaa", "bbbb"]);
        // A live event (no interval) folds within two 60 s polls only.
        assert_eq!(fold_gap(&Event::keyed(0, 0, key("tm-a", "idle", "x"))), 120);
    }
}
