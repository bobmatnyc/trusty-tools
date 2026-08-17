//! [`Secret`] — the wrapper a resolved credential is returned in, and the
//! reason a resolved credential cannot be stored, serialised, or printed
//! (issue #4565, DOC-45 `C-8.2`/`C-8.3`).
//!
//! # Spec References
//!
//! - [`SPEC-CREDAUTH-08~draft`](docs/specs/DOC-45-credential-authority-model.md#SPEC-CREDAUTH-08~draft)
//!
//! Why: redaction that a call site has to remember is redaction that eventually
//! does not happen. `C-8.2` therefore requires the *type* to be unable to
//! render its value, and `C-8.3` requires a resolved credential never to be
//! returned by value from a function whose result is serialised into a
//! `ToolResult` — "where the type system permits, pinned at compile time
//! rather than by review". This module is that pin.
//!
//! What: [`Secret`] renders a **constant** in `Debug`/`Display` — not a
//! truncated preview, not a length, nothing derived from the value at all — and
//! deliberately implements **neither** `Serialize`, `Deserialize`, `Clone`,
//! `Deref`, `PartialEq`, nor `AsRef`. Each omission is load-bearing; see
//! [`Secret`]'s own docs. The value is reachable only through the named,
//! greppable [`Secret::expose`].
//!
//! Relationship to [`crate::inference::types::SecretString`]: that type came
//! first (#2402) and rendered a four-character head preview via
//! [`super::redact_secret`] until #4632 fixed it; it now renders its own
//! constant and meets `C-8.2` too. What still separates the two is the rest of
//! the list above — `SecretString` derives `Clone` and `PartialEq`, which
//! `Secret` deliberately does not, so it cannot become an alias.
//! [`SecretString::into_secret`](crate::inference::types::SecretString::into_secret)
//! converts one to the other; collapsing the two is left to a follow-up.
//!
//! Test: `tests::debug_and_display_are_value_independent`,
//! `tests::rendering_contains_no_substring_of_the_value`,
//! `tests::expose_returns_the_raw_value`, and the compile-time assertions in
//! `not_serialize_not_clone`.

use std::fmt;

/// The exact text [`Secret`] renders in `Debug` and `Display`.
///
/// Why: a single constant makes "the rendered form is independent of the value"
/// checkable by inspection as well as by test.
const REDACTED: &str = "Secret(<redacted>)";

/// A resolved credential. Never prints, never serialises, never clones.
///
/// Why: the resolved value is the one thing in this subsystem whose accidental
/// disclosure is unrecoverable. Every capability this type *lacks* is a leak
/// path it closes:
///
/// | Not implemented | What it would let you do |
/// |---|---|
/// | `Serialize` / `Deserialize` | put a secret in a config row, a store row, or a `ToolResult` — `C-8.1`, `C-8.3` |
/// | `Clone` | duplicate one into a long-lived struct, defeating `C-8.4`'s bounded window |
/// | `Deref` / `AsRef` | expose the value implicitly, with no greppable call site |
/// | `PartialEq` | compare a secret in non-constant time, and encourage secrets as map keys |
/// | a value-derived `Debug`/`Display` | leak a prefix into a log, which is `SecretString`'s existing shortfall |
///
/// The single omission that matters most is `Serialize`: because every config
/// struct in this workspace derives it, a `Secret` **cannot compile** inside
/// one. That is what makes "a config row holds a [`super::CredentialRef`], not
/// a credential" (`C-8.8`) structural rather than aspirational.
///
/// What: a transparent wrapper over `T` with hand-written `Debug`/`Display`
/// that render [`REDACTED`] and read nothing from the value. Constructed by
/// [`Secret::new`], read by [`Secret::expose`].
///
/// Test: `tests::debug_and_display_are_value_independent`,
/// `tests::rendering_contains_no_substring_of_the_value`,
/// `tests::expose_returns_the_raw_value`.
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wrap a resolved credential value.
    ///
    /// Why: one constructor, so every secret enters the type through one door.
    /// Test: `tests::expose_returns_the_raw_value`.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Borrow the raw value.
    ///
    /// Why: authentication needs the actual bytes. The explicit, greppable name
    /// documents the deliberate exposure at the call site — which is exactly
    /// what a `Deref` impl would have hidden. Returns a **borrow**, so using a
    /// secret does not hand the caller an owned copy to stash.
    /// Test: `tests::expose_returns_the_raw_value`.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Consume the wrapper and return the raw value.
    ///
    /// Why: an adapter that must own its credential (an HTTP client built once
    /// per call) would otherwise clone through `expose`, and a clone with no
    /// wrapper is worse than a move. Consuming `self` keeps the count of live
    /// copies at one.
    /// Test: `tests::into_inner_moves_the_value`.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    /// Render [`REDACTED`], reading nothing from the value.
    ///
    /// Why: `C-8.2`. Note the absence of a `T: Debug` bound — the impl *cannot*
    /// consult the value even if it wanted to, which is a stronger statement
    /// than an impl that chooses not to.
    /// Test: `tests::debug_and_display_are_value_independent`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Display for Secret<T> {
    /// Render [`REDACTED`], reading nothing from the value.
    ///
    /// Why: `Display` is the surface most likely to reach a user-facing message
    /// or a `format!` in a log; it must be as blind as `Debug`.
    /// Test: `tests::debug_and_display_are_value_independent`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Compile-time proof that [`Secret`] implements neither `Serialize` nor
/// `Clone` (DOC-45 `C-8.3`, #4565 acceptance criterion 3).
///
/// Why: "a resolved credential is never returned by value from a function whose
/// result is `Serialize`" cannot be asserted by a runtime test — by the time a
/// test could observe the leak, the impl exists. Rust has no `!Trait` bound, so
/// this uses coherence instead: a blanket impl over every `T: Serialize` plus a
/// concrete impl for `Secret<String>` can only coexist while `Secret<String>`
/// does **not** implement `Serialize`. The day someone adds `#[derive(Serialize)]`
/// the two impls overlap and the crate fails to build with E0119.
///
/// What: for each trait, two blanket impls of a private helper — one
/// unconditional, one bounded by the trait under test. Resolving
/// `<Secret<String> as AmbiguousIfImpl<_>>` picks the unconditional impl while
/// `Secret<String>` lacks the trait; the moment it gains the trait both impls
/// apply, inference becomes ambiguous, and the build fails. (This is the
/// mechanism behind `static_assertions::assert_not_impl_all!`, inlined rather
/// than adding a dependency for two assertions.)
/// Test: compiling this module is the test. `tests::compile_time_assertions_hold`
/// records that in the test list so a reader learns the assertions exist.
mod not_serialize_not_clone {
    use super::Secret;

    /// `C-8.3`: a resolved credential must never be `Serialize`, because every
    /// config struct and every `ToolResult` in this workspace derives it.
    const _ASSERT_NOT_SERIALIZE: fn() = || {
        trait AmbiguousIfImpl<A> {
            fn some_item() {}
        }
        impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
        impl<T: ?Sized + serde::Serialize> AmbiguousIfImpl<u8> for T {}
        let _ = <Secret<String> as AmbiguousIfImpl<_>>::some_item;
    };

    /// `C-8.4`: not `Clone`, so a resolved credential cannot be duplicated into
    /// a longer-lived home than the call that resolved it.
    const _ASSERT_NOT_CLONE: fn() = || {
        trait AmbiguousIfImpl<A> {
            fn some_item() {}
        }
        impl<T> AmbiguousIfImpl<()> for T {}
        impl<T: Clone> AmbiguousIfImpl<u8> for T {}
        let _ = <Secret<String> as AmbiguousIfImpl<_>>::some_item;
    };

    /// `C-8.1`: not `Deserialize` either, so a hand-edited config cannot
    /// construct one — the reverse of the `Serialize` door.
    const _ASSERT_NOT_DESERIALIZE: fn() = || {
        trait AmbiguousIfImpl<A> {
            fn some_item() {}
        }
        impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
        impl<T: serde::de::DeserializeOwned> AmbiguousIfImpl<u8> for T {}
        let _ = <Secret<String> as AmbiguousIfImpl<_>>::some_item;
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spread of realistic credential shapes: long, short, high-entropy,
    /// low-entropy, non-ASCII, and empty. `C-8.2` says "property-tested"; this
    /// is the input set the two properties below are quantified over.
    fn specimens() -> Vec<String> {
        let mut v: Vec<String> = [
            "",
            "a",
            "ab",
            "secret",
            // pragma: allowlist secret
            "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
            // pragma: allowlist secret
            concat!("xo", "xb", "-2314151234-2321313111-QwErTyUiOpAsDf"),
            // pragma: allowlist secret
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.dBjftJeZ4CVP",
            // pragma: allowlist secret
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEow==\n",
            "パスワード",
            "Secret(<redacted>)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // Deterministic pseudo-random values, so the property is quantified
        // over more than a hand-picked table without adding a proptest dep.
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

    /// Why: the strongest possible statement of `C-8.2` — not "the rendering
    /// omits the value" but "the rendering does not *depend* on the value at
    /// all". If two secrets with nothing in common render identically, no
    /// information about either can have survived.
    /// Test: itself.
    #[test]
    fn debug_and_display_are_value_independent() {
        let baseline_debug = format!("{:?}", Secret::new(String::new()));
        let baseline_display = format!("{}", Secret::new(String::new()));
        for value in specimens() {
            let s = Secret::new(value.clone());
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
    /// ≥ 3 of every credential-shaped specimen.
    ///
    /// Two exclusions, both deliberate and both necessary for the property to
    /// mean anything. Specimens shorter than 8 characters are dropped: they are
    /// not credential-shaped, and `C-8.2` is about credentials. And a candidate
    /// that already appears in the rendering of the *empty* secret is exempt:
    /// the constant `Secret(<redacted>)` contains `ecr`, so an English word
    /// like `secret` trips a naive version of this assertion without anything
    /// having leaked. The exemption is what keeps the test measuring
    /// disclosure rather than coincidence — and
    /// `debug_and_display_are_value_independent` is the stronger property that
    /// makes the exemption safe, since a rendering that never varies with the
    /// value cannot be disclosing it.
    /// Test: itself.
    #[test]
    fn rendering_contains_no_substring_of_the_value() {
        const MIN_DISCLOSURE_LEN: usize = 3;
        const MIN_CREDENTIAL_LEN: usize = 8;
        let baseline = format!("{:?} {}", Secret::new(String::new()), Secret::new(""));
        for value in specimens() {
            let chars: Vec<char> = value.chars().collect();
            if chars.len() < MIN_CREDENTIAL_LEN {
                continue;
            }
            let rendered = format!("{:?} {}", Secret::new(value.clone()), Secret::new(&value));
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

    /// Why: `expose` is the one sanctioned path back to the raw value, and it
    /// must be lossless — a redacting accessor would be worse than none.
    /// Test: itself.
    #[test]
    fn expose_returns_the_raw_value() {
        // pragma: allowlist secret
        let s = Secret::new("raw-key".to_string());
        assert_eq!(s.expose(), "raw-key");
        let bytes = Secret::new(vec![1u8, 2, 3]);
        assert_eq!(bytes.expose(), &[1u8, 2, 3]);
    }

    /// Why: `into_inner` must move rather than copy, keeping the number of live
    /// copies at one — the point of not deriving `Clone`.
    /// Test: itself.
    #[test]
    fn into_inner_moves_the_value() {
        // pragma: allowlist secret
        let s = Secret::new("raw-key".to_string());
        assert_eq!(s.into_inner(), "raw-key");
    }

    /// Why: names the compile-time assertions so a reader of the test list
    /// learns they exist. The assertion itself is structural — if
    /// `Secret<String>` ever gains `Serialize`, `Deserialize`, or `Clone`,
    /// `super::not_serialize_not_clone` fails to compile and this test never
    /// runs at all.
    /// Test: itself, plus compilation of `super::not_serialize_not_clone`.
    #[test]
    fn compile_time_assertions_hold() {
        // Reaching this line at all means the crate compiled, which means the
        // three ambiguity assertions in `not_serialize_not_clone` resolved.
        assert_eq!(REDACTED, "Secret(<redacted>)");
    }
}
