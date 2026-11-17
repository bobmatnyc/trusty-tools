//! Consent scope presets: the allow-list of scope sets `setup` may request.
//!
//! Why: The owner needs a profile that can read Gmail, Drive and Docs and do
//! nothing else (#10746 ok-T20). Accepting free-form scope strings would let a
//! typo or a pasted value request any Google scope, so the choice is a closed
//! enum: one name per scope set, and anything else is an error.
//! What: [`ScopePreset`] names a scope set, [`ScopePreset::parse`] maps the
//! user's `--scopes` / `scopes` value onto it, and [`ScopePreset::scopes`] /
//! [`ScopePreset::scope_string`] give the scopes the consent request carries.
//! Test: `readonly_preset_scope_string_is_exact`,
//! `unknown_scope_preset_is_rejected`, `absent_preset_is_the_full_default`.

use anyhow::{Result, anyhow};

use crate::api::constants::{OAUTH_SCOPES, READONLY_SCOPES};

// #10746: ok-T20 read-only scope preset
/// A named consent scope set.
///
/// Why: The consent flow, the auth URL and the stored token's recorded scopes
/// must all agree on one scope set; passing this value instead of a string
/// keeps them from drifting apart.
/// What: `Full` is [`OAUTH_SCOPES`], the Python-compatible default.
/// `ReadOnly` is [`READONLY_SCOPES`].
/// Test: `readonly_preset_scope_string_is_exact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopePreset {
    /// The full default scope set, [`OAUTH_SCOPES`].
    #[default]
    Full,
    /// The read-only import set, [`READONLY_SCOPES`].
    ReadOnly,
}

/// Preset names a caller may pass, in the order the error message lists them.
const KNOWN_PRESETS: &[&str] = &["readonly"];

impl ScopePreset {
    /// Map an optional preset name onto a preset.
    ///
    /// Why: Both entry points (`setup --scopes`, `add_account`'s `scopes`)
    /// must accept and refuse exactly the same values.
    /// What: `None` gives `Full`, today's behaviour. `Some("readonly")` gives
    /// `ReadOnly`. Any other value, including an empty string or a scope URL,
    /// is an error naming the accepted presets.
    /// Test: `unknown_scope_preset_is_rejected`,
    /// `absent_preset_is_the_full_default`.
    pub fn parse(name: Option<&str>) -> Result<Self> {
        match name {
            None => Ok(Self::Full),
            Some("readonly") => Ok(Self::ReadOnly),
            Some(other) => Err(anyhow!(
                "unknown scope preset '{other}'; accepted presets: {} \
                 (omit the option for the full default scope set)",
                KNOWN_PRESETS.join(", ")
            )),
        }
    }

    /// The scopes this preset requests at consent.
    pub fn scopes(self) -> &'static [&'static str] {
        match self {
            Self::Full => OAUTH_SCOPES,
            Self::ReadOnly => READONLY_SCOPES,
        }
    }

    /// The space-delimited `scope` parameter for the authorization request.
    pub fn scope_string(self) -> String {
        self.scopes().join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the preset must request exactly the five read-only scopes, as
    /// full URLs, in a stable order.
    /// What: compares the whole scope string.
    /// Test: this test.
    #[test]
    fn readonly_preset_scope_string_is_exact() {
        assert_eq!(
            ScopePreset::parse(Some("readonly")).unwrap().scope_string(),
            "openid https://www.googleapis.com/auth/userinfo.email \
             https://www.googleapis.com/auth/gmail.readonly \
             https://www.googleapis.com/auth/drive.readonly \
             https://www.googleapis.com/auth/documents.readonly"
        );
    }

    /// Why: only allow-listed names pass; a free-form scope must never reach
    /// the consent request.
    /// What: a misspelling, a different case, an empty value and a raw scope
    /// URL are each refused, and the error names the accepted preset.
    /// Test: this test.
    #[test]
    fn unknown_scope_preset_is_rejected() {
        for bad in [
            "read-only",
            "READONLY",
            "",
            "full",
            "https://www.googleapis.com/auth/drive",
        ] {
            let err = ScopePreset::parse(Some(bad)).unwrap_err().to_string();
            assert!(err.contains("unknown scope preset"), "{bad:?}: {err}");
            assert!(err.contains("readonly"), "{bad:?}: {err}");
        }
    }

    /// Why: no option must keep today's scope set exactly.
    /// What: `parse(None)` is `Full`, and `Full` requests `OAUTH_SCOPES`.
    /// Test: this test.
    #[test]
    fn absent_preset_is_the_full_default() {
        let preset = ScopePreset::parse(None).unwrap();
        assert_eq!(preset, ScopePreset::Full);
        assert_eq!(preset, ScopePreset::default());
        assert_eq!(preset.scope_string(), OAUTH_SCOPES.join(" "));
    }
}
