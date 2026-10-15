//! The `gh` stderr phrases that mean "no such secret" or "signed out"
//! (#9069).
//!
//! Why: G4 — a `gh` with no usable login must read as
//! [`crate::SecretsError::BackendLocked`], whose hint names `gh auth login`,
//! and the push must not run. A secret the repository does not hold yet is
//! a miss on the baseline read only, never on the confirming read.
//! What: [`MISSING`] and [`LOCKED`], matched ASCII case-insensitively by the
//! shared classifier on a failed null-stdin run's stderr. A run that carried
//! the value on stdin is never read for meaning.
//!
//! UNCONFIRMED: the phrases follow `gh`'s documented output (`gh api`
//! prints `gh: Not Found (HTTP 404)`; a missing login prints the
//! `gh auth login` hint), but no run against a real account has pinned
//! them. A phrase that never matches turns a miss or a locked run into a
//! plain backend error, which fails closed. The #9069 live check pins them.
//! Test: `github_signed_out_preflight_is_backend_locked_and_never_pushes`.

/// A `gh api` read of a secret the repository does not hold.
pub(super) const MISSING: &[&str] = &["(http 404)"];

/// Phrases meaning `gh` has no usable login.
pub(super) const LOCKED: &[&str] = &[
    // No stored login: `gh` prints the login hint.
    "gh auth login",
    "not logged into any",
    // A stored token GitHub no longer accepts.
    "(http 401)",
    "bad credentials",
];
