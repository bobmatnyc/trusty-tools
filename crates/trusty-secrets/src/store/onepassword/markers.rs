//! The `op` stderr phrases the shared runner classifies by (#7519 A3).
//!
//! Why: a missing item must read as a miss (`get` → `Ok(None)`, `delete` →
//! `Ok(false)`), and a locked or signed-out CLI as an error. `op` says
//! which only in its stderr text, so these phrases decide it. They are kept
//! in this one table so the live check can pin them in one place.
//! What: [`MISSING`] and [`LOCKED`], handed to the runner's classifier,
//! which matches them ASCII case-insensitively, locked first, and only for
//! a run with no stdin. A failure matching neither is `Verdict::Other`, an
//! error.
//!
//! UNCONFIRMED: every phrase below is taken from `op` 2.x error text as
//! documented and reported, not from a run against a real account. The
//! 1Password live check pins each one. Until then a phrase that never
//! matches turns a miss into an error, which fails closed. A phrase that is
//! too broad could turn a real failure into a miss, so each phrase is the
//! narrowest text that names its case; a generic "not found" is never one.
//! Test: `onepassword_marker_table_is_narrow_and_disjoint`,
//! `onepassword_missing_is_none_and_failures_are_errors`.

/// stderr phrases meaning the item or vault does not exist.
pub(super) const MISSING: &[&str] = &[
    // `op read`, `op item edit|delete <id>`: the item is gone.
    "isn't an item",
    // `op item list --vault <v>`: this account has no such vault, so it
    // holds no item of this backend's either.
    "isn't a vault",
];

/// stderr phrases meaning `op` is locked or signed out.
pub(super) const LOCKED: &[&str] = &[
    // No session, no app integration and no service-account token.
    "not currently signed in",
    "account is not signed in",
    "no accounts configured",
    // A session token that has lapsed.
    "session expired",
    // The desktop app's unlock prompt was refused or not answered.
    "authorization prompt dismissed",
    "authorization timeout",
];
