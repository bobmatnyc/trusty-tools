//! Deliverable status enum and its transition state machine (DOC-35 §10.3, #2380).
//!
//! Why: a Deliverable moves through a small, fixed lifecycle
//! (`proposed → in-progress → [blocked ↔ in-progress] → complete →
//! delivered/shipped`). Allowing an arbitrary jump — e.g. `proposed → complete`
//! — would corrupt the ledger the L3 substrate exists to keep honest (§10.1).
//! Encoding the legal transitions as a pure, table-driven function keeps the rule
//! deterministic (§11 boundary: no LLM, no inference) and lets the CRUD API layer
//! reject illegal `set-status` requests with a structured error that names the
//! legal next states (#2380).
//! What: [`DeliverableStatus`] (the six lifecycle states), [`allowed_next`]
//! ([`DeliverableStatus::allowed_next`]) as the single source of truth for legal
//! transitions, [`DeliverableStatus::can_transition`], and
//! [`validate_transition`] which returns a [`TransitionError`] naming the legal
//! next states on rejection.
//! Test: `transition_table_covers_every_ordered_pair` exercises all 36 ordered
//! pairs against an independent legal-set; `rejects_proposed_to_complete` and
//! `terminal_states_have_no_successors` pin the named cases from #2380;
//! `status_wire_format_is_kebab_case` pins the serde encoding.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The lifecycle state of a [`Deliverable`](crate::deliverable::Deliverable).
///
/// Why: the state machine in §10.3 is the deterministic backbone of Deliverable
/// tracking; a closed enum makes every legal state explicit and lets the
/// transition table be exhaustive at compile time.
/// What: the six states of `proposed → in-progress → [blocked ↔ in-progress] →
/// complete → delivered/shipped`. `Delivered` and `Shipped` are both terminal
/// and both reachable from `Complete` (the `/` in the spec's `delivered/shipped`
/// — an explicit, manual user choice of terminal label, §10.3). Serialized in
/// kebab-case so `InProgress` is `"in-progress"` on the wire.
/// Test: `status_wire_format_is_kebab_case`, and the transition tests below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeliverableStatus {
    /// Work is planned but not started.
    Proposed,
    /// A session is actively working the Deliverable.
    InProgress,
    /// Work is paused on an external blocker (manual, never inferred, §10.3).
    Blocked,
    /// The objective gate passed or the user confirmed completion (§10.3).
    Complete,
    /// Terminal: the Deliverable was delivered to its consumer.
    Delivered,
    /// Terminal: the Deliverable shipped.
    Shipped,
}

impl DeliverableStatus {
    /// Every status variant, for exhaustive iteration in tests and histograms.
    ///
    /// Why: the transition-table test and any future status rollup (#2382) need
    /// to enumerate every state without hand-maintaining a second list that could
    /// drift from the enum.
    /// What: the six variants in lifecycle order.
    /// Test: `all_lists_every_variant`.
    pub const ALL: [DeliverableStatus; 6] = [
        DeliverableStatus::Proposed,
        DeliverableStatus::InProgress,
        DeliverableStatus::Blocked,
        DeliverableStatus::Complete,
        DeliverableStatus::Delivered,
        DeliverableStatus::Shipped,
    ];

    /// The stable kebab-case wire label for this status.
    ///
    /// Why: the `Display` impl, error messages, and the structured
    /// `allowed_next` array must all use the exact string serde emits so an
    /// operator reading a rejection sees the same tokens they would `PATCH`.
    /// What: maps each variant to its serde `rename_all = "kebab-case"` string.
    /// Test: `status_wire_format_is_kebab_case`.
    pub const fn as_str(self) -> &'static str {
        match self {
            DeliverableStatus::Proposed => "proposed",
            DeliverableStatus::InProgress => "in-progress",
            DeliverableStatus::Blocked => "blocked",
            DeliverableStatus::Complete => "complete",
            DeliverableStatus::Delivered => "delivered",
            DeliverableStatus::Shipped => "shipped",
        }
    }

    /// The set of states this status may legally transition TO (§10.3).
    ///
    /// Why: this is the single source of truth for the state machine. Every
    /// other predicate (`can_transition`, `validate_transition`) and the
    /// structured rejection error derive from it, so the machine can never
    /// disagree with itself.
    /// What: returns a static slice of legal successors. Self-transitions are
    /// deliberately excluded — `set-status` requires an actual state change.
    /// `Delivered` and `Shipped` are terminal (empty slice). There is no path
    /// that skips `Proposed → InProgress` or jumps straight to a terminal.
    /// Test: `transition_table_covers_every_ordered_pair`,
    /// `terminal_states_have_no_successors`.
    pub const fn allowed_next(self) -> &'static [DeliverableStatus] {
        match self {
            DeliverableStatus::Proposed => &[DeliverableStatus::InProgress],
            DeliverableStatus::InProgress => {
                &[DeliverableStatus::Blocked, DeliverableStatus::Complete]
            }
            DeliverableStatus::Blocked => &[DeliverableStatus::InProgress],
            DeliverableStatus::Complete => {
                &[DeliverableStatus::Delivered, DeliverableStatus::Shipped]
            }
            DeliverableStatus::Delivered => &[],
            DeliverableStatus::Shipped => &[],
        }
    }

    /// Whether `self → to` is a legal transition.
    ///
    /// Why: the CRUD `set-status` path needs a cheap boolean gate before it
    /// mutates and persists a Deliverable.
    /// What: true iff `to` appears in [`allowed_next`](Self::allowed_next).
    /// Test: `transition_table_covers_every_ordered_pair`.
    pub fn can_transition(self, to: DeliverableStatus) -> bool {
        self.allowed_next().contains(&to)
    }
}

impl std::fmt::Display for DeliverableStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rejected status transition, naming the legal next states (#2380).
///
/// Why: rejecting an illegal `set-status` with a bare "invalid" is useless to
/// the caller. #2380 requires the rejection to name the legal next states so the
/// operator (or a future policy engine) can self-correct without consulting the
/// spec.
/// What: carries the attempted `from`/`to` and the list of states that WOULD
/// have been legal from `from`. Its `Display` renders a one-line, deterministic
/// message; the route layer additionally surfaces `allowed` as a JSON array.
/// Test: `rejects_proposed_to_complete`, `error_message_names_legal_states`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub struct TransitionError {
    /// The Deliverable's current status.
    pub from: DeliverableStatus,
    /// The requested (rejected) target status.
    pub to: DeliverableStatus,
    /// The states that would have been legal transitions from `from`.
    pub allowed: Vec<DeliverableStatus>,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let allowed = if self.allowed.is_empty() {
            "none (terminal state)".to_string()
        } else {
            self.allowed
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        write!(
            f,
            "invalid status transition {} \u{2192} {}; legal next states from {}: [{}]",
            self.from, self.to, self.from, allowed
        )
    }
}

/// Validate a `from → to` status transition against the §10.3 state machine.
///
/// Why: this is the one deterministic gate the CRUD `set-status` endpoint calls
/// before persisting a status change (#2380). Keeping it a free function makes it
/// unit-testable in isolation from any store or HTTP concern.
/// What: `Ok(())` when the transition is legal; otherwise a [`TransitionError`]
/// carrying the legal next states. No side effects, no I/O — a pure function of
/// its two inputs (§11 determinism test).
/// Test: `transition_table_covers_every_ordered_pair`,
/// `rejects_proposed_to_complete`, `rejects_self_transition`.
pub fn validate_transition(
    from: DeliverableStatus,
    to: DeliverableStatus,
) -> Result<(), TransitionError> {
    if from.can_transition(to) {
        Ok(())
    } else {
        Err(TransitionError {
            from,
            to,
            allowed: from.allowed_next().to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::DeliverableStatus::*;
    use super::*;

    /// The complete, independently-authored set of LEGAL ordered transition
    /// pairs from §10.3. Kept separate from `allowed_next` so the table test
    /// verifies behavior against an independent spec transcription, not a
    /// tautological echo of the implementation.
    const LEGAL_PAIRS: [(DeliverableStatus, DeliverableStatus); 6] = [
        (Proposed, InProgress),
        (InProgress, Blocked),
        (InProgress, Complete),
        (Blocked, InProgress),
        (Complete, Delivered),
        (Complete, Shipped),
    ];

    #[test]
    fn all_lists_every_variant() {
        assert_eq!(DeliverableStatus::ALL.len(), 6);
    }

    /// #2380 core: exercise EVERY ordered pair (6 x 6 = 36) — both legal and
    /// illegal — against the independent legal-set and confirm `validate_transition`
    /// agrees with `can_transition`.
    #[test]
    fn transition_table_covers_every_ordered_pair() {
        let legal: std::collections::HashSet<(DeliverableStatus, DeliverableStatus)> =
            LEGAL_PAIRS.iter().copied().collect();
        let mut checked = 0usize;
        for &from in &DeliverableStatus::ALL {
            for &to in &DeliverableStatus::ALL {
                let expect_ok = legal.contains(&(from, to));
                assert_eq!(
                    from.can_transition(to),
                    expect_ok,
                    "can_transition({from} -> {to}) disagreed with the §10.3 legal set"
                );
                match validate_transition(from, to) {
                    Ok(()) => assert!(expect_ok, "validate accepted illegal {from} -> {to}"),
                    Err(e) => {
                        assert!(!expect_ok, "validate rejected legal {from} -> {to}");
                        assert_eq!(e.from, from);
                        assert_eq!(e.to, to);
                        assert_eq!(e.allowed, from.allowed_next().to_vec());
                    }
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 36, "must exhaustively check all 36 ordered pairs");
    }

    #[test]
    fn rejects_proposed_to_complete() {
        // The literal example named in issue #2380.
        let err = validate_transition(Proposed, Complete).expect_err("must reject");
        assert_eq!(err.from, Proposed);
        assert_eq!(err.to, Complete);
        assert_eq!(err.allowed, vec![InProgress]);
    }

    #[test]
    fn rejects_self_transition() {
        // `set-status` requires an actual state change; X -> X is not a
        // transition in the machine.
        for &s in &DeliverableStatus::ALL {
            assert!(
                validate_transition(s, s).is_err(),
                "self-transition {s} -> {s} must be rejected"
            );
        }
    }

    #[test]
    fn terminal_states_have_no_successors() {
        assert!(Delivered.allowed_next().is_empty());
        assert!(Shipped.allowed_next().is_empty());
        for &to in &DeliverableStatus::ALL {
            assert!(validate_transition(Delivered, to).is_err());
            assert!(validate_transition(Shipped, to).is_err());
        }
    }

    #[test]
    fn blocked_branch_round_trips() {
        assert!(InProgress.can_transition(Blocked));
        assert!(Blocked.can_transition(InProgress));
        // Blocked cannot leap straight to Complete — it must return to
        // in-progress first (§10.3).
        assert!(!Blocked.can_transition(Complete));
    }

    #[test]
    fn error_message_names_legal_states() {
        let err = validate_transition(Proposed, Shipped).expect_err("reject");
        let msg = err.to_string();
        assert!(msg.contains("proposed"), "message names from-state: {msg}");
        assert!(msg.contains("shipped"), "message names to-state: {msg}");
        assert!(
            msg.contains("in-progress"),
            "message names the legal next state: {msg}"
        );
    }

    #[test]
    fn status_wire_format_is_kebab_case() {
        assert_eq!(
            serde_json::to_string(&InProgress).unwrap(),
            "\"in-progress\""
        );
        assert_eq!(serde_json::to_string(&Proposed).unwrap(), "\"proposed\"");
        let back: DeliverableStatus = serde_json::from_str("\"delivered\"").unwrap();
        assert_eq!(back, Delivered);
        // as_str must match the serde encoding exactly.
        for &s in &DeliverableStatus::ALL {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
        }
    }
}
