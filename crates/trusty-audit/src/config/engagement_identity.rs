//! What a freshly minted engagement declares about itself (#5478).
//!
//! Why: [`super::generate_for_new_engagement`] strips what belonged to the
//! PREVIOUS engagement and substitutes the credential. It cannot add what THIS
//! one is, because before #5478 nothing minted a window or a keypair to add.
//! This module is the other half — the values that make a rendered file an
//! engagement rather than a template — and it lives beside
//! [`super::EngagementConfig`] rather than inside it so `config.rs` keeps its
//! headroom under the 500-SLOC cap.
//!
//! What: the `[audit]` table's schema, the field names both halves agree on,
//! and the one substitution that writes them.
//! Test: `engagement_identity_tests`.

use std::path::Path;

use serde::{Deserialize, Deserializer};

use super::{EngagementConfig, SIGNING_FIELD, parse_template};
use crate::error::AuditError;

/// The lookback window an engagement assesses, in ISO weeks (#5478).
///
/// One year, the owner's figure. #5482 is the separate work of making `tga`
/// honour it; until that lands the emitted value is inert.
pub const DEFAULT_AUDIT_WINDOW_WEEKS: u32 = 52;

/// The TOML table holding the engagement's audit window (#5478).
///
/// Named once so [`with_engagement_identity`] and [`AuditWindow`] cannot drift
/// apart.
pub const AUDIT_FIELD: &str = "audit";

/// The TOML key holding the audit window itself (#5478).
pub const WINDOW_WEEKS_FIELD: &str = "window_weeks";

/// The TOML key naming the client this engagement is for (#5478).
pub const CLIENT_FIELD: &str = "client";

/// The TOML key labelling the engagement (#5478).
pub const ENGAGEMENT_FIELD: &str = "engagement";

/// How far back this engagement looks (#5478).
///
/// Why: the window was a sentence in `instructions` — "Assess the last 52
/// weeks" — which is prose for a model rather than a value any tool reads. An
/// engagement that wanted six months had nowhere to say so, and the auditor had
/// to trust that a model honoured a sentence. Making it a field is what lets
/// the generator emit it and #5482's `tga` side consume it.
/// What: one key under an `[audit]` table, spelled the way `tga`'s own
/// `audit.window_weeks` is, so an operator moving between the two files types
/// the same thing. The whole table defaults, and so does the key inside it, so
/// a config written before this existed loads and means one year.
///
/// ```toml
/// [audit]
/// window_weeks = 52
/// ```
///
/// A declared `0` is REFUSED rather than accepted. Zero weeks selects no
/// history, so `tga` would sweep nothing and the run would still exit 0 with a
/// report over an empty corpus — the fail-open shape #5478 exists to rule out.
/// The refusal is in the deserializer rather than in the generator, so it holds
/// for a hand-edited config too, and so there is one implementation of it.
///
/// Test: `engagement_identity_tests::a_config_with_no_audit_table_is_a_one_year_window`,
/// `engagement_identity_tests::a_declared_audit_window_loads`,
/// `engagement_identity_tests::a_zero_week_window_is_refused_rather_than_defaulted`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct AuditWindow {
    /// ISO weeks of history this engagement assesses. Never zero.
    #[serde(
        default = "default_window_weeks",
        deserialize_with = "deserialize_window_weeks"
    )]
    pub window_weeks: u32,
}

impl Default for AuditWindow {
    fn default() -> Self {
        Self {
            window_weeks: DEFAULT_AUDIT_WINDOW_WEEKS,
        }
    }
}

/// Serde's default for [`AuditWindow::window_weeks`]. See the type docs.
fn default_window_weeks() -> u32 {
    DEFAULT_AUDIT_WINDOW_WEEKS
}

/// Reject a zero-week window at parse time. See [`AuditWindow`].
///
/// Test: `engagement_identity_tests::a_zero_week_window_is_refused_rather_than_defaulted`.
fn deserialize_window_weeks<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let weeks = u32::deserialize(deserializer)?;
    if weeks == 0 {
        return Err(serde::de::Error::custom(
            "window_weeks must be at least 1 — a zero-week engagement assesses no history",
        ));
    }
    Ok(weeks)
}

/// What a freshly minted engagement declares about itself (#5478).
///
/// Why: four values that arrive together and belong to ONE engagement, so they
/// are one argument rather than four positional ones a caller can transpose —
/// the same reason [`crate::distribute::DistributeOptions`] exists.
/// What: the window, the signing seed as the hex
/// [`crate::package::signing::EngagementKey::private_hex`] produces, and the
/// two optional labels. Borrowed, because every one of them outlives the call.
/// Test: `engagement_identity_tests::a_minted_identity_writes_the_window_and_the_signing_key`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct EngagementIdentity<'a> {
    /// ISO weeks of history the engagement assesses.
    pub window_weeks: u32,
    /// The ed25519 private seed this engagement signs its return package with.
    pub signing_key: &'a str,
    /// The client this engagement is for, when the operator named one.
    pub client: Option<&'a str>,
    /// The engagement's own label, when the operator named one.
    pub engagement: Option<&'a str>,
}

/// Write a minted engagement's own identity into `template` (#5478).
///
/// Why: see the module docs — this is the half [`super::generate_for_new_engagement`]
/// cannot do.
///
/// The two labels are REPLACED rather than merged, and removed when the
/// operator names none. That is #5861's rule one field over: `client = "Acme"`
/// surviving from a template into the config Beta reads is Acme's name inside
/// Beta's package, in a file whose whole premise is that the recipient reads it
/// (#5473). Nothing an auditor's template says about a client can be true of
/// the engagement being minted, so the dropped labels are reported the way
/// [`super::generate_for_new_engagement`] reports dropped board credentials.
///
/// The signing key is the opposite case, and is why this is a separate function
/// from that one: there, `[signing]` is another engagement's and always goes;
/// here it is the key just minted FOR this engagement and is the point.
/// Ordering matters — call [`super::generate_for_new_engagement`] first, then
/// this, so the drop can never land on the fresh key.
///
/// What: the table substitution [`super::with_targets`] uses, over four keys.
/// `[audit]` and `[signing]` are each written whole rather than merged into,
/// because each holds exactly one key and that key is the one being set. The
/// result is parsed back through [`EngagementConfig::from_toml`], so a
/// substitution that would produce an unloadable config — a zero window
/// included — fails HERE and not on the recipient's machine.
/// Test: `engagement_identity_tests::a_minted_identity_writes_the_window_and_the_signing_key`,
/// `engagement_identity_tests::a_minted_identity_drops_the_templates_client_labels`,
/// `engagement_identity_tests::a_minted_identity_preserves_every_other_field`,
/// `engagement_identity_tests::a_minted_identity_refuses_a_zero_week_window`.
///
/// # Errors
///
/// [`AuditError::Parse`] when `template` is not TOML, or when the substituted
/// result is not a loadable engagement config; [`AuditError::Render`] when the
/// table cannot be re-serialized.
pub fn with_engagement_identity(
    template: &str,
    identity: &EngagementIdentity<'_>,
    path: &Path,
) -> Result<(String, Vec<String>), AuditError> {
    let mut table = parse_template(template, path)?;

    let mut audit = toml::Table::new();
    audit.insert(
        WINDOW_WEEKS_FIELD.to_owned(),
        toml::Value::Integer(i64::from(identity.window_weeks)),
    );
    table.insert(AUDIT_FIELD.to_owned(), toml::Value::Table(audit));

    // #5478: the one site that writes a MINTED signing seed into a config. The
    // seed reaches here as hex from `EngagementKey::private_hex`, which is the
    // crate's one function that turns a key into printable text.
    let mut signing = toml::Table::new();
    signing.insert(
        "private_key".to_owned(),
        toml::Value::String(identity.signing_key.to_owned()),
    );
    table.insert(SIGNING_FIELD.to_owned(), toml::Value::Table(signing));

    let mut dropped = Vec::new();
    for (field, value) in [
        (CLIENT_FIELD, identity.client),
        (ENGAGEMENT_FIELD, identity.engagement),
    ] {
        match value {
            Some(label) => {
                table.insert(field.to_owned(), toml::Value::String(label.to_owned()));
            }
            None => {
                if table.remove(field).is_some() {
                    dropped.push(field.to_owned());
                }
            }
        }
    }

    let rendered = toml::to_string_pretty(&table).map_err(|source| AuditError::Render {
        what: "engagement config",
        source: Box::new(source),
    })?;
    EngagementConfig::from_toml(&rendered, path)?;
    Ok((rendered, dropped))
}

#[cfg(test)]
mod engagement_identity_tests {
    use super::*;

    /// The 64 zeros are a placeholder, not a usable key — a credential scan
    /// over this source must not have to decide.
    const SEED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    const TEMPLATE: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."
client = "Previous Client"
engagement = "Previous Engagement"

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"

[report]
investigate_max_files = 240
"#;

    fn identity<'a>(window_weeks: u32, client: Option<&'a str>) -> EngagementIdentity<'a> {
        EngagementIdentity {
            window_weeks,
            signing_key: SEED,
            client,
            engagement: None,
        }
    }

    fn path() -> &'static Path {
        Path::new("engagement.toml")
    }

    /// Acceptance (d): a config written before `[audit]` existed still loads,
    /// and means one year.
    #[test]
    fn a_config_with_no_audit_table_is_a_one_year_window() {
        let cfg = EngagementConfig::from_toml(TEMPLATE, path()).expect("the old shape still loads");
        assert_eq!(cfg.audit.window_weeks, DEFAULT_AUDIT_WINDOW_WEEKS);
    }

    #[test]
    fn a_declared_audit_window_loads() {
        let text = format!("{TEMPLATE}\n[audit]\nwindow_weeks = 26\n");
        let cfg = EngagementConfig::from_toml(&text, path()).expect("loads");
        assert_eq!(cfg.audit.window_weeks, 26);
    }

    /// A zero window is the fail-open shape: `tga` would sweep no history and
    /// still exit 0. It must be a typed refusal, never a silent default.
    #[test]
    fn a_zero_week_window_is_refused_rather_than_defaulted() {
        let text = format!("{TEMPLATE}\n[audit]\nwindow_weeks = 0\n");
        let err = EngagementConfig::from_toml(&text, path()).expect_err("zero is refused");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
        assert!(
            err.to_string().contains("window_weeks"),
            "the refusal must name the field: {err}"
        );
    }

    #[test]
    fn a_minted_identity_writes_the_window_and_the_signing_key() {
        let (rendered, _) =
            with_engagement_identity(TEMPLATE, &identity(26, Some("Acme")), path()).expect("mints");

        let loaded = EngagementConfig::from_toml(&rendered, path()).expect("loads");
        assert_eq!(loaded.audit.window_weeks, 26);
        assert_eq!(
            loaded
                .signing
                .private_key
                .as_ref()
                .expect("the seed is written")
                .expose(),
            SEED
        );
        assert_eq!(loaded.client.as_deref(), Some("Acme"));
    }

    #[test]
    fn a_minted_identity_drops_the_templates_client_labels() {
        let (rendered, dropped) =
            with_engagement_identity(TEMPLATE, &identity(52, None), path()).expect("mints");

        assert!(!rendered.contains("Previous Client"), "{rendered}");
        assert!(!rendered.contains("Previous Engagement"), "{rendered}");
        assert!(dropped.iter().any(|d| d == CLIENT_FIELD), "{dropped:?}");
        assert!(dropped.iter().any(|d| d == ENGAGEMENT_FIELD), "{dropped:?}");
    }

    #[test]
    fn a_minted_identity_preserves_every_other_field() {
        let (rendered, _) =
            with_engagement_identity(TEMPLATE, &identity(52, Some("Acme")), path()).expect("mints");

        let loaded = EngagementConfig::from_toml(&rendered, path()).expect("loads");
        assert_eq!(loaded.openrouter_key.expose(), "sk-or-v1-not-a-real-key");
        assert_eq!(loaded.tools.tga.version(), "2.9.4");
        assert_eq!(loaded.tools.trusty_review.version(), "0.15.1");
        assert_eq!(loaded.report.investigate_max_files, Some(240));
    }

    /// The round-trip through [`EngagementConfig::from_toml`] is what makes the
    /// substitution fail-closed: a window no config may declare is refused by
    /// the writer, not discovered by the recipient.
    #[test]
    fn a_minted_identity_refuses_a_zero_week_window() {
        let err = with_engagement_identity(TEMPLATE, &identity(0, Some("Acme")), path())
            .expect_err("a zero window cannot be minted");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
    }

    /// A template that is not a config is named as a parse failure rather than
    /// silently producing a file with the identity keys and nothing else.
    #[test]
    fn a_template_that_is_not_toml_is_a_typed_parse_error() {
        let err = with_engagement_identity("this is not toml{{", &identity(52, None), path())
            .expect_err("not a template");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
    }
}
