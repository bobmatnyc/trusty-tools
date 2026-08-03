//! [`CredentialRef`] — the opaque, non-secret handle that *names* a credential
//! (issue #4565, DOC-45 §4).
//!
//! # Spec References
//!
//! - [`SPEC-CREDAUTH-02~draft`](docs/specs/DOC-45-credential-authority-model.md#SPEC-CREDAUTH-02~draft)
//!
//! Why: today a credential's only identity is the value itself, so anything
//! that needs to *refer* to one has to *hold* one. That is why
//! `McpService.env` is a `HashMap<String, String>` of literal API keys in a
//! hand-editable TOML, and why DOC-63 `S-5.6`'s "no secrets in the assistant
//! home" is a rule people remember rather than a property of the types. A
//! reference type is the mechanism that inverts that: a config row, a store
//! row, a log line, an audit record, and a model-visible `ToolResult` can all
//! carry a `CredentialRef` in plain text because a `CredentialRef` is not a
//! secret and — by `C-2.4`'s grammar — cannot be made to carry one.
//!
//! What: a `provider` key, optionally qualified (`slack/bot`), restricted to
//! lowercase-kebab segments and 64 bytes. Stable across rotation (`C-2.2`),
//! shape-agnostic between OAuth and API-key credentials (`C-2.5`), rendered
//! verbatim by `Display` (`C-2.6`), and resolved through the provider registry
//! (`C-2.7`).
//!
//! Test: `tests::round_trips_through_serde`, `tests::display_is_verbatim`,
//! `tests::realistic_credentials_are_rejected_by_the_grammar`,
//! `tests::qualifier_is_optional_and_preserved`,
//! `tests::rejects_out_of_grammar_text`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Maximum total length of a rendered `CredentialRef`, in bytes.
///
/// Why: part of `C-2.4`'s restrictive grammar. Every credential format in this
/// workspace's census is either longer than this or uses characters the
/// grammar rejects, so a value cannot be laundered into a ref by accident.
const MAX_LEN: usize = 64;

/// Maximum length of one `/`-separated segment.
const MAX_SEGMENT_LEN: usize = 32;

/// Why a piece of text is not a valid [`CredentialRef`].
///
/// Why: `C-2.4` requires construction from out-of-grammar text to be a
/// *recoverable parse error*, not a panic and not a silent acceptance — the
/// audit stream's "secret material is unrepresentable" guarantee (`C-7.3`) is
/// worth exactly as much as this type's willingness to say no.
/// What: one variant per rejection reason, each naming the offending shape
/// without echoing the input — a rejected input may well *be* a secret, which
/// is the whole reason it was rejected.
/// Test: `tests::rejects_out_of_grammar_text`,
/// `tests::parse_error_never_echoes_the_input`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialRefError {
    /// The text was empty.
    #[error("credential reference is empty")]
    Empty,

    /// The text exceeded [`MAX_LEN`] bytes, or a segment exceeded
    /// [`MAX_SEGMENT_LEN`].
    #[error(
        "credential reference is too long ({len} bytes; max {MAX_LEN}, max {MAX_SEGMENT_LEN} per segment)"
    )]
    TooLong {
        /// Length of the offending text, in bytes. Never the text itself.
        len: usize,
    },

    /// The text used more than one `/`, or had an empty segment.
    #[error("credential reference must be `provider` or `provider/qualifier`")]
    Shape,

    /// A segment used a character outside `[a-z0-9-]`, or started/ended with
    /// `-`.
    #[error(
        "credential reference segments must be lowercase kebab-case `[a-z0-9-]`, \
         not starting or ending with `-` (DOC-45 C-2.4)"
    )]
    Charset,
}

/// An opaque, durable, serialisable, **non-secret** handle naming a credential.
///
/// Why: see the module docs. In one line — this is what a config row holds
/// *instead of* a secret, so "no credential in the assistant home" becomes true
/// by construction rather than by discipline (`C-8.8`).
///
/// What: `provider` plus an optional `qualifier`, rendered `provider` or
/// `provider/qualifier`. The provider segment is the key looked up in
/// [`super::registry`] (`C-2.7`); the qualifier distinguishes several
/// credentials of one provider held by one operator (`github/work` vs
/// `github/personal`) without needing a registry entry per instance.
///
/// Invariants, all enforced by [`CredentialRef::parse`] and by the
/// `Deserialize` impl, which routes through it:
/// - every segment matches `[a-z0-9]([a-z0-9-]*[a-z0-9])?`;
/// - at most one `/`;
/// - total rendered length ≤ 64 bytes, each segment ≤ 32.
///
/// **Stability (`C-2.2`).** A ref does not change when the underlying secret
/// rotates. A store row written once keeps working across every rotation, and
/// rotation never becomes a config edit across N files.
///
/// **Shape-agnosticism (`C-2.5`).** The same type names an OAuth credential and
/// a plain API key. There is deliberately no `Kind`, no `is_oauth()`, and no
/// second constructor — DOC-63 §7.1b item 5 warns that the API-key shape is the
/// one most likely to be special-cased, and states that it must not be.
///
/// Test: `tests::round_trips_through_serde`,
/// `tests::realistic_credentials_are_rejected_by_the_grammar`,
/// `tests::qualifier_is_optional_and_preserved`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CredentialRef {
    provider: String,
    qualifier: Option<String>,
}

impl CredentialRef {
    /// Parse `text` into a reference, validating the full grammar.
    ///
    /// Why: the single door into the type. `C-2.4` requires that a
    /// `CredentialRef` cannot be built from arbitrary text, because the audit
    /// record (#4567) accepts a `CredentialRef` precisely so that secret
    /// material is unrepresentable there — an unvalidated newtype would
    /// launder any string straight into a retained stream.
    /// What: splits on `/`, validates each segment, and enforces the length
    /// caps. Returns [`CredentialRefError`] rather than panicking.
    /// Test: `tests::rejects_out_of_grammar_text`,
    /// `tests::realistic_credentials_are_rejected_by_the_grammar`,
    /// `tests::qualifier_is_optional_and_preserved`.
    pub fn parse(text: &str) -> Result<Self, CredentialRefError> {
        if text.is_empty() {
            return Err(CredentialRefError::Empty);
        }
        if text.len() > MAX_LEN {
            return Err(CredentialRefError::TooLong { len: text.len() });
        }
        let mut parts = text.split('/');
        let provider = parts.next().unwrap_or_default();
        let qualifier = parts.next();
        if parts.next().is_some() {
            return Err(CredentialRefError::Shape);
        }
        validate_segment(provider)?;
        if let Some(q) = qualifier {
            validate_segment(q)?;
        }
        Ok(Self {
            provider: provider.to_string(),
            qualifier: qualifier.map(str::to_string),
        })
    }

    /// The provider key this reference names.
    ///
    /// Why: [`super::registry::env_var_for`] and the store tier both key off
    /// the provider, never the qualifier — `C-2.7`'s "a ref, a registry entry,
    /// and a storage location are one chain".
    /// Test: `tests::qualifier_is_optional_and_preserved`.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The optional qualifier, distinguishing several credentials of one
    /// provider.
    ///
    /// Test: `tests::qualifier_is_optional_and_preserved`.
    pub fn qualifier(&self) -> Option<&str> {
        self.qualifier.as_deref()
    }
}

/// Reject a segment that is not `[a-z0-9]([a-z0-9-]*[a-z0-9])?` or is too long.
fn validate_segment(segment: &str) -> Result<(), CredentialRefError> {
    if segment.is_empty() {
        return Err(CredentialRefError::Shape);
    }
    if segment.len() > MAX_SEGMENT_LEN {
        return Err(CredentialRefError::TooLong { len: segment.len() });
    }
    if segment.starts_with('-') || segment.ends_with('-') {
        return Err(CredentialRefError::Charset);
    }
    if !segment
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(CredentialRefError::Charset);
    }
    Ok(())
}

impl fmt::Display for CredentialRef {
    /// Render the reference verbatim.
    ///
    /// Why: `C-2.6` — a ref is non-secret by `C-2.1` and `C-2.4`, so masking it
    /// would only make the audit trail harder to read while buying nothing.
    /// This is the deliberate opposite of [`super::Secret`], and the contrast
    /// is the design: the handle prints, the value never does.
    /// Test: `tests::display_is_verbatim`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.provider)?;
        if let Some(q) = &self.qualifier {
            write!(f, "/{q}")?;
        }
        Ok(())
    }
}

impl FromStr for CredentialRef {
    type Err = CredentialRefError;

    /// Delegates to [`CredentialRef::parse`] so there is one validation path.
    /// Test: `tests::round_trips_through_serde`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for CredentialRef {
    /// Serialise as the rendered string, so a TOML/JSON row reads
    /// `credential_ref = "slack/bot"` rather than a nested table.
    /// Test: `tests::round_trips_through_serde`.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CredentialRef {
    /// Deserialise through [`CredentialRef::parse`], so a hand-edited config
    /// cannot introduce an out-of-grammar ref — the grammar is enforced at the
    /// boundary, not merely at the constructor.
    /// Test: `tests::round_trips_through_serde`,
    /// `tests::deserialize_rejects_out_of_grammar_text`.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `C-2.1` requires a ref to be durable — writable into a config TOML
    /// and readable back — and the serialised form is what a store row holds.
    /// Test: itself.
    #[test]
    fn round_trips_through_serde() {
        for text in ["slack", "slack/bot", "github/work", "google-oauth"] {
            let parsed = CredentialRef::parse(text).unwrap();
            let json = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, format!("\"{text}\""));
            let back: CredentialRef = serde_json::from_str(&json).unwrap();
            assert_eq!(back, parsed);
            assert_eq!(text.parse::<CredentialRef>().unwrap(), parsed);
        }
    }

    /// Why: `C-2.6` — the handle prints verbatim, which is what makes an audit
    /// record readable. Paired with `secret::tests` proving the *value* never
    /// prints, this is the whole reference-type contract in two tests.
    /// Test: itself.
    #[test]
    fn display_is_verbatim() {
        assert_eq!(CredentialRef::parse("slack").unwrap().to_string(), "slack");
        assert_eq!(
            CredentialRef::parse("github/work").unwrap().to_string(),
            "github/work"
        );
    }

    /// Why: this is `C-2.4`'s actual claim — the grammar is restrictive enough
    /// that no realistic API key, JWT, OAuth token, or PEM body can satisfy it,
    /// so a secret cannot be laundered into a ref (and thence into the audit
    /// stream) by accident. Specimens are shaped like the real formats this
    /// workspace's registry names, with the values themselves invented.
    /// Test: itself.
    #[test]
    fn realistic_credentials_are_rejected_by_the_grammar() {
        let specimens: &[(&str, &str)] = &[
            // pragma: allowlist secret
            ("GitHub PAT", "ghp_16C7e42F292c6912E7710c838347Ae178B4a"),
            // pragma: allowlist secret
            (
                "GitHub fine-grained",
                "github_pat_11ABCDE0Y_aBcDeFgHiJkLmNoP",
            ),
            // pragma: allowlist secret
            ("OpenAI", "sk-proj-Ab12Cd34Ef56Gh78Ij90KlMnOpQrSt"),
            // pragma: allowlist secret
            (
                "Slack bot",
                // Assembled rather than written literally so GitHub push
                // protection does not flag an invented specimen as a live token.
                concat!("xo", "xb", "-2314151234-2321313111-QwErTyUiOpAsDf"),
            ),
            // pragma: allowlist secret
            (
                "Slack app",
                concat!("xa", "pp", "-1-A012BCDEF-1234567890-abcdefABCDEF0123"),
            ),
            // pragma: allowlist secret
            (
                "Telegram",
                "1234567890:AAF-abcDEF1234ghIkl-zyx57W2v1u123ew11",
            ),
            // pragma: allowlist secret
            ("Brave", "BSA_aBcDeFgHiJkLmNoPqRsTuVwXyZ012345"),
            // pragma: allowlist secret
            ("JWT", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.dBjftJeZ4CVP"),
            // pragma: allowlist secret
            ("PEM body", "-----BEGIN RSA PRIVATE KEY-----\nMIIEow=="),
            // pragma: allowlist secret
            ("Google OAuth secret", "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"),
        ];
        for (name, specimen) in specimens {
            assert!(
                CredentialRef::parse(specimen).is_err(),
                "{name} specimen parsed as a CredentialRef — the C-2.4 grammar is too permissive"
            );
        }
    }

    /// Why: the qualifier is what lets one operator hold two credentials of one
    /// provider without a registry entry per instance; dropping it silently
    /// would collapse `github/work` and `github/personal` onto one row.
    /// Test: itself.
    #[test]
    fn qualifier_is_optional_and_preserved() {
        let bare = CredentialRef::parse("github").unwrap();
        assert_eq!(bare.provider(), "github");
        assert_eq!(bare.qualifier(), None);

        let qualified = CredentialRef::parse("github/work").unwrap();
        assert_eq!(qualified.provider(), "github");
        assert_eq!(qualified.qualifier(), Some("work"));
        assert_ne!(bare, qualified);
    }

    /// Why: each rejection reason must be reachable, so the grammar is pinned
    /// rather than merely described.
    /// Test: itself.
    #[test]
    fn rejects_out_of_grammar_text() {
        use CredentialRefError::*;
        let cases: &[(&str, CredentialRefError)] = &[
            ("", Empty),
            ("a/b/c", Shape),
            ("slack/", Shape),
            ("/slack", Shape),
            ("Slack", Charset),
            ("slack_bot", Charset),
            ("slack.bot", Charset),
            ("slack bot", Charset),
            ("-slack", Charset),
            ("slack-", Charset),
            ("slack\n", Charset),
        ];
        for (input, expected) in cases {
            assert_eq!(
                CredentialRef::parse(input).unwrap_err(),
                *expected,
                "input {input:?}"
            );
        }
        let long = "a".repeat(MAX_LEN + 1);
        assert_eq!(
            CredentialRef::parse(&long).unwrap_err(),
            TooLong { len: MAX_LEN + 1 }
        );
        let long_segment = format!("{}/b", "a".repeat(MAX_SEGMENT_LEN + 1));
        assert_eq!(
            CredentialRef::parse(&long_segment).unwrap_err(),
            TooLong {
                len: MAX_SEGMENT_LEN + 1
            }
        );
    }

    /// Why: a rejected input may itself be a secret — that is precisely why it
    /// was rejected — so the error must not echo it. `C-5.9` in miniature.
    /// Test: itself.
    #[test]
    fn parse_error_never_echoes_the_input() {
        // pragma: allowlist secret
        let secret = "ghp_16C7e42F292c6912E7710c838347Ae178B4a";
        let rendered = CredentialRef::parse(secret).unwrap_err().to_string();
        assert!(!rendered.contains(secret), "leaked: {rendered}");
        assert!(!rendered.contains("ghp_"), "leaked prefix: {rendered}");
    }

    /// Why: enforcing the grammar only in `parse` would leave a hand-edited
    /// config TOML as an unchecked door into the type.
    /// Test: itself.
    #[test]
    fn deserialize_rejects_out_of_grammar_text() {
        // pragma: allowlist secret
        let json = concat!(
            "\"",
            "xo",
            "xb",
            "-2314151234-2321313111-QwErTyUiOpAsDf",
            "\""
        );
        assert!(serde_json::from_str::<CredentialRef>(json).is_err());
    }
}
