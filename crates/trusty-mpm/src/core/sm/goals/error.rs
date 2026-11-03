//! Structured errors for the SM goal store (DOC-14 §9).
//!
//! Why: SM-6 is library code in `trusty-mpm-core`; per the workspace convention
//! library failures surface as typed, matchable `thiserror` enums rather than
//! `unwrap()`/`panic!`. The two operator-facing failure modes — an unknown goal
//! id and a verification-gate rejection (§3.5) — must be DISTINCT variants so the
//! future endpoint/loop (SM-7/SM-8) can render a precise message; the I/O and
//! palace surfaces degrade gracefully so a missing cache or unavailable palace
//! never crashes the daemon.
//! What: [`SmGoalError`] wraps not-found, the verification-gate rejection, cache
//! I/O, JSON (de)serialisation, and palace-memory failures, preserving the source
//! chain where one exists.
//! Test: `goals/store_tests.rs` asserts the `NotFound` and `VerificationGate`
//! variants on their respective paths; happy paths assert `Ok`.

/// Structured errors for goal-store operations (library → `thiserror`).
///
/// Why: see the module docs — distinct, matchable variants for the gate and
/// not-found cases, plus graceful wrappers for the persistence surfaces.
/// What: the six failure modes of the goal store.
/// Test: `close_without_all_verified_is_rejected` (gate),
/// `link_unknown_goal_is_not_found` (not-found),
/// `update_to_verified_without_evidence_is_rejected` (evidence).
#[derive(Debug, thiserror::Error)]
pub enum SmGoalError {
    /// No goal exists with the referenced id.
    #[error("goal '{0}' not found")]
    NotFound(String),

    /// The verification gate (§3.5) refused a `Done` transition because not every
    /// linked task is `Verified`. Carries the offending goal id plus the
    /// verified/total counts so the caller can render an actionable message.
    #[error(
        "verification gate: goal '{goal_id}' cannot be marked Done — \
         {verified}/{total} linked tasks verified (all must be Verified)"
    )]
    VerificationGate {
        /// The goal that failed the gate.
        goal_id: String,
        /// How many linked tasks are `Verified`.
        verified: usize,
        /// Total linked tasks.
        total: usize,
    },

    /// A link was stored or updated as `Verified` without non-blank evidence
    /// (#10722). The goal is left unchanged.
    #[error("goal '{goal_id}': session '{session_id}' cannot be Verified without evidence")]
    VerifiedWithoutEvidence {
        /// The goal the link belongs to.
        goal_id: String,
        /// The linked session whose update was refused.
        session_id: String,
    },

    /// A cache filesystem operation (mkdir / write / rename / read) failed.
    #[error("goal cache I/O failed: {source}")]
    Io {
        /// The underlying filesystem error.
        source: std::io::Error,
    },

    /// Serialising or deserialising the goal cache JSON failed.
    #[error("goal cache (de)serialisation failed: {source}")]
    Serde {
        /// The underlying serde_json error.
        source: serde_json::Error,
    },

    /// A palace-memory operation (the source-of-truth write/enumerate) failed.
    #[error("goal palace persistence failed: {message}")]
    Palace {
        /// The underlying memory error, as a string (the concrete `SmMemoryError`
        /// lives behind the `sm-memory` feature, so it is captured as a message to
        /// keep this error type feature-independent).
        message: String,
    },
}

/// Result alias for goal-store operations.
///
/// Why: keeps the public signatures terse and consistent with the rest of the
/// `sm` module (mirrors `SmMemoryResult` / `ConversationStoreResult`).
/// What: `Result<T, SmGoalError>`.
/// Test: used throughout `goals/store_tests.rs`.
pub type SmGoalResult<T> = std::result::Result<T, SmGoalError>;
