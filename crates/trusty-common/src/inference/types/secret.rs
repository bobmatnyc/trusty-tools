//! [`SecretString`] — a credential wrapper that never prints its value.
//!
//! Why: the configurator ([`super::super::configurator`]) resolves an API key
//! and carries it inside [`super::super::configurator::ResolvedProvider`] so an
//! adapter can be built from it. That resolved value must never leak into a log
//! line, a `Debug` dump, or a serialised payload — slice 1's review caught
//! exactly this class of Debug-leak. Wrapping the key in this newtype makes the
//! leak-safe behaviour the default rather than something each call site must
//! remember.
//! What: [`SecretString`] holds a `String` whose `Debug`/`Display` write the
//! fixed constant `REDACTED`. That string is computed from no part of the
//! wrapped value, so it satisfies DOC-45 `C-8.2` — the rendered form contains
//! no substring of the value and does not vary with it, not even in length. It
//! does NOT derive `Serialize`, so it can never be written to the wire. The raw
//! value is reachable only via the explicit [`SecretString::expose`] method.
//!
//! Until #4632 the two impls rendered [`crate::credentials::redact_secret`]'s
//! four-character head preview instead, so anything deriving `Debug` around a
//! `SecretString` printed the head of a live API key.
//!
//! **Superseded by [`crate::credentials::Secret`] (#4565).** That type remains
//! the canonical credential wrapper going forward: it matches this type's
//! rendering guarantee and adds compile-time refusal of `Serialize`,
//! `Deserialize`, and `Clone`. `SecretString` cannot simply become an alias for
//! it, because the inference stack relies on `Clone`/`PartialEq`; collapsing
//! the two is a deliberate follow-up. [`SecretString::into_secret`] is the
//! one-line migration.
//! Test: inline `tests` — `debug_and_display_are_value_independent`,
//! `rendering_contains_no_substring_of_the_value`, `secret_debug_is_redacted`,
//! `secret_display_is_redacted`, `secret_expose_returns_raw`,
//! `secret_string_converts_to_the_canonical_secret`.

use std::fmt;

use crate::credentials::Secret;

/// The entire rendered form of a [`SecretString`], for both `Debug` and
/// `Display`. A constant, never a function of the wrapped value — DOC-45
/// `C-8.2`.
const REDACTED: &str = "SecretString(<redacted>)";

/// A string credential that redacts itself in `Debug`/`Display`.
///
/// Why: prevents an API key from ever reaching a log or panic message by
/// accident — the only way to read the raw value is the named, greppable
/// [`Self::expose`] call, so credential handling is auditable.
/// What: a transparent wrapper over `String` with hand-written `Debug`/`Display`
/// that both write the constant `REDACTED` and read nothing from the value.
/// Intentionally NOT `Serialize`, `Clone`-audited (clone is allowed — the value
/// is already in memory), and NOT `Deref` (that would re-expose the raw value
/// implicitly).
/// Test: `debug_and_display_are_value_independent`,
/// `rendering_contains_no_substring_of_the_value`, `secret_expose_returns_raw`.
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

    /// Convert into the canonical [`Secret`] wrapper (#4565).
    ///
    /// Why: the migration path off this type. Both render value-independently
    /// since #4632; what `Secret<String>` adds on top is compile-time refusal of
    /// `Serialize`, `Deserialize`, and `Clone`, so this conversion only ever
    /// tightens the guarantees. There is deliberately no conversion back.
    /// What: moves the wrapped string into a `Secret`.
    /// Test: `secret_string_converts_to_the_canonical_secret`.
    pub fn into_secret(self) -> Secret<String> {
        Secret::new(self.0)
    }
}

impl fmt::Debug for SecretString {
    /// Render a fixed redaction, never any part of the raw value.
    ///
    /// Why: a `#[derive(Debug)]` struct that transitively contains a
    /// `SecretString` (e.g. `ResolvedProvider`) must be safe to log; this impl
    /// is what makes that true. It reads `self.0` not at all, so there is no
    /// value for a log line, a panic message, or an error chain to carry.
    /// What: writes `REDACTED`.
    /// Test: `debug_and_display_are_value_independent`,
    /// `rendering_contains_no_substring_of_the_value`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // #4632: a four-character head narrows a brute-force space; render a
        // constant instead of a preview derived from the value.
        f.write_str(REDACTED)
    }
}

impl fmt::Display for SecretString {
    /// Render a fixed redaction, never any part of the raw value.
    ///
    /// Why: `Display` is the surface most likely to reach a user-facing message
    /// or a `format!` in a log; it must be as safe as `Debug`.
    /// What: writes `REDACTED`, identically to `Debug`.
    /// Test: `debug_and_display_are_value_independent`,
    /// `rendering_contains_no_substring_of_the_value`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // #4632: see the `Debug` impl above — the two must not diverge.
        f.write_str(REDACTED)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: no part of the raw value may appear in a `Debug` dump — head
    /// included. This test previously asserted the opposite of its own name:
    /// `dumped.contains("chars")` pinned the `sk-o…(25 chars)` preview shape in
    /// place, which is why #4632's leak survived a suite that looked like it
    /// covered this.
    /// Test: itself.
    #[test]
    fn secret_debug_is_redacted() {
        let s = SecretString::new("sk-or-verysecretvalue1234"); // pragma: allowlist secret
        let dumped = format!("{s:?}");
        assert!(!dumped.contains("verysecretvalue"), "leaked: {dumped}");
        assert!(!dumped.contains("sk-o"), "head leaked: {dumped}");
        assert!(!dumped.contains("chars"), "length leaked: {dumped}");
        assert_eq!(dumped, REDACTED);
    }

    /// Why: `Display` must give away exactly as little as `Debug` — #4632
    /// leaked the same head through both.
    /// Test: itself.
    #[test]
    fn secret_display_is_redacted() {
        let s = SecretString::new("sk-or-verysecretvalue1234"); // pragma: allowlist secret
        let shown = format!("{s}");
        assert!(!shown.contains("verysecretvalue"), "leaked: {shown}");
        assert!(!shown.contains("sk-o"), "head leaked: {shown}");
        assert_eq!(shown, REDACTED);
    }

    /// Why: `expose` is the one sanctioned path back to the raw value.
    /// Test: itself.
    #[test]
    fn secret_expose_returns_raw() {
        let s = SecretString::new("raw-key"); // pragma: allowlist secret
        assert_eq!(s.expose(), "raw-key");
    }

    /// A spread of realistic credential shapes plus deterministic
    /// pseudo-random values, mirroring the input set
    /// `crate::credentials::secret`'s properties are quantified over.
    fn specimens() -> Vec<String> {
        let mut v: Vec<String> = [
            "",
            "a",
            "secret",
            // pragma: allowlist secret
            "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
            // pragma: allowlist secret
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.dBjftJeZ4CVP",
            "パスワード",
            "SecretString(<redacted>)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        for len in 1..=64 {
            let mut s = String::with_capacity(len);
            for _ in 0..len {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let byte = ((state >> 33) % 94) as u8 + 33; // printable ASCII
                s.push(byte as char);
            }
            v.push(s);
        }
        v
    }

    /// Why: the strongest statement of DOC-45 `C-8.2` — the rendering does not
    /// *depend* on the wrapped value at all, so no information about it can
    /// have survived. This is the regression test for #4632: before the fix
    /// `Debug` rendered a four-character head plus the byte length, both of
    /// which vary with the value.
    /// Test: itself.
    #[test]
    fn debug_and_display_are_value_independent() {
        let baseline_debug = format!("{:?}", SecretString::new(""));
        let baseline_display = format!("{}", SecretString::new(""));
        for value in specimens() {
            let s = SecretString::new(value.clone());
            assert_eq!(
                format!("{s:?}"),
                baseline_debug,
                "Debug varied with the value: {value:?}"
            );
            assert_eq!(
                format!("{s}"),
                baseline_display,
                "Display varied with the value: {value:?}"
            );
        }
    }

    /// Why: `C-8.2` as literally written — "the rendered form contains no
    /// substring of it". Quantified over every contiguous substring of length
    /// ≥ 3 of every credential-shaped specimen. Specimens shorter than 8
    /// characters are not credential-shaped and are skipped; a candidate that
    /// already appears in the rendering of the *empty* secret is coincidence,
    /// not disclosure, and is exempt — `debug_and_display_are_value_independent`
    /// is the stronger property that makes that exemption safe.
    /// Test: itself.
    #[test]
    fn rendering_contains_no_substring_of_the_value() {
        const MIN_DISCLOSURE_LEN: usize = 3;
        const MIN_CREDENTIAL_LEN: usize = 8;
        let empty = SecretString::new("");
        let baseline = format!("{empty:?} {empty}");
        for value in specimens() {
            let chars: Vec<char> = value.chars().collect();
            if chars.len() < MIN_CREDENTIAL_LEN {
                continue;
            }
            let s = SecretString::new(value.clone());
            let rendered = format!("{s:?} {s}");
            for start in 0..chars.len() {
                for end in (start + MIN_DISCLOSURE_LEN)..=chars.len() {
                    let candidate: String = chars[start..end].iter().collect();
                    if baseline.contains(&candidate) {
                        continue; // present regardless of the value; not disclosure
                    }
                    assert!(
                        !rendered.contains(&candidate),
                        "rendering {rendered:?} disclosed {candidate:?} from {value:?}"
                    );
                }
            }
        }
    }

    /// Why: the migration path off this type must be lossless, and neither side
    /// of it may disclose the value. Before #4632 this test asserted the source
    /// type *did* leak a head (`leaky.contains("sk-o")`); both sides now redact,
    /// and what `Secret` still adds is compile-time refusal of `Serialize`,
    /// `Deserialize`, and `Clone`.
    /// Test: itself.
    #[test]
    fn secret_string_converts_to_the_canonical_secret() {
        let s = SecretString::new("sk-or-verysecretvalue1234"); // pragma: allowlist secret
        let before = format!("{s:?}");
        assert!(!before.contains("sk-o"), "head leaked: {before}");

        let tightened = s.into_secret();
        assert_eq!(tightened.expose(), "sk-or-verysecretvalue1234");
        let rendered = format!("{tightened:?} {tightened}");
        assert!(!rendered.contains("sk-o"), "head survived: {rendered}");
        assert!(!rendered.contains("verysecretvalue"), "leaked: {rendered}");
    }
}
