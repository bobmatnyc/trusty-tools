//! The Vercel CLI's stderr phrases that mean "signed out" (#9069 PR 2).
//!
//! Why: a CLI with no usable login must read as
//! [`crate::SecretsError::BackendLocked`], whose hint names `vercel login`,
//! and the push must not run. Vercel has no "missing item" read here: the
//! listing names every variable, so [`MISSING`] is empty.
//! What: [`LOCKED`], matched ASCII case-insensitively by the shared
//! classifier on a failed null-stdin run's stderr. A run that carried the
//! value on stdin is never read for meaning.
//!
//! UNCONFIRMED: the phrases follow the CLI's documented output, but no run
//! against a real account has pinned them. A phrase that never matches turns
//! a signed-out run into a plain backend error, which fails closed. The
//! #9069 live check pins them.
//! Test: `vercel_signed_out_preflight_is_backend_locked_and_never_pushes`.

/// No listing is ever read as "missing".
pub(super) const MISSING: &[&str] = &[];

/// Phrases meaning the Vercel CLI has no usable login.
pub(super) const LOCKED: &[&str] = &[
    "no existing credentials found",
    "vercel login",
    "token is not valid",
    "invalid token",
];
