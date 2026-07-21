//! Shared pane-identity cross-check for `TM_MANAGED_SESSION_ID` consumers
//! (issue #3600).
//!
//! Why: `TM_MANAGED_SESSION_ID` is published via `tmux set-environment -t
//! <session>` (`runtime::claude_code::session_id_export_prefix`,
//! `session_manager::manager`) — SESSION-scoped, not pane-scoped. tmux
//! applies a session's stored environment to every NEW pane/window created
//! afterward in that same tmux session, so a sibling pane opened after a
//! runtime-exit heal inherits an IDENTICAL `TM_MANAGED_SESSION_ID` and would
//! wrongly believe it IS that session. `guided.rs`'s nested-session guard hit
//! this exact hijack (the "#2456 CORRECTION", see its module doc) and closed
//! it by comparing tmux's own stable `pane_id` (never inherited across
//! panes, unlike an env var or `pane_pid`) against the resolved record's OWN
//! captured `pane_id` before ever trusting the env var for a destructive
//! action. Two more CLI consumers read the bare env var with NO such check
//! (`commands::rename::session_rename`'s in-session form,
//! `commands::pm_guard_deny_by_default::persona_status`) — issue #3600. This
//! module is the ONE shared primitive all three now use, so the guard cannot
//! rot back into two divergent (or missing) copies.
//! What: [`pane_identity_confirmed`] is the pure bool predicate originally
//! defined in `guided.rs` (moved here unchanged, still re-exported from
//! there so its existing call sites/imports keep resolving).
//! [`EnvSessionIdentity`] is a richer three-way outcome
//! (`Confirmed`/`Mismatch`/`Unavailable`) for callers — `rename.rs` and
//! `pm_guard_deny_by_default.rs` — that must render a SPECIFIC refusal
//! message (naming both the env-var session and the actual pane) rather than
//! a bare bool.
//! Test: `pane_identity_confirmed_true_when_pane_id_matches_record`,
//! `pane_identity_confirmed_false_when_current_pane_id_absent`,
//! `pane_identity_confirmed_false_when_record_pane_id_absent`,
//! `pane_identity_confirmed_false_when_pane_ids_differ`,
//! `pane_identity_confirmed_false_when_inherited_env_but_different_pane_id`
//! (moved from `guided.rs`'s own test file, `tests_behavior_c_tests.rs`);
//! `env_session_identity_confirmed_when_pane_ids_match`,
//! `env_session_identity_mismatch_when_pane_ids_differ`,
//! `env_session_identity_unavailable_when_current_pane_id_absent`,
//! `env_session_identity_unavailable_when_record_pane_id_absent`.

/// Confirm the CURRENT tmux pane is provably the one bound to a resolved
/// session record, by comparing tmux's own stable `pane_id` — never
/// `TM_MANAGED_SESSION_ID` alone.
///
/// Why: a process-level env-var comparison was tried and EMPIRICALLY
/// DISPROVEN (live tmux 3.6b, #2456 review finding 1 round 2): the
/// runtime-exit healing step calls `tmux set-environment -t <session>` —
/// SESSION-scoped — and tmux applies a session's stored environment to the
/// process env of every NEW pane/window created in that session AFTERWARD.
/// Any session that has exited-and-relaunched once therefore has a
/// "poisoned" session env that a genuinely different sibling window
/// inherits into its OWN process env — an env-based gate would wrongly pass
/// that sibling. tmux's own `pane_id` (e.g. `"%5"`, distinct from
/// `pane_pid`, which the OS can reuse across a pane's lifetime) is NEVER
/// inherited across panes.
/// What: `true` only when BOTH `current_pane_id` and `record_pane_id` are
/// `Some` and equal. `false` when either is `None` (a legacy record that
/// never had a `pane_id` captured, or a tmux query failure) OR they differ —
/// fails CLOSED in every ambiguous case, meaning the caller must refuse (or
/// fall back to a weaker, explicit confirmation) rather than proceed.
/// Test: `pane_identity_confirmed_true_when_pane_id_matches_record`,
/// `pane_identity_confirmed_false_when_current_pane_id_absent`,
/// `pane_identity_confirmed_false_when_record_pane_id_absent`,
/// `pane_identity_confirmed_false_when_inherited_env_but_different_pane_id`
/// (the exact two-idle-panes/inherited-session-env hijack scenario).
pub(crate) fn pane_identity_confirmed(
    current_pane_id: Option<&str>,
    record_pane_id: Option<&str>,
) -> bool {
    matches!((current_pane_id, record_pane_id), (Some(cur), Some(rec)) if cur == rec)
}

/// Three-way outcome of cross-checking an env-var-resolved session against
/// this process's own CURRENT tmux pane (issue #3600).
///
/// Why: [`pane_identity_confirmed`]'s bare bool is enough for `guided.rs`'s
/// own gate (which only ever needs "safe to proceed or not"), but a CLI
/// command that REFUSES on failure (`rename`) or gates on it
/// (`pm_guard_deny_by_default`) should tell the operator WHY: a genuine
/// cross-pane mismatch (naming both pane ids) is a different, more alarming
/// situation than simply being unable to determine identity at all (not
/// inside tmux, a legacy record, or a daemon/tmux query failure).
/// What: [`Self::evaluate`] is the pure classifier; [`Self::is_confirmed`] is
/// the single collapse back to a pass/fail decision for callers that don't
/// need the distinction.
/// Test: `env_session_identity_confirmed_when_pane_ids_match`,
/// `env_session_identity_mismatch_when_pane_ids_differ`,
/// `env_session_identity_unavailable_when_current_pane_id_absent`,
/// `env_session_identity_unavailable_when_record_pane_id_absent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvSessionIdentity {
    /// Both pane ids are known and match — this process is provably running
    /// inside the resolved record's own pane.
    Confirmed,
    /// Both pane ids are known but differ — this process is in a DIFFERENT
    /// pane than the one the resolved record's own `pane_id` was captured
    /// against (the tmux-session-scoped env-var inheritance hijack).
    Mismatch {
        current_pane_id: String,
        record_pane_id: String,
    },
    /// One or both pane ids could not be determined (not inside tmux, a
    /// tmux query failure, or a legacy record with no captured `pane_id`).
    /// Deliberately distinct from [`Self::Confirmed`] — a caller must never
    /// treat this as a pass; it is exactly the "verification failed" case
    /// that must not silently degrade into "trust the env var".
    Unavailable,
}

impl EnvSessionIdentity {
    /// Classify a cross-check from the two already-resolved pane ids.
    ///
    /// Why: isolating the classification from the daemon round-trip and the
    /// `tmux display-message` shell-out makes the actual decision
    /// exhaustively unit-testable without a live daemon or tmux server —
    /// mirrors the pure-predicate/thin-I/O-wrapper split already used
    /// throughout this crate (`guided::nested_managed_match`,
    /// `pm_guard_deny_by_default::classify_persona_status`).
    /// What: `Confirmed` when both are `Some` and equal; `Mismatch` when
    /// both are `Some` but differ; `Unavailable` for every other case
    /// (either or both `None`).
    /// Test: `env_session_identity_confirmed_when_pane_ids_match`,
    /// `env_session_identity_mismatch_when_pane_ids_differ`,
    /// `env_session_identity_unavailable_when_current_pane_id_absent`,
    /// `env_session_identity_unavailable_when_record_pane_id_absent`.
    pub(crate) fn evaluate(current_pane_id: Option<&str>, record_pane_id: Option<&str>) -> Self {
        match (current_pane_id, record_pane_id) {
            (Some(cur), Some(rec)) if cur == rec => Self::Confirmed,
            (Some(cur), Some(rec)) => Self::Mismatch {
                current_pane_id: cur.to_string(),
                record_pane_id: rec.to_string(),
            },
            _ => Self::Unavailable,
        }
    }

    /// Collapse to a plain pass/fail decision for callers that don't render
    /// the mismatch/unavailable distinction.
    ///
    /// Test: exercised transitively by `env_session_identity_*`.
    pub(crate) fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── pane_identity_confirmed (moved from guided.rs / tests_behavior_c_tests.rs,
    // #2456 review finding 1, ROUND 2) ──────────────────────────────────────────

    #[test]
    fn pane_identity_confirmed_true_when_pane_id_matches_record() {
        // THIS pane's own tmux pane_id equals the SAME record's captured
        // pane_id — genuinely the pane bound to that record; safe to relaunch.
        assert!(pane_identity_confirmed(Some("%5"), Some("%5")));
    }

    #[test]
    fn pane_identity_confirmed_false_when_current_pane_id_absent() {
        // The CURRENT pane's tmux query failed (or we are not inside tmux at
        // all) — cannot confirm identity; must refuse, even if the record
        // DOES have a pane_id.
        assert!(!pane_identity_confirmed(None, Some("%5")));
    }

    #[test]
    fn pane_identity_confirmed_false_when_record_pane_id_absent() {
        // A legacy record (created before #2453) never had a pane_id
        // captured — `None` must be treated as "identity unconfirmed", never
        // an implicit match, regardless of the current pane's own id.
        assert!(!pane_identity_confirmed(Some("%5"), None));
    }

    #[test]
    fn pane_identity_confirmed_false_when_pane_ids_differ() {
        // Both resolved, but for genuinely DIFFERENT panes — must still refuse.
        assert!(!pane_identity_confirmed(Some("%7"), Some("%5")));
    }

    #[test]
    fn pane_identity_confirmed_false_when_inherited_env_but_different_pane_id() {
        // The exact hijack scenario: a session whose runtime-exit healing has
        // fired once (so `TM_MANAGED_SESSION_ID` is now poisoned into the
        // tmux SESSION environment) gets a second window opened afterward.
        // That new window's process env INHERITS the healed session's id —
        // an env-var-only gate would wrongly treat this as belonging to the
        // healed session's record. Modeled here directly at the pane_id
        // layer: the healed record's `pane_id` is `"%5"` (the ORIGINAL
        // pane), but the sibling window's own current pane_id is `"%9"` —
        // must reject.
        let healed_record_pane_id = Some("%5");
        let sibling_window_current_pane_id = Some("%9");
        assert!(!pane_identity_confirmed(
            sibling_window_current_pane_id,
            healed_record_pane_id
        ));
    }

    // ── EnvSessionIdentity (issue #3600) ─────────────────────────────────────

    #[test]
    fn env_session_identity_confirmed_when_pane_ids_match() {
        assert_eq!(
            EnvSessionIdentity::evaluate(Some("%5"), Some("%5")),
            EnvSessionIdentity::Confirmed
        );
        assert!(EnvSessionIdentity::evaluate(Some("%5"), Some("%5")).is_confirmed());
    }

    #[test]
    fn env_session_identity_mismatch_when_pane_ids_differ() {
        // The exact #3600 hijack: a sibling pane inherited the same
        // `TM_MANAGED_SESSION_ID` via tmux's session-scoped env propagation,
        // but its OWN pane_id ("%9") differs from the record's captured
        // pane_id ("%5") — must classify as a named mismatch, not a bare
        // refusal, so the caller can report both ids.
        assert_eq!(
            EnvSessionIdentity::evaluate(Some("%9"), Some("%5")),
            EnvSessionIdentity::Mismatch {
                current_pane_id: "%9".to_string(),
                record_pane_id: "%5".to_string(),
            }
        );
        assert!(!EnvSessionIdentity::evaluate(Some("%9"), Some("%5")).is_confirmed());
    }

    #[test]
    fn env_session_identity_unavailable_when_current_pane_id_absent() {
        // Not inside tmux, or the tmux query failed — verification simply
        // could not run. Must NOT degrade into "trust the env var".
        assert_eq!(
            EnvSessionIdentity::evaluate(None, Some("%5")),
            EnvSessionIdentity::Unavailable
        );
        assert!(!EnvSessionIdentity::evaluate(None, Some("%5")).is_confirmed());
    }

    #[test]
    fn env_session_identity_unavailable_when_record_pane_id_absent() {
        // A legacy record (predates pane_id capture) or a resolve failure —
        // same "cannot verify" treatment as the pane-id-absent case above.
        assert_eq!(
            EnvSessionIdentity::evaluate(Some("%5"), None),
            EnvSessionIdentity::Unavailable
        );
        assert!(!EnvSessionIdentity::evaluate(Some("%5"), None).is_confirmed());
    }
}
