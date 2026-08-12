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
//!
//! The config also carries [`ToolPins`], the exact `tga` / `trusty-analyze` /
//! `trusty-review` triple the run must use (#5495). All three are REQUIRED
//! fields: a config that pins two of them fails to parse rather than leaving the
//! third to resolve to whatever is current. That is the version-skew hole
//! #5454 closed from the run side, closed here from the install side.
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

/// One tool's pin: an exact version, optionally an exact artifact digest.
///
/// Why: the version alone is what the run needs, and `tga = "2.9.4"` is what a
/// recipient reading this file should see — the transparency premise (#5473)
/// makes readability a requirement, not a preference. The digest form exists
/// because `trusty-installer`'s published-checksum check is self-published by
/// the same release pipeline it would have to be protecting against, so it is
/// not an independent gate; a digest recorded when the handoff was built is.
/// What: an untagged enum, so both TOML shapes load:
///
/// ```toml
/// [tools]
/// tga = "2.9.4"
/// trusty-analyze = { version = "0.9.2", sha256 = "9f86d0…" }
/// ```
///
/// Test: `super::config_tests::a_pin_reads_as_a_bare_version_or_a_digest_table`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ToolPin {
    /// `tga = "2.9.4"` — pin the version, trust the published checksum.
    Version(String),
    /// `tga = { version = "2.9.4", sha256 = "…" }` — pin the bytes too.
    Digest {
        /// The exact version.
        version: String,
        /// SHA-256 of the release artifact, lowercase hex.
        sha256: String,
    },
}

impl ToolPin {
    /// The pinned version, whichever shape the config used.
    pub fn version(&self) -> &str {
        match self {
            ToolPin::Version(v) | ToolPin::Digest { version: v, .. } => v,
        }
    }

    /// The pinned artifact digest, when the config pinned one.
    pub fn sha256(&self) -> Option<&str> {
        match self {
            ToolPin::Version(_) => None,
            ToolPin::Digest { sha256, .. } => Some(sha256),
        }
    }
}

/// The exact version triple this engagement runs.
///
/// Why: #5454 / PR #5458 closed a hole where a new `tga` paired with an old
/// `trusty-review` produced a deterministic report and exited 0. Fetching
/// "latest" reintroduces that from the install side, so the versions are
/// engagement data rather than a build-time constant, and the report package
/// records which triple produced it.
/// What: three REQUIRED fields. Serde rejects a config that omits one, which is
/// the fail-closed behaviour — there is no default to silently fall back to.
/// The TOML keys are the crate names, hyphenated.
/// Test: `super::config_tests::a_config_missing_one_pin_does_not_load`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ToolPins {
    /// The audit sweep itself.
    pub tga: ToolPin,
    /// Static analysis feeding the scorecard.
    #[serde(rename = "trusty-analyze")]
    pub trusty_analyze: ToolPin,
    /// Report rendering.
    #[serde(rename = "trusty-review")]
    pub trusty_review: ToolPin,
}

/// The engagement config that travels inside the handoff package.
///
/// Why: the recipient can read this file before running anything — that
/// readability is the transparency premise of the whole handoff (#5473), so the
/// schema stays plain TOML with no encoding step.
/// What: the per-engagement OpenRouter key, the audit instructions and the
/// pinned tool triple, all required, plus optional engagement labels. Unknown
/// keys are tolerated so a config written by a newer generator (#5478) still
/// loads.
/// Test: `super::config_tests::parses_a_representative_engagement_config`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct EngagementConfig {
    /// Spend-capped OpenRouter key, minted per engagement out of band (#5473).
    pub openrouter_key: SecretKey,
    /// What this engagement is asked to assess, in prose.
    pub instructions: String,
    /// The exact tool versions this engagement runs (#5495).
    pub tools: ToolPins,
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

    /// Decide the config path from the flag, then the package directory.
    ///
    /// Why: the same shape as [`crate::workdir::WorkDir::resolve`] — the caller
    /// reads the process environment, this stays pure, and the resolution order
    /// is provable without touching a real directory.
    /// What: `explicit` wins; otherwise [`Self::default_path`] under
    /// `package_dir`, which for the CLI is the directory the recipient unzipped
    /// the handoff into and is running from.
    /// Test: `super::config_tests::the_flag_beats_the_package_directory`.
    pub fn resolve_path(explicit: Option<PathBuf>, package_dir: &Path) -> PathBuf {
        explicit.unwrap_or_else(|| Self::default_path(package_dir))
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    const SAMPLE: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks across the selected repositories."
client = "Acme"

[tools]
tga = "2.9.4"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    #[test]
    fn parses_a_representative_engagement_config() {
        let cfg =
            EngagementConfig::from_toml(SAMPLE, Path::new("engagement.toml")).expect("parses");
        assert_eq!(cfg.openrouter_key.expose(), "sk-or-v1-not-a-real-key");
        assert_eq!(cfg.client.as_deref(), Some("Acme"));
        assert!(cfg.engagement.is_none());
        assert_eq!(cfg.tools.tga.version(), "2.9.4");
        assert_eq!(cfg.tools.trusty_analyze.version(), "0.9.2");
        assert_eq!(cfg.tools.trusty_review.version(), "0.15.1");
    }

    #[test]
    fn a_pin_reads_as_a_bare_version_or_a_digest_table() {
        let text = SAMPLE.replace(
            r#"trusty-review = "0.15.1""#,
            r#"trusty-review = { version = "0.15.1", sha256 = "9f86d081884c7d65" }"#,
        );
        let cfg = EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("parses");
        assert_eq!(cfg.tools.trusty_review.version(), "0.15.1");
        assert_eq!(cfg.tools.trusty_review.sha256(), Some("9f86d081884c7d65"));
        // The bare form pins the version and nothing more.
        assert_eq!(cfg.tools.tga.sha256(), None);
    }

    /// The fail-closed property: a partly-pinned config is not a config.
    #[test]
    fn a_config_missing_one_pin_does_not_load() {
        let text = SAMPLE.replace("trusty-review = \"0.15.1\"\n", "");
        let err = EngagementConfig::from_toml(&text, Path::new("engagement.toml"))
            .expect_err("every tool in the triple must be pinned");
        assert!(matches!(err, AuditError::Parse { .. }));
    }

    #[test]
    fn a_config_with_no_tools_section_does_not_load() {
        let text = SAMPLE
            .split("[tools]")
            .next()
            .expect("the sample has a [tools] section")
            .to_owned();
        let err = EngagementConfig::from_toml(&text, Path::new("engagement.toml"))
            .expect_err("the pinned triple is required, not defaulted");
        assert!(matches!(err, AuditError::Parse { .. }));
    }

    #[test]
    fn the_flag_beats_the_package_directory() {
        let explicit = EngagementConfig::resolve_path(
            Some(PathBuf::from("/elsewhere/engagement.toml")),
            Path::new("/package"),
        );
        assert_eq!(explicit, Path::new("/elsewhere/engagement.toml"));
        assert_eq!(
            EngagementConfig::resolve_path(None, Path::new("/package")),
            Path::new("/package/engagement.toml")
        );
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
        let text = SAMPLE.replace("openrouter_key = \"sk-or-v1-not-a-real-key\"\n", "");
        let err = EngagementConfig::from_toml(&text, Path::new("engagement.toml"))
            .expect_err("openrouter_key is required");
        assert!(matches!(err, AuditError::Parse { .. }));
    }

    #[test]
    fn blank_keys_are_detectable() {
        assert!(SecretKey::new("   ").is_empty());
        assert!(!SecretKey::new("sk-or-v1-x").is_empty());
    }
}
