//! The `keeper` output phrases that mean Keeper Commander is locked
//! (#7519 P3).
//!
//! Why: ruling 6 — a `keeper` that is not logged in, whose device was never
//! approved, or whose persistent-login session lapsed must read as
//! [`crate::SecretsError::BackendLocked`], whose hint names the human step.
//! Ruling 3 — a miss never comes from output text, only from a successful
//! listing, so [`MISSING`] is empty and the runner can never return
//! `Verdict::Missing` for Keeper.
//! What: [`LOCKED`], matched ASCII case-insensitively by the shared
//! classifier on a failed run's stderr, and by the backend on the stdout of
//! a listing that exited 0 but did not parse.
//!
//! UNCONFIRMED: no phrase below is quoted in Keeper's public docs, and no
//! run against a real account has pinned one; they are the narrowest text
//! that names each case in Commander's reported messages. A phrase that
//! never matches turns a locked run into a plain backend error, which still
//! fails closed. The Keeper live check pins them.
//! Test: `keeper_marker_table_is_narrow_and_has_no_miss_phrases`,
//! `keeper_locked_output_is_backend_locked_and_names_device_approval`.

/// No stderr phrase means "not found" for Keeper (#7519 ruling 3).
pub(super) const MISSING: &[&str] = &[];

/// Phrases meaning Commander is logged out, unapproved, or lapsed.
pub(super) const LOCKED: &[&str] = &[
    // No session for the config file's user.
    "not logged in",
    "login required",
    // The device was never approved, or its approval was revoked.
    "device is not approved",
    "device approval",
    // Persistent login is off or its session token expired.
    "persistent login",
    "session token expired",
    "session_token_expired",
    // A second factor would be needed, and nothing can answer it.
    "two-factor",
];
