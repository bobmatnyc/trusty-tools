//! `SessionStart` cwd→session correlation, and the #3764 item-2 corruption
//! alarm for a colliding cwd.
//!
//! Why: `correlate_session_start` (`daemon/api.rs`) has always treated "≥2
//! Active managed session records resolve to the same cwd" as a mere
//! *attribution* problem — it skipped persisting `claude_session_id` and logged
//! a `warn!`, then carried on as if nothing were wrong. It is not an
//! attribution problem. Two Active records pointing at ONE worktree means the
//! registry is describing a state that cannot physically exist, and it is the
//! precursor the #3715 worktree-destruction forensics found sitting on the
//! exact path that was later wiped: a 3-way collision at 01:58–02:16Z, hours
//! before the corruption began, that passed almost silently. Any decommission,
//! reap, or GC of the wrong one of those records destroys a LIVE session's
//! tree (which is what the #3764 item-1 guard now refuses).
//!
//! The escalation from `warn!` to `error!` is the whole point and is NOT
//! cosmetic: the daemon composes `trusty_common::error_capture::bug_capture_layer`
//! (`bin/tm/main.rs`), which persists ERROR-level events to
//! `<data_dir>/trusty-mpm/errors.jsonl` and surfaces them through the
//! `list_recent_errors` MCP tool, `preview_bug_report`, and `tm doctor`.
//! `warn!` is captured by NONE of those. At WARN this condition was invisible
//! to every operator-facing surface; at ERROR it becomes a durable, greppable,
//! queryable record the moment it first occurs.
//!
//! What: [`CwdCorrelation`] (the pure verdict), [`classify`] (the pure
//! classifier — the testable core), and [`alarm`] (the loud rendering of a
//! [`CwdCorrelation::Collision`]).
//! Test: `classify_*` and `alarm_is_error_level` below; the wired-in behaviour is
//! covered by `session_start_hook_correlates_claude_id` in `api_tests.rs`.

use crate::session_manager::record::ManagedSessionId;

/// What a `SessionStart` hook's cwd resolved to among the Active records.
///
/// Why: making the three outcomes an explicit enum — instead of a bare
/// `match matched.len()` buried in a 2 000-line HTTP module — is what makes the
/// corruption case testable at all, and what stops a future edit from quietly
/// folding [`Self::Collision`] back into "nothing to do".
/// What: [`None`](Self::None) — no Active record at this cwd (ordinary; an
/// unmanaged shell). [`Unique`](Self::Unique) — exactly one; attribute to it.
/// [`Collision`](Self::Collision) — TWO OR MORE, which is registry corruption.
/// Test: `classify_no_match_is_none`, `classify_single_match_is_unique`,
/// `classify_two_matches_is_collision`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CwdCorrelation {
    /// No Active managed session is running at this cwd.
    None,
    /// Exactly one Active managed session — safe to attribute.
    Unique(ManagedSessionId),
    /// ≥2 Active records share this cwd: physically impossible, corruption.
    Collision(Vec<ManagedSessionId>),
}

/// Classify the Active records that matched a `SessionStart` hook's cwd.
///
/// Why: the pure half, so the corruption verdict can be asserted directly
/// rather than by scraping log output. Pre-#3764 this logic existed only as an
/// inline `match matched.len() { 0 => {}, 1 => ..., n => warn!(...) }`, whose
/// `n` arm was indistinguishable from the `0` arm in effect — both did nothing.
/// What: `[]` → [`CwdCorrelation::None`]; `[id]` → [`CwdCorrelation::Unique`];
/// anything longer → [`CwdCorrelation::Collision`] carrying every colliding id
/// so the alarm can name them (the pre-#3764 warn logged only a COUNT, which
/// gave an operator nothing to act on).
/// Test: `classify_no_match_is_none`, `classify_single_match_is_unique`,
/// `classify_two_matches_is_collision`, `classify_three_matches_lists_all_ids`.
pub(crate) fn classify(matched: &[ManagedSessionId]) -> CwdCorrelation {
    match matched {
        [] => CwdCorrelation::None,
        [only] => CwdCorrelation::Unique(*only),
        many => CwdCorrelation::Collision(many.to_vec()),
    }
}

/// Emit the error-level corruption alarm for a colliding cwd (#3764 item 2).
///
/// Why: see the module doc — ERROR is the only level the daemon's bug-capture
/// layer persists, so this level choice is what makes the condition visible to
/// `list_recent_errors` / `tm doctor` instead of dying in a log file nobody
/// greps. Mirrors the #3715 F3 streak alarm in
/// `session_manager::prune`, which escalates the same way for the same reason.
/// What: one `error!` naming the cwd, the collision count, and EVERY colliding
/// session id, plus the concrete next action. No side effects beyond the log.
/// Test: `alarm_is_error_level` (pins the level via `tracing::Level`, and that
/// every colliding id appears in the `sessions` field).
pub(crate) fn alarm(cwd: &str, ids: &[ManagedSessionId]) {
    let rendered: Vec<String> = ids.iter().map(ToString::to_string).collect();
    tracing::error!(
        cwd = %cwd,
        collisions = ids.len(),
        sessions = %rendered.join(","),
        "SESSION REGISTRY CORRUPTION: {} Active managed session records resolve to the \
         SAME workspace path — this state cannot physically exist and is the observed \
         precursor to cross-session worktree destruction. claude_session_id attribution \
         is skipped. Reconcile these records NOW (`tm ls`, `tm sessions delete <stale-id>`) \
         before any decommission/reap touches that path (#3764, precursor to #3715)",
        ids.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_no_match_is_none() {
        assert_eq!(classify(&[]), CwdCorrelation::None);
    }

    #[test]
    fn classify_single_match_is_unique() {
        let id = ManagedSessionId::new();
        assert_eq!(classify(&[id]), CwdCorrelation::Unique(id));
    }

    /// TWO Active records at one cwd is a Collision, not a benign skip.
    ///
    /// Why: this is the assertion that fails against pre-#3764 `main`, where
    /// the `n` arm produced no distinguishable outcome at all.
    /// Test: this function IS the test.
    #[test]
    fn classify_two_matches_is_collision() {
        let a = ManagedSessionId::new();
        let b = ManagedSessionId::new();
        assert_eq!(classify(&[a, b]), CwdCorrelation::Collision(vec![a, b]));
    }

    /// The 3-way collision actually observed before the #3715 corruption is
    /// reported with ALL THREE ids, not just a count.
    ///
    /// Why: the pre-#3764 warn logged `n` only. An operator who saw it had no
    /// way to know WHICH records to reconcile, so the log was unactionable
    /// even in the one case where somebody read it.
    /// Test: this function IS the test.
    #[test]
    fn classify_three_matches_lists_all_ids() {
        let ids = [
            ManagedSessionId::new(),
            ManagedSessionId::new(),
            ManagedSessionId::new(),
        ];
        match classify(&ids) {
            CwdCorrelation::Collision(reported) => {
                assert_eq!(
                    reported,
                    ids.to_vec(),
                    "every colliding id must be reported"
                );
            }
            other => panic!("3 Active records at one cwd must be a Collision, got {other:?}"),
        }
    }

    /// The alarm is emitted at ERROR, the only level the daemon's bug-capture
    /// layer persists to `errors.jsonl` / `list_recent_errors`.
    ///
    /// Why: this is the "fail LOUD, never silent" invariant itself. A future
    /// edit softening this back to `warn!` would restore the exact silence
    /// that let a 3-way collision sit unnoticed for hours before the worktree
    /// was destroyed — so the level is asserted, not assumed.
    /// What: installs a subscriber that records the level of every event, runs
    /// [`alarm`], and asserts an ERROR was emitted carrying the colliding ids.
    /// Test: this function IS the test.
    #[test]
    fn alarm_is_error_level() {
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};

        #[derive(Default)]
        struct Captured {
            levels: Vec<tracing::Level>,
            sessions: Vec<String>,
        }

        struct Collector(Arc<Mutex<Captured>>);

        // `tracing`'s `%` sigil records through `record_debug` (the value is
        // wrapped in `field::display`), never `record_str` — so both are
        // implemented here rather than assuming one.
        struct FieldGrab<'a>(&'a mut Vec<String>);
        impl Visit for FieldGrab<'_> {
            fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
                if f.name() == "sessions" {
                    self.0.push(format!("{v:?}"));
                }
            }
            fn record_str(&mut self, f: &Field, v: &str) {
                if f.name() == "sessions" {
                    self.0.push(v.to_string());
                }
            }
        }

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Collector {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut cap = self.0.lock().expect("lock");
                cap.levels.push(*event.metadata().level());
                let mut grab = FieldGrab(&mut cap.sessions);
                event.record(&mut grab);
            }
        }

        let captured = Arc::new(Mutex::new(Captured::default()));
        let subscriber = {
            use tracing_subscriber::layer::SubscriberExt as _;
            tracing_subscriber::registry().with(Collector(Arc::clone(&captured)))
        };

        let a = ManagedSessionId::new();
        let b = ManagedSessionId::new();
        tracing::subscriber::with_default(subscriber, || {
            alarm("/tmp/contested-worktree", &[a, b]);
        });

        let cap = captured.lock().expect("lock");
        assert!(
            cap.levels.contains(&tracing::Level::ERROR),
            "the cwd-collision alarm MUST be ERROR (the only level the daemon's \
             bug-capture layer persists); observed levels: {:?}",
            cap.levels
        );
        let joined = cap.sessions.join(" ");
        assert!(
            joined.contains(&a.to_string()) && joined.contains(&b.to_string()),
            "the alarm must name every colliding session id; got {joined:?}"
        );
    }
}
