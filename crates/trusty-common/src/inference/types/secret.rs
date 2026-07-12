//! [`SecretString`] — a credential wrapper that never prints its value.
//!
//! Why: the configurator ([`super::super::configurator`]) resolves an API key
//! and carries it inside [`super::super::configurator::ResolvedProvider`] so an
//! adapter can be built from it. That resolved value must never leak into a log
//! line, a `Debug` dump, or a serialised payload — slice 1's review caught
//! exactly this class of Debug-leak. Wrapping the key in this newtype makes the
//! leak-safe behaviour the default rather than something each call site must
//! remember.
//! What: [`SecretString`] holds a `String` whose `Debug`/`Display` render the
//! redacted preview from [`crate::inference::credentials::redact_secret`]. It
//! does NOT derive `Serialize`, so it can never be written to the wire. The raw
//! value is reachable only via the explicit [`SecretString::expose`] method.
//! Test: inline `tests` — `secret_debug_is_redacted`, `secret_display_is_redacted`,
//! `secret_expose_returns_raw`.

use std::fmt;

use crate::inference::credentials::redact_secret;

/// A string credential that redacts itself in `Debug`/`Display`.
///
/// Why: prevents an API key from ever reaching a log or panic message by
/// accident — the only way to read the raw value is the named, greppable
/// [`Self::expose`] call, so credential handling is auditable.
/// What: a transparent wrapper over `String` with hand-written `Debug`/`Display`
/// that delegate to [`redact_secret`]. Intentionally NOT `Serialize`,
/// `Clone`-audited (clone is allowed — the value is already in memory), and
/// NOT `Deref` (that would re-expose the raw value implicitly).
/// Test: `secret_debug_is_redacted`, `secret_expose_returns_raw`.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Wrap a raw credential value.
    ///
    /// Why: the single constructor makes every secret enter the type through one
    /// door.
    /// What: moves `value` into the wrapper.
    /// Test: `secret_expose_returns_raw`.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the raw secret value.
    ///
    /// Why: adapter construction (in #2403) needs the actual key to authenticate;
    /// the explicit method name documents the deliberate exposure at the call
    /// site rather than hiding it behind `Deref`/`Display`.
    /// What: returns the wrapped string slice unredacted.
    /// Test: `secret_expose_returns_raw`.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    /// Render the redacted preview, never the raw value.
    ///
    /// Why: a `#[derive(Debug)]` struct that transitively contains a
    /// `SecretString` (e.g. `ResolvedProvider`) must be safe to log; this impl
    /// guarantees that.
    /// What: writes `SecretString(<redacted>)`.
    /// Test: `secret_debug_is_redacted`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretString({})", redact_secret(&self.0))
    }
}

impl fmt::Display for SecretString {
    /// Render the redacted preview, never the raw value.
    ///
    /// Why: `Display` is the surface most likely to reach a user-facing message
    /// or a `format!` in a log; it must be as safe as `Debug`.
    /// What: writes the redacted preview only.
    /// Test: `secret_display_is_redacted`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact_secret(&self.0))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the raw value must never appear in a `Debug` dump.
    /// Test: itself.
    #[test]
    fn secret_debug_is_redacted() {
        let s = SecretString::new("sk-or-verysecretvalue1234"); // pragma: allowlist secret
        let dumped = format!("{s:?}");
        assert!(!dumped.contains("verysecretvalue"), "leaked: {dumped}");
        assert!(dumped.contains("chars"), "not redacted shape: {dumped}");
    }

    /// Why: the raw value must never appear in a `Display` render.
    /// Test: itself.
    #[test]
    fn secret_display_is_redacted() {
        let s = SecretString::new("sk-or-verysecretvalue1234"); // pragma: allowlist secret
        let shown = format!("{s}");
        assert!(!shown.contains("verysecretvalue"), "leaked: {shown}");
    }

    /// Why: `expose` is the one sanctioned path back to the raw value.
    /// Test: itself.
    #[test]
    fn secret_expose_returns_raw() {
        let s = SecretString::new("raw-key"); // pragma: allowlist secret
        assert_eq!(s.expose(), "raw-key");
    }
}
