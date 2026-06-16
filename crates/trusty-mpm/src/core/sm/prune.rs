//! Idle-session prune policy (issue #1313).
//!
//! Why: paused orchestration sessions leave behind idle SM tmux sessions that
//! consume claude Max rate-limit slots. The cleanup must honor the SM durability
//! lifecycle: a session classified `idle` is STOPPED (transient runtime killed,
//! workspace + record kept → resumable); a session classified `done` is fully
//! DECOMMISSIONED (stop + remove workspace + deregister). Every other verdict —
//! `working`, `blocked-on-permission`, `errored`, `unknown`, or no verdict at
//! all — is LEFT ALONE. Keeping that mapping as a pure function over the verdict
//! string makes the policy trivially testable without a live daemon and is the
//! single source of truth the CLI handler drives.
//! What: the [`PruneAction`] decision enum and the pure [`decide`] mapping from
//! an activity-monitor verdict string to an action, plus [`normalize_verdict`]
//! which canonicalizes the several on-the-wire spellings the daemon emits.
//! Test: `decide_idle_stops`, `decide_done_decommissions`,
//! `decide_working_skips`, `decide_blocked_skips`, `decide_errored_skips`,
//! `decide_no_verdict_skips`, `decide_unknown_skips`, plus the
//! `normalize_verdict_*` cases in the `tests` module below.

/// The teardown action the prune policy selects for one session.
///
/// Why: the prune command must map each session's latest activity verdict to
/// exactly one of three durability-respecting outcomes; an enum makes the
/// decision explicit, exhaustive, and renderable in both the dry-run plan and
/// the JSON output mode.
/// What: `Stop` → runtime-stop (resumable); `Decommission` → full teardown;
/// `Skip` → leave the session untouched (carries the reason for reporting).
/// Test: produced by [`decide`]; asserted across the `decide_*` tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneAction {
    /// Stop the runtime but keep the workspace + record (idle → resumable).
    Stop,
    /// Fully decommission: stop + remove workspace + deregister (done → terminal).
    Decommission,
    /// Leave the session alone; the string explains why (for the plan/report).
    Skip(&'static str),
}

impl PruneAction {
    /// A stable lowercase label for the action, for JSON output and tables.
    ///
    /// Why: programmatic callers (the claude-mpm pause skill) and the human
    /// table both need a consistent verb; deriving it here keeps the wording in
    /// one place rather than scattered across the formatter.
    /// What: returns `"stop"`, `"decommission"`, or `"skip"`.
    /// Test: `action_label_*` in the `tests` module.
    pub fn label(&self) -> &'static str {
        match self {
            PruneAction::Stop => "stop",
            PruneAction::Decommission => "decommission",
            PruneAction::Skip(_) => "skip",
        }
    }

    /// True when the action mutates the fleet (i.e. is not a skip).
    ///
    /// Why: the handler counts how many sessions it would actually touch and
    /// the dry-run / live summary both need that count; centralizing the
    /// predicate avoids re-matching the enum at every call site.
    /// What: returns `false` for `Skip`, `true` otherwise.
    /// Test: `is_actionable_*` in the `tests` module.
    pub fn is_actionable(&self) -> bool {
        !matches!(self, PruneAction::Skip(_))
    }
}

/// Canonicalize an activity-monitor verdict string to a comparable token.
///
/// Why: the daemon emits the verdict via `format!("{:?}", state).to_lowercase()`,
/// so `BlockedOnPermission` becomes `"blockedonpermission"` (no separators),
/// while the issue and humans write `blocked-on-permission` /
/// `blocked_on_permission`. Normalizing away separators and case lets one
/// `decide` match every spelling without a brittle exact-string compare.
/// What: lowercases the input and removes ASCII `-`, `_`, and whitespace,
/// returning the collapsed token (e.g. `"blocked-on-permission"` → `"blockedonpermission"`).
/// Test: `normalize_verdict_strips_separators`, `normalize_verdict_lowercases`.
pub fn normalize_verdict(verdict: &str) -> String {
    verdict
        .chars()
        .filter(|c| !matches!(c, '-' | '_') && !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Map a session's latest activity verdict to a [`PruneAction`] (pure policy).
///
/// Why: this is the locked teardown policy (issue #1313, decided with Bob
/// 2026-06-16) expressed as a single pure function — the testable heart of the
/// prune command. Sessions with no verdict yet are represented by `None` and
/// must be skipped, exactly like the indeterminate `unknown` verdict, so that
/// the cleanup never touches a session whose state it cannot positively
/// classify.
/// What: `Some("idle")` → `Stop`; `Some("done")` → `Decommission`; every other
/// verdict (`working`, any `blocked*`, `errored`, `unknown`, or an unrecognized
/// label) and `None` → `Skip` with a human reason. Matching runs over the
/// [`normalize_verdict`] token so separator/case variants all resolve.
/// Test: `decide_idle_stops`, `decide_done_decommissions`, `decide_working_skips`,
/// `decide_blocked_skips`, `decide_errored_skips`, `decide_unknown_skips`,
/// `decide_no_verdict_skips`, `decide_unrecognized_skips`.
pub fn decide(verdict: Option<&str>) -> PruneAction {
    let Some(raw) = verdict else {
        return PruneAction::Skip("no verdict yet");
    };
    match normalize_verdict(raw).as_str() {
        "idle" => PruneAction::Stop,
        "done" => PruneAction::Decommission,
        "working" => PruneAction::Skip("working"),
        "blockedonpermission" => PruneAction::Skip("blocked on permission"),
        "errored" => PruneAction::Skip("errored"),
        "unknown" | "" => PruneAction::Skip("unknown / no classifier"),
        _ => PruneAction::Skip("unrecognized verdict"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the headline policy guarantee — an `idle` session is stopped
    /// (resumable), never decommissioned.
    /// What: asserts `decide(Some("idle")) == Stop`.
    /// Test: this test.
    #[test]
    fn decide_idle_stops() {
        assert_eq!(decide(Some("idle")), PruneAction::Stop);
    }

    /// Why: a finished (`done`) session reclaims its workspace via full teardown.
    /// What: asserts `decide(Some("done")) == Decommission`.
    /// Test: this test.
    #[test]
    fn decide_done_decommissions() {
        assert_eq!(decide(Some("done")), PruneAction::Decommission);
    }

    /// Why: an actively working session must never be touched.
    /// What: asserts `working` maps to a non-actionable `Skip`.
    /// Test: this test.
    #[test]
    fn decide_working_skips() {
        let action = decide(Some("working"));
        assert!(matches!(action, PruneAction::Skip(_)));
        assert!(!action.is_actionable());
    }

    /// Why: a session blocked on a human permission decision must be left for
    /// the operator; verify every spelling the daemon/issue might emit maps to
    /// skip.
    /// What: asserts the Debug-lowercased wire form and the hyphen/underscore
    /// human forms all skip.
    /// Test: this test.
    #[test]
    fn decide_blocked_skips() {
        for spelling in [
            "blockedonpermission",
            "blocked-on-permission",
            "blocked_on_permission",
            "Blocked On Permission",
        ] {
            assert!(
                matches!(decide(Some(spelling)), PruneAction::Skip(_)),
                "spelling {spelling:?} should skip"
            );
        }
    }

    /// Why: an errored session may need inspection; it must not be reaped.
    /// What: asserts `errored` skips.
    /// Test: this test.
    #[test]
    fn decide_errored_skips() {
        assert!(matches!(decide(Some("errored")), PruneAction::Skip(_)));
    }

    /// Why: `unknown` means the classifier could not positively label the
    /// session (e.g. no OpenRouter key) — treat it exactly like no verdict.
    /// What: asserts `unknown` skips.
    /// Test: this test.
    #[test]
    fn decide_unknown_skips() {
        assert!(matches!(decide(Some("unknown")), PruneAction::Skip(_)));
    }

    /// Why: a session with no activity verdict yet must be left alone.
    /// What: asserts `None` skips with the "no verdict yet" reason.
    /// Test: this test.
    #[test]
    fn decide_no_verdict_skips() {
        assert_eq!(decide(None), PruneAction::Skip("no verdict yet"));
    }

    /// Why: defense-in-depth — any future/unrecognized verdict label must fail
    /// safe to skip rather than accidentally tear down a session.
    /// What: asserts a novel label skips.
    /// Test: this test.
    #[test]
    fn decide_unrecognized_skips() {
        assert!(matches!(
            decide(Some("compacting")),
            PruneAction::Skip("unrecognized verdict")
        ));
    }

    /// Why: the JSON/table renderers depend on stable verbs.
    /// What: asserts each action's label.
    /// Test: this test.
    #[test]
    fn action_label_is_stable() {
        assert_eq!(PruneAction::Stop.label(), "stop");
        assert_eq!(PruneAction::Decommission.label(), "decommission");
        assert_eq!(PruneAction::Skip("x").label(), "skip");
    }

    /// Why: the actionable-count logic must treat only stop/decommission as
    /// mutating.
    /// What: asserts the `is_actionable` predicate per variant.
    /// Test: this test.
    #[test]
    fn is_actionable_distinguishes_skip() {
        assert!(PruneAction::Stop.is_actionable());
        assert!(PruneAction::Decommission.is_actionable());
        assert!(!PruneAction::Skip("x").is_actionable());
    }

    /// Why: separator collapsing is what lets the wire/human spellings unify.
    /// What: asserts hyphens, underscores, and whitespace are removed.
    /// Test: this test.
    #[test]
    fn normalize_verdict_strips_separators() {
        assert_eq!(
            normalize_verdict("blocked-on_permission"),
            "blockedonpermission"
        );
        assert_eq!(
            normalize_verdict("blocked on permission"),
            "blockedonpermission"
        );
    }

    /// Why: case must not affect the decision.
    /// What: asserts mixed case lowercases.
    /// Test: this test.
    #[test]
    fn normalize_verdict_lowercases() {
        assert_eq!(normalize_verdict("IDLE"), "idle");
        assert_eq!(normalize_verdict("Done"), "done");
    }
}
