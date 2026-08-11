//! The engagement config that ships TO the recipient.
//!
//! Why: the handoff package carries a readable config holding a spend-capped
//! OpenRouter key the owner mints per engagement, plus the audit instructions
//! (#5473). The deliverable that comes BACK carries neither. That asymmetry is
//! the one invariant this module exists to hold:
//!
//! > A credential belongs in the file that goes to the recipient, never in the
//! > file that comes back.
//!
//! What: [`EngagementConfig`], deserialized from TOML, with the key wrapped in
//! [`SecretKey`]. `SecretKey` implements `Deserialize` and deliberately does
//! **not** implement `Serialize`, so a future output artifact that tries to
//! carry the key does not compile — the invariant is enforced by the type
//! system rather than by a reviewer noticing. `Debug` and `Display` both
//! redact, so the key cannot reach a log line or an error message either.
//! Test: `super::config_tests`.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::AuditError;

/// An API key read from the engagement config.
///
/// Why: see the module docs. Three properties, in the order they matter:
/// no `Serialize` (it cannot be written into an output artifact), redacting
/// `Debug` (it cannot reach a `tracing` field or a `{:?}` panic message), and
/// redacting `Display` (it cannot reach an error string). Reading the value
/// requires calling [`SecretKey::expose`], which is greppable.
/// What: a newtype over `String` with hand-written formatting impls.
/// Test: `super::config_tests::the_key_is_redacted_in_debug_and_display`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct SecretKey(String);

impl SecretKey {
    /// Wrap a raw key.
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The raw key, for the one place that hands it to an HTTP client.
    ///
    /// Why: named `expose` so `git grep expose` finds every site that touches
    /// the plaintext.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the configured key is blank.
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

/// Redacted. See the type docs.
impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// Redacted. See the type docs.
impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The engagement config that travels inside the handoff package.
///
/// Why: the recipient can read this file before running anything — that
/// readability is the transparency premise of the whole handoff (#5473), so the
/// schema stays plain TOML with no encoding step.
/// What: the per-engagement OpenRouter key and the audit instructions, both
/// required, plus optional engagement labels. Unknown keys are tolerated so a
/// config written by a newer generator (#5478) still loads.
/// Test: `super::config_tests::parses_a_representative_engagement_config`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct EngagementConfig {
    /// Spend-capped OpenRouter key, minted per engagement out of band (#5473).
    pub openrouter_key: SecretKey,
    /// What this engagement is asked to assess, in prose.
    pub instructions: String,
    /// Client name, when the generator recorded one.
    #[serde(default)]
    pub client: Option<String>,
    /// Engagement label, when the generator recorded one.
    #[serde(default)]
    pub engagement: Option<String>,
}

impl EngagementConfig {
    /// Filename the handoff package uses.
    pub const FILE_NAME: &'static str = "engagement.toml";

    /// Parse a config from TOML text.
    ///
    /// Why: separated from the read so the schema is testable without a file.
    /// What: `toml::from_str`, with `path` carried only for the error message.
    /// Test: `super::config_tests::parses_a_representative_engagement_config`.
    ///
    /// # Errors
    ///
    /// [`AuditError::Parse`] when the text is not a valid engagement config.
    pub fn from_toml(text: &str, path: &Path) -> Result<Self, AuditError> {
        toml::from_str(text).map_err(|source| AuditError::Parse {
            path: path.to_path_buf(),
            what: "engagement config",
            source: Box::new(source),
        })
    }

    /// Read and parse the config at `path`.
    ///
    /// # Errors
    ///
    /// [`AuditError::Read`] when the file cannot be read, [`AuditError::Parse`]
    /// when its contents do not match the schema.
    pub fn load(path: &Path) -> Result<Self, AuditError> {
        let text = std::fs::read_to_string(path).map_err(|source| AuditError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml(&text, path)
    }

    /// Conventional location of the config: beside the running binary's package.
    pub fn default_path(package_dir: &Path) -> PathBuf {
        package_dir.join(Self::FILE_NAME)
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    const SAMPLE: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks across the selected repositories."
client = "Acme"
"#;

    #[test]
    fn parses_a_representative_engagement_config() {
        let cfg =
            EngagementConfig::from_toml(SAMPLE, Path::new("engagement.toml")).expect("parses");
        assert_eq!(cfg.openrouter_key.expose(), "sk-or-v1-not-a-real-key");
        assert_eq!(cfg.client.as_deref(), Some("Acme"));
        assert!(cfg.engagement.is_none());
    }

    #[test]
    fn unknown_keys_from_a_newer_generator_do_not_break_the_load() {
        let text = format!("{SAMPLE}\naudit_window_weeks = 52\n");
        EngagementConfig::from_toml(&text, Path::new("engagement.toml"))
            .expect("unknown keys are tolerated");
    }

    /// The credential must not reach a log line, a panic message, or an error.
    #[test]
    fn the_key_is_redacted_in_debug_and_display() {
        let cfg =
            EngagementConfig::from_toml(SAMPLE, Path::new("engagement.toml")).expect("parses");

        let debug = format!("{cfg:?}");
        assert!(
            !debug.contains("sk-or-v1-not-a-real-key"),
            "Debug leaked the key: {debug}"
        );
        assert!(debug.contains("<redacted>"), "Debug should say so: {debug}");

        let display = format!("{}", cfg.openrouter_key);
        assert_eq!(display, "<redacted>");
    }

    #[test]
    fn a_missing_key_is_a_parse_error_not_a_blank_default() {
        let text = "instructions = \"whatever\"\n";
        let err = EngagementConfig::from_toml(text, Path::new("engagement.toml"))
            .expect_err("openrouter_key is required");
        assert!(matches!(err, AuditError::Parse { .. }));
    }

    #[test]
    fn blank_keys_are_detectable() {
        assert!(SecretKey::new("   ").is_empty());
        assert!(!SecretKey::new("sk-or-v1-x").is_empty());
    }
}
