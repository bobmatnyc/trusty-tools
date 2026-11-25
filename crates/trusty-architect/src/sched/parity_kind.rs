//! `parity --by-kind`: the join's result per alert kind, with examples
//! (#11826, parity window 6).
//!
//! Why: the window-6 breakdown needed a Python port of the join to see which
//! kinds failed; the report itself printed only totals.
//! What: [`tally`] reads each event's [`Fate`] from a [`Joined`] and counts
//! matched (shadow side), scheduler-only and poller-only (live side, inside a
//! joined record's window) per kind, keeping every unmatched row as an
//! [`Unmatched`], and the folded `idle` events per side. [`render`] prints
//! one line per kind (with its folded counts, when any), the most mismatched
//! first, and up to `examples` unmatched rows under each.
//! Test: `by_kind_counts_each_kind_and_lists_examples`,
//! `parity_by_kind_flag_prints_per_kind_rows_and_examples`.

use std::collections::BTreeMap;

use super::parity::Counts;
use super::parity_join::{Event, Fate, Joined};
use crate::classify::alerts::iso;

/// One row only one side has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmatched {
    /// True for a live (poller-only) row, false for a scheduler-only one.
    pub poller: bool,
    /// Unix seconds: the record's slot or the live row's time.
    pub ts: i64,
    /// Session, empty when unknown.
    pub session: String,
    /// Kind, `?` when unknown.
    pub kind: String,
    /// Fingerprint, when the row has one.
    pub fingerprint: Option<String>,
}

/// Per-kind counts and the unmatched rows of one join.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ByKind {
    /// Counts by kind.
    pub kinds: BTreeMap<String, Counts>,
    /// Every unmatched row, scheduler-only first, each side in input order.
    pub unmatched: Vec<Unmatched>,
    /// Folded (absorbed) events by kind: (shadow, live in scope).
    pub folded: BTreeMap<String, (u64, u64)>,
}

fn row(e: &Event, poller: bool) -> Unmatched {
    let (session, kind) = match (&e.sk, &e.key) {
        (Some((s, k)), _) => (s.clone(), k.clone()),
        (None, Some(k)) => (k.session.clone(), k.kind.clone()),
        (None, None) => (String::new(), "?".to_owned()),
    };
    Unmatched {
        poller,
        ts: e.ts,
        session,
        kind,
        fingerprint: e.key.as_ref().map(|k| k.fingerprint.clone()),
    }
}

/// Counts `j`'s fates per kind; see the module doc.
pub fn tally(
    shadow: &[Event],
    live: &[Event],
    j: &Joined,
    in_scope: &dyn Fn(i64) -> bool,
) -> ByKind {
    let mut out = ByKind::default();
    for (e, fate) in shadow.iter().zip(&j.shadow) {
        let r = row(e, false);
        let c = out.kinds.entry(r.kind.clone()).or_default();
        match fate {
            Fate::Matched => c.matched += 1,
            Fate::Unmatched => {
                c.scheduler_only += 1;
                out.unmatched.push(r);
            }
            Fate::Absorbed => out.folded.entry(r.kind).or_default().0 += 1,
        }
    }
    for (e, fate) in live.iter().zip(&j.live) {
        if *fate == Fate::Matched || !in_scope(e.ts) {
            continue;
        }
        let r = row(e, true);
        if *fate == Fate::Absorbed {
            out.kinds.entry(r.kind.clone()).or_default();
            out.folded.entry(r.kind).or_default().1 += 1;
            continue;
        }
        out.kinds.entry(r.kind.clone()).or_default().poller_only += 1;
        out.unmatched.push(r);
    }
    out
}

/// The per-kind section for `what` (`alerts` or `wakes`): one line per kind,
/// most mismatched first, then up to `examples` unmatched rows per kind.
pub fn render(what: &str, b: &ByKind, examples: usize) -> String {
    let mut kinds: Vec<(&String, &Counts)> = b.kinds.iter().collect();
    kinds.sort_by_key(|(k, c)| (std::cmp::Reverse(c.scheduler_only + c.poller_only), *k));
    let mut s = format!("{what} by kind: matched / scheduler-only / poller-only\n");
    for (kind, c) in kinds {
        let folded = match b.folded.get(kind) {
            Some((sh, li)) => format!(" (folded: shadow {sh}, live {li})"),
            None => String::new(),
        };
        s += &format!(
            "  {kind}: {} / {} / {}{folded}\n",
            c.matched, c.scheduler_only, c.poller_only
        );
        let rows = b
            .unmatched
            .iter()
            .filter(|u| &u.kind == kind)
            .take(examples);
        for u in rows {
            let side = if u.poller {
                "poller-only"
            } else {
                "scheduler-only"
            };
            let fp = u.fingerprint.as_deref().unwrap_or("-");
            let session = if u.session.is_empty() {
                "-"
            } else {
                &u.session
            };
            s += &format!("    {side} {} {session} {fp}\n", iso(u.ts as f64));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sched::parity_join::join;
    use crate::sched::shadow::EventKey;

    fn ev(ts: i64, iv: i64, s: &str, k: &str, f: &str) -> Event {
        Event::keyed(
            ts,
            iv,
            EventKey {
                session: s.to_owned(),
                kind: k.to_owned(),
                fingerprint: f.to_owned(),
            },
        )
    }

    #[test]
    fn by_kind_counts_each_kind_and_lists_examples() {
        let shadow = [
            ev(1000, 60, "tm-a", "token", "hit_low:1"),
            ev(1000, 60, "tm-a", "ctx", "31"),
        ];
        let live = [
            ev(1010, 0, "tm-a", "ctx", "31"),
            ev(1020, 0, "", "meta_due", "M0c"),
        ];
        let all = |_| true;
        let j = join(&shadow, &live, &all, true);
        let b = tally(&shadow, &live, &j, &all);
        let c = |m, s, p| Counts {
            matched: m,
            scheduler_only: s,
            poller_only: p,
        };
        assert_eq!(b.kinds["token"], c(0, 1, 0));
        assert_eq!(b.kinds["ctx"], c(1, 0, 0));
        assert_eq!(b.kinds["meta_due"], c(0, 0, 1));
        let text = render("alerts", &b, 1);
        assert!(text.starts_with("alerts by kind: matched / scheduler-only / poller-only\n"));
        assert!(
            text.contains("  meta_due: 0 / 0 / 1\n    poller-only 1970-01-01T00:17:00Z - M0c\n"),
            "{text}"
        );
        assert!(text.contains("  ctx: 1 / 0 / 0\n"), "{text}");
        // No examples asked: the counts only.
        assert!(!render("alerts", &b, 0).contains("scheduler-only 1970"));
        // A transient idle screen folds and is counted per kind, per side.
        let shadow = [ev(1000, 60, "tm-a", "idle", "since-1-aaaa")];
        let live = [
            ev(1010, 0, "tm-a", "idle", "since-1-aaaa"),
            ev(1070, 0, "tm-a", "idle", "since-1-bbbb"),
        ];
        let j = join(&shadow, &live, &all, true);
        let b = tally(&shadow, &live, &j, &all);
        assert_eq!((b.kinds["idle"], b.folded["idle"]), (c(1, 0, 0), (0, 1)));
        assert!(render("alerts", &b, 0).contains("  idle: 1 / 0 / 0 (folded: shadow 0, live 1)\n"));
    }
}
