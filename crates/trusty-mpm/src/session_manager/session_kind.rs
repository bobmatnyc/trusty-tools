//! What role a managed session plays in the fleet (#8942).
//!
//! Why: the Architect (the fleet supervisor) and its `-poll`/`-collector`
//! helpers must never be stopped, resumed, pruned or killed by the daemon.
//! A record needs a field that says so, and a value this build does not know
//! must be protected too, never read as an ordinary session.
//! What: [`SessionKind`], persisted on `SessionRecord::kind`.
//! Test: `session_kind_tests.rs`.

use serde::{Deserialize, Serialize};

/// The lifecycle role of a managed session record (#8942).
///
/// Why: `SessionProfile` resolves an unknown value to `Pm`, which fails open.
/// This kind fails closed: [`SessionKind::Unknown`] catches any value a newer
/// build wrote, and it is protected exactly like the supervisor kinds.
/// What: `Ordinary` is the serde default, so every record persisted before
/// #8942 loads as an ordinary session. The wire form is snake_case.
/// Test: `legacy_record_without_kind_deserializes_as_ordinary`,
/// `an_unrecognised_kind_reads_as_unknown_and_is_protected`,
/// `only_ordinary_is_unprotected`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// An ordinary managed session: stoppable, resumable, prunable.
    #[default]
    Ordinary,
    /// The Architect, the one fleet supervisor session per user.
    Supervisor,
    /// A helper session of the Architect (`<name>-poll`, `<name>-collector`).
    SupervisorAux,
    /// A kind this build does not recognise. Protected like `Supervisor`.
    #[serde(other)]
    Unknown,
}

impl SessionKind {
    /// Whether the daemon must never tear down, signal or auto-resume a
    /// session of this kind.
    ///
    /// Why: one predicate for every refusal site, so they cannot disagree.
    /// What: `true` for every kind except [`SessionKind::Ordinary`].
    /// Test: `only_ordinary_is_unprotected`.
    pub fn is_protected(self) -> bool {
        !matches!(self, Self::Ordinary)
    }
}

#[cfg(test)]
#[path = "session_kind_tests.rs"]
mod tests;
