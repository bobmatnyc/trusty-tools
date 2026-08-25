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
use crate::registry::Target;

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
/// What: four REQUIRED fields. Serde rejects a config that omits one, which is
/// the fail-closed behaviour — there is no default to silently fall back to.
/// The TOML keys are the crate names, hyphenated.
/// Test: `super::config_tests::a_config_missing_one_pin_does_not_load`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ToolPins {
    /// The audit sweep itself.
    pub tga: ToolPin,
    /// The search daemon `trusty-analyze` and the indexing pass both need
    /// (#5670). `tga audit` starts it, so an engagement pins it like the rest.
    #[serde(rename = "trusty-search")]
    pub trusty_search: ToolPin,
    /// Static analysis feeding the scorecard.
    #[serde(rename = "trusty-analyze")]
    pub trusty_analyze: ToolPin,
    /// Report rendering.
    #[serde(rename = "trusty-review")]
    pub trusty_review: ToolPin,
}

/// Per-role model selection for the audit's review inference (#5671).
///
/// Why: the built-in slugs in [`crate::inference`] are Rust constants, so a
/// model rename would otherwise need a release before an engagement could run.
/// Naming them here lets the owner correct a slug in the file the recipient
/// already has. Every field is optional — a config that says nothing about
/// models gets the built-in defaults, which is the common case.
/// What: an all-optional mirror of the four variables `trusty-review` reads.
/// The TOML shape is a `[models]` table:
///
/// ```toml
/// [models]
/// reviewer = "anthropic/claude-sonnet-4.6"
/// ```
///
/// Unknown keys are REJECTED here, unlike the rest of the config. The point of
/// the table is fixing a slug without a release; `reviewr = "…"` silently
/// falling back to the built-in default defeats exactly that, and the operator
/// would only learn of the typo from a surprising bill. The cost is that a
/// newer generator adding a fifth role fails to load rather than degrading —
/// the right trade for four keys that are all optional anyway.
/// Test: `super::config_tests::a_models_table_loads_and_is_optional`,
/// `super::config_tests::a_typo_in_the_models_table_is_a_parse_error`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ModelPins {
    /// Overrides the provider `trusty-review` selects (`openrouter` by default).
    pub provider: Option<String>,
    /// Overrides the reviewer role's model id.
    pub reviewer: Option<String>,
    /// Overrides the verifier role's model id.
    pub verifier: Option<String>,
    /// Overrides the summarizer role's model id.
    pub summarizer: Option<String>,
}

/// The client's JIRA credential, as supplied through the engagement config.
///
/// Why: the audit runs at the CLIENT site against the CLIENT's board, so this
/// is their credential, not the owner's — it arrives the same way the
/// OpenRouter key does and lives under the same rules (#5822). `token` is a
/// [`SecretKey`], so it cannot be serialized into an artifact and redacts in
/// `Debug` and `Display`.
/// What: the three values JIRA Cloud's REST API needs — the site URL, the
/// account email, and the API token, which pair as HTTP Basic auth. The field
/// names match tga's own `jira` collector config so an operator who has
/// configured one has configured both.
/// Test: `super::config_tests::board_credentials_load_and_redact`,
/// `super::config_tests::a_url_carrying_credentials_never_reaches_debug`.
#[derive(Clone, Deserialize)]
#[non_exhaustive]
pub struct JiraCredentials {
    /// Site base URL, e.g. `https://acme.atlassian.net`.
    ///
    /// May carry `user:password@` userinfo — [`crate::validate`] anticipates
    /// that, and this module's `Debug` strips it (#5822).
    pub url: String,
    /// Account email, the Basic-auth username.
    pub email: String,
    /// API token, the Basic-auth password.
    pub token: SecretKey,
}

/// What replaces a URL's userinfo, and what a `Debug` render shows instead.
const USERINFO_REDACTED: &str = "<redacted>";

/// Redacts the URL. See [`redact_userinfo`].
///
/// Why: the derived `Debug` printed `url` verbatim, so `format!("{cfg:?}")` on
/// an [`EngagementConfig`] — or any future `tracing::debug!("{:?}", config)` —
/// published whatever an operator had pasted into it. `token` was already safe
/// through [`SecretKey`]; this closes the same hole one field over (#5822).
/// What: the derived layout with `url` passed through [`redact_userinfo`].
/// `email` stays visible: it is the Basic-auth username, and with the password
/// redacted it is not on its own a credential.
/// Test: `super::config_tests::a_url_carrying_credentials_never_reaches_debug`.
impl fmt::Debug for JiraCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JiraCredentials")
            .field("url", &redact_userinfo(&self.url))
            .field("email", &self.email)
            .field("token", &self.token)
            .finish()
    }
}

/// Replace a URL's `user:password@` with [`USERINFO_REDACTED`].
///
/// Why: redacting rather than refusing the config at parse time. The stricter
/// option would reject a userinfo-bearing `boards.jira.url` outright, which
/// turns a deployment that works today — a JIRA reached through a proxy that
/// wants inline credentials — into a hard failure the recipient cannot work
/// around; they did not author this file. Redaction closes the leak and changes
/// nothing about what loads or what [`crate::validate`] sends.
///
/// [`trusty_common::credentials::scrub_secrets`] does not cover this shape: it
/// removes values the process already holds by name, and userinfo pasted into a
/// URL is not one of them. Nothing else in the workspace redacts a URL
/// structurally, and only this crate needs it, so it stays here (CLAUDE.md's
/// common-entry-point rule governs capabilities shared ACROSS crates).
/// What: cuts everything between `//` and the last `@` of the authority — the
/// span before the first `/`, `?` or `#`. A URL with no userinfo, and text that
/// is not a URL at all, come back unchanged.
/// Test: `super::config_tests::a_url_carrying_credentials_never_reaches_debug`,
/// `super::config_tests::redaction_leaves_a_url_without_userinfo_alone`.
fn redact_userinfo(url: &str) -> String {
    let Some(marker) = url.find("//") else {
        return url.to_owned();
    };
    let authority_start = marker + 2;
    let rest = &url[authority_start..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        return url.to_owned();
    };
    format!(
        "{}{USERINFO_REDACTED}@{}",
        &url[..authority_start],
        &rest[at + 1..]
    )
}

/// The client's Linear credential. See [`JiraCredentials`].
///
/// Linear authenticates with a single personal API key sent as the
/// `Authorization` header with no `Bearer` prefix — tga's collector documents
/// the same convention.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct LinearCredentials {
    /// Personal API key.
    pub api_key: SecretKey,
}

/// Whatever board credentials this engagement was given.
///
/// Why: an engagement may register no boards, one, or both, and a repo-only
/// engagement must keep loading a config that says nothing about boards — so
/// every field is optional and the whole table defaults (#5822). Absence is
/// what [`crate::registry`] turns into an actionable refusal at the moment a
/// board is registered, naming the field to set.
/// What: one optional entry per provider, under a `[boards]` table.
///
/// ```toml
/// [boards.jira]
/// url = "https://acme.atlassian.net"
/// email = "auditor@acme.example"
/// token = "…"
///
/// [boards.linear]
/// api_key = "lin_api_…"
/// ```
///
/// Test: `super::config_tests::board_credentials_load_and_redact`,
/// `super::config_tests::a_config_with_no_boards_table_still_loads`.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct BoardCredentials {
    /// JIRA, when the engagement was given one.
    pub jira: Option<JiraCredentials>,
    /// Linear, when the engagement was given one.
    pub linear: Option<LinearCredentials>,
}

/// What the engagement asks the investigation pass to read, per repository.
///
/// Why (#6247): the investigation budget had no declaration point an operator
/// could write. `crate::grounding::priority::Budget` resolved it from the
/// process environment and the compiled default, while the budget the sweep
/// RECORDED was re-resolved later from the per-repository manifest — two
/// resolutions of one value, computed at different times by different
/// functions. An engagement that wanted a wider sample had nowhere to say so,
/// and the manifest it got back could name a budget the sampler never ran
/// under.
/// What: the same two key names `trusty-review` reads from its own manifest
/// `[report]` section, so an operator moving between the two files types the
/// same thing. Both optional and both independent — see
/// [`crate::grounding::priority::Budget::for_engagement`] for the precedence
/// and for why an absent byte budget is derived rather than pinned.
///
/// ```toml
/// [report]
/// investigate_max_files = 240
/// investigate_max_bytes = 2457600
/// ```
///
/// Test: `super::config_tests::a_declared_investigation_budget_loads`,
/// `super::config_tests::a_config_with_no_report_table_still_loads`.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct ReportSettings {
    /// Files the investigation pass may read per repository.
    #[serde(default)]
    pub investigate_max_files: Option<usize>,
    /// Total content bytes it may send per repository.
    #[serde(default)]
    pub investigate_max_bytes: Option<usize>,
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
    /// Per-role model selection for review inference (#5671). Absent means the
    /// built-in slugs in [`crate::inference`].
    #[serde(default)]
    pub models: ModelPins,
    /// The client's own JIRA / Linear credentials (#5822). Absent means this
    /// engagement registers no boards.
    #[serde(default)]
    pub boards: BoardCredentials,
    /// What this engagement asks the investigation pass to read (#6247).
    ///
    /// Absent means the machine's environment overrides, then the compiled
    /// defaults — see [`crate::grounding::priority::Budget::for_engagement`].
    #[serde(default)]
    pub report: ReportSettings,
    /// What this engagement audits (#5979).
    ///
    /// Absent and empty are DIFFERENT states, which is why this is an `Option`
    /// rather than a defaulted `Vec`. Absent means no version of this file ever
    /// declared a target set, so [`crate::registry::engagement_targets`] adopts
    /// whatever `state/audit-targets.toml` holds. `targets = []` means the
    /// operator removed their last target, and the answer is zero. Collapsing
    /// the two would either lose an upgrading operator's registrations or
    /// resurrect the ones they just deleted.
    ///
    /// Read it through [`EngagementConfig::declared_targets`].
    #[serde(default)]
    pub targets: Option<Vec<Target>>,
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

    /// The target set this file declares, or `None` when it declares none.
    ///
    /// `Some(&[])` is a declaration of zero targets; `None` is the absence of a
    /// declaration. See [`EngagementConfig::targets`].
    pub fn declared_targets(&self) -> Option<&[Target]> {
        self.targets.as_deref()
    }

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

    /// Read the config, or `None` when the handoff package left none here.
    ///
    /// Why: the guided flow reports state on a working directory that may have
    /// no config yet, and must keep doing so (#5797). It needs the pins to
    /// auto-install, so it asks for them — but an absent config is a state to
    /// report, not a failure, and the flow falls back to naming the install step
    /// rather than performing it.
    /// What: `NotFound` becomes `Ok(None)`; every other read failure and every
    /// parse failure still propagates, because a config that is present and
    /// wrong is not the same as one that is not there. Shaped after
    /// [`crate::manifest::AuditManifest::load_if_present`].
    /// Test: `super::config_tests::an_absent_config_is_not_an_error`,
    /// `super::config_tests::a_malformed_config_is_still_an_error`.
    ///
    /// # Errors
    ///
    /// [`AuditError::Read`] for a read failure other than absence, and
    /// [`AuditError::Parse`] when the file does not match the schema.
    pub fn load_if_present(path: &Path) -> Result<Option<Self>, AuditError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text, path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(AuditError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
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

    /// Every secret value this config carries, in one list.
    ///
    /// Why: two guards need exactly this set and had derived it separately —
    /// [`crate::package`] refuses an archive member containing any of them, and
    /// [`crate::run`] strips them out of a child's log. The board credentials
    /// are what makes a shared list worth having: they arrive here from the
    /// TOML rather than from the environment, so
    /// `trusty_common::credentials::resolved_secret_values` — which enumerates
    /// the registered providers — does not know them, and each guard has to add
    /// them by hand. A guard that added one and forgot the other is the #5869
    /// leak class, one credential over.
    /// What: the OpenRouter key, plus each board credential the engagement was
    /// given. Raw values: the only correct uses are as scrub needles and as
    /// scan needles.
    /// Test: `super::config_tests::configured_secrets_covers_every_credential`,
    /// `crate::run::run_tests::a_child_that_echoes_a_board_credential_does_not_leave_it_in_the_log`.
    pub fn configured_secrets(&self) -> Vec<&str> {
        let mut secrets = vec![self.openrouter_key.expose()];
        if let Some(jira) = self.boards.jira.as_ref() {
            secrets.push(jira.token.expose());
        }
        if let Some(linear) = self.boards.linear.as_ref() {
            secrets.push(linear.api_key.expose());
        }
        secrets
    }
}

/// The TOML key holding the OpenRouter credential.
///
/// Named once so [`generate`] and the schema cannot drift apart.
pub const OPENROUTER_KEY_FIELD: &str = "openrouter_key";

/// Render an engagement config carrying `key`, from `template`.
///
/// Why: the INBOUND package (#5825) ships a generated `engagement.toml` with the
/// credential in it, and that is deliberately hard to write. [`SecretKey`] has
/// no `Serialize`, so no derive can produce this file and no future output
/// artifact can produce it by accident. This is the one narrow path that does,
/// and it lives here — beside the type whose invariant it steps around — so
/// `git grep expose` finds it next to the crate's other two plaintext sites
/// rather than buried in a packaging module.
///
/// Direction is the whole justification, and nothing here weakens the other
/// direction. The inbound config travels TO the recipient and must carry the
/// key; the outbound return package travels BACK and must not, which
/// [`crate::package`]'s per-member scan and [`crate::error::AuditError::CredentialInPackage`]
/// enforce unchanged. No `Serialize` impl is added, so the outbound path still
/// cannot write a [`SecretKey`] even by accident. The two directions are two
/// functions in two modules producing two types, not one function with a flag.
///
/// What: parses `template` as a TOML table, replaces [`OPENROUTER_KEY_FIELD`],
/// and re-renders. Everything else survives verbatim — the pins, the board
/// credentials, keys a newer generator added — so this is a substitution rather
/// than a schema this function has to keep in step with. The result is parsed
/// back through [`EngagementConfig::from_toml`], so a template that would
/// produce an unloadable config fails HERE and not on the recipient's machine.
///
/// 🔴 Surviving verbatim is correct for an IN-PLACE re-render, which is what
/// this function is for: `crate::cli::credential` saves a newly-entered key into
/// the recipient's own `engagement.toml` and must not destroy the board
/// credentials that recipient configured. Generating a config for a DIFFERENT
/// engagement is [`generate_for_new_engagement`], because the same substitution
/// carries one client's board credentials into another's package (#5861).
///
/// #5478 (the engagement-config generator) is unimplemented; when it lands it
/// calls one of these two rather than growing a third way to write the file.
/// Test: `config_tests::the_generated_config_carries_the_supplied_key`,
/// `config_tests::generating_a_config_that_would_not_load_fails_here`,
/// `config_tests::generating_preserves_every_other_field`.
///
/// # Errors
///
/// [`AuditError::Parse`] when `template` is not TOML, or when the substituted
/// result is not a loadable engagement config; [`AuditError::Render`] when the
/// table cannot be re-serialized.
pub fn generate(template: &str, key: &SecretKey, path: &Path) -> Result<String, AuditError> {
    let table = parse_template(template, path)?;
    render_with_key(table, key, path)
}

/// The TOML table holding the client's board credentials.
///
/// Named once so [`generate_for_new_engagement`] and the schema cannot drift
/// apart.
pub const BOARDS_FIELD: &str = "boards";

/// [`generate`], for a config that belongs to a DIFFERENT engagement (#5861).
///
/// Why: [`generate`] is a substitution, so everything the template holds
/// survives verbatim — including `[boards.jira]` and `[boards.linear]`. An
/// auditor reusing one template across engagements therefore shipped client A's
/// board credentials inside client B's package, and `engagement.toml` is
/// deliberately readable, so client B could read them. [`crate::package`]'s
/// outbound scan cannot catch this: it guards what leaves on the way back, not
/// what was carried over on the way in.
///
/// The board credentials are the CLIENT's, not the auditor's — `[boards.jira]`
/// is documented as "their credential, not the owner's" on
/// [`BoardCredentials`]. So nothing an auditor's template holds under
/// [`BOARDS_FIELD`] can belong to the engagement being generated, and dropping
/// the table is the whole fix rather than a heuristic about which fields look
/// secret.
///
/// This is a SEPARATE function rather than a flag on [`generate`], because the
/// other caller re-renders a config IN PLACE — `crate::cli::credential` saves a
/// newly-entered key into the recipient's own `engagement.toml`, and stripping
/// there would destroy the board credentials that recipient configured. A
/// boolean saying which is which is a boolean somebody eventually passes wrong.
///
/// What: [`generate`] with [`BOARDS_FIELD`] removed from the parsed table
/// first, plus the field names that were dropped so the caller can tell the
/// operator what did not ship. The recipient is not left guessing either:
/// registering a board with no credential is
/// [`AuditError::BoardCredentialMissing`](crate::error::AuditError::BoardCredentialMissing),
/// which names the field to set.
/// Test: `config_tests::a_generated_config_never_carries_the_templates_board_credentials`,
/// `config_tests::an_in_place_render_keeps_the_recipients_own_board_credentials`.
///
/// # Errors
///
/// As [`generate`].
pub fn generate_for_new_engagement(
    template: &str,
    key: &SecretKey,
    path: &Path,
) -> Result<(String, Vec<String>), AuditError> {
    let mut table = parse_template(template, path)?;
    let dropped = drop_board_credentials(&mut table);
    Ok((render_with_key(table, key, path)?, dropped))
}

/// The template as a TOML table.
fn parse_template(template: &str, path: &Path) -> Result<toml::Table, AuditError> {
    toml::from_str(template).map_err(|source| AuditError::Parse {
        path: path.to_path_buf(),
        what: "engagement config template",
        source: Box::new(source),
    })
}

/// Take the board credentials out, naming every field that went (#5861).
fn drop_board_credentials(table: &mut toml::Table) -> Vec<String> {
    match table.remove(BOARDS_FIELD) {
        None => Vec::new(),
        Some(toml::Value::Table(providers)) => providers
            .keys()
            .map(|provider| format!("{BOARDS_FIELD}.{provider}"))
            .collect(),
        // Not a table, so nothing to enumerate — it still goes, because a
        // `boards` this function does not understand is not one it may ship.
        Some(_) => vec![BOARDS_FIELD.to_owned()],
    }
}

/// Substitute the key into `table` and re-render, proving the result loads.
fn render_with_key(
    mut table: toml::Table,
    key: &SecretKey,
    path: &Path,
) -> Result<String, AuditError> {
    // #5825: the ONE site that writes the plaintext credential into a generated
    // file. See [`generate`]'s docs for why it is not a `Serialize` impl.
    table.insert(
        OPENROUTER_KEY_FIELD.to_owned(),
        toml::Value::String(key.expose().to_owned()),
    );
    let rendered = toml::to_string_pretty(&table).map_err(|source| AuditError::Render {
        what: "engagement config",
        source: Box::new(source),
    })?;
    EngagementConfig::from_toml(&rendered, path)?;
    Ok(rendered)
}

/// The TOML key holding the engagement's target set.
///
/// Named once so [`with_targets`] and the schema cannot drift apart.
pub const TARGETS_FIELD: &str = "targets";

/// Re-render `template` with `targets` as its declared target set (#5979).
///
/// Why: `engagement.toml` is now what an engagement's targets are declared in,
/// so `taudit add repo` has to write this file. It cannot be produced by a
/// derive — [`SecretKey`] has no `Serialize`, deliberately — so this is a
/// substitution over the parsed table, the same shape [`generate`] uses one
/// field over. Everything the recipient or a newer generator put in the file
/// survives verbatim, which is what stops a registration quietly dropping the
/// pins or a board credential.
///
/// The existing `openrouter_key` passes through as the `toml::Value::String` it
/// parsed to and is never re-serialized from a [`SecretKey`], so this function
/// adds no second way for a credential to be written.
///
/// What: replaces [`TARGETS_FIELD`] and re-renders. An EMPTY slice writes
/// `targets = []` rather than removing the key — the difference between a
/// declared-empty engagement and one that never declared anything is what
/// [`EngagementConfig::targets`] turns into "adopt the old registry" or "the
/// answer is zero". The result is parsed back through
/// [`EngagementConfig::from_toml`], so a substitution that would produce an
/// unloadable config fails HERE, before anything is written.
/// Test: `config_tests::writing_targets_preserves_every_other_field`,
/// `config_tests::an_emptied_target_list_is_declared_rather_than_dropped`,
/// `config_tests::the_target_list_is_rendered_ahead_of_the_required_pins`.
///
/// # Errors
///
/// [`AuditError::Parse`] when `template` is not TOML, or when the substituted
/// result is not a loadable engagement config; [`AuditError::Render`] when the
/// targets or the table cannot be serialized.
pub fn with_targets(template: &str, targets: &[Target], path: &Path) -> Result<String, AuditError> {
    let mut table: toml::Table = toml::from_str(template).map_err(|source| AuditError::Parse {
        path: path.to_path_buf(),
        what: "engagement config",
        source: Box::new(source),
    })?;
    let value = toml::Value::try_from(targets).map_err(|source| AuditError::Render {
        what: "engagement targets",
        source: Box::new(source),
    })?;
    table.insert(TARGETS_FIELD.to_owned(), value);
    let rendered = toml::to_string_pretty(&table).map_err(|source| AuditError::Render {
        what: "engagement config",
        source: Box::new(source),
    })?;
    EngagementConfig::from_toml(&rendered, path)?;
    Ok(rendered)
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use crate::registry::BoardProvider;

    fn repo(name: &str) -> Target {
        Target::Repo {
            name_with_owner: name.to_owned(),
        }
    }

    /// A registration must not cost the recipient their pins, their models, or
    /// a board credential — the whole file survives the substitution.
    #[test]
    fn writing_targets_preserves_every_other_field() {
        let source = format!("{SAMPLE}\n[boards.linear]\napi_key = \"lin_api_secret\"\n");
        let path = Path::new("engagement.toml");
        let text = with_targets(
            &source,
            &[
                repo("acme/api"),
                Target::Board {
                    provider: BoardProvider::Jira,
                    key: "ACME".to_owned(),
                },
            ],
            path,
        )
        .expect("writes");

        let loaded = EngagementConfig::from_toml(&text, path).expect("loads");
        let declared = loaded.declared_targets().expect("targets are declared");
        assert_eq!(
            declared.iter().map(Target::id).collect::<Vec<_>>(),
            vec!["acme/api", "jira:ACME"]
        );
        assert_eq!(loaded.openrouter_key.expose(), "sk-or-v1-not-a-real-key");
        assert_eq!(loaded.tools.trusty_review.version(), "0.15.1");
        assert_eq!(loaded.client.as_deref(), Some("Acme"));
        assert_eq!(
            loaded
                .boards
                .linear
                .as_ref()
                .expect("linear survives")
                .api_key
                .expose(),
            "lin_api_secret"
        );
    }

    /// Removing the last target declares zero, and must not read as a file that
    /// never declared anything — that would resurrect the old registry.
    #[test]
    fn an_emptied_target_list_is_declared_rather_than_dropped() {
        let path = Path::new("engagement.toml");
        let text = with_targets(SAMPLE, &[], path).expect("writes");
        let loaded = EngagementConfig::from_toml(&text, path).expect("loads");
        assert_eq!(loaded.declared_targets(), Some(&[][..]));

        let untouched = EngagementConfig::from_toml(SAMPLE, path).expect("loads");
        assert_eq!(untouched.declared_targets(), None);
    }

    /// 🔴 #5979: what replaces the registry's `count` guard. A truncation that
    /// loses the tail of the target list also loses `[tools]`, whose four pins
    /// are required — so a partial file refuses to load rather than reading as a
    /// smaller-but-complete engagement.
    ///
    /// The ordering is `toml::to_string_pretty`'s, and asserting it is the
    /// point: a serializer change that moved `[tools]` above `[[targets]]` would
    /// silently retire the guard.
    #[test]
    fn the_target_list_is_rendered_ahead_of_the_required_pins() {
        let path = Path::new("engagement.toml");
        let text =
            with_targets(SAMPLE, &[repo("acme/api"), repo("acme/web")], path).expect("writes");
        let targets = text.find("[[targets]]").expect("targets are rendered");
        let tools = text.find("[tools]").expect("tools are rendered");
        assert!(targets < tools, "{text}");

        // Cut the file at the first target: what is left is not an engagement.
        let truncated = &text[..targets];
        EngagementConfig::from_toml(truncated, path)
            .expect_err("a truncated config must not read as a smaller target set");
    }

    /// A hand-written `[[targets]]` block loads with the same spelling
    /// `crate::registry` writes, so the file stays one an operator can edit.
    #[test]
    fn a_hand_written_target_block_loads() {
        let text = format!(
            "{SAMPLE}\n[[targets]]\nkind = \"repo\"\nname_with_owner = \"acme/api\"\n\
             \n[[targets]]\nkind = \"board\"\nprovider = \"linear\"\nkey = \"ENG\"\n"
        );
        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("parses");
        let declared = loaded.declared_targets().expect("declared");
        assert_eq!(
            declared.iter().map(Target::id).collect::<Vec<_>>(),
            vec!["acme/api", "linear:ENG"]
        );
    }

    /// A target block that names no known kind is a refusal, not a skipped
    /// entry — a silently dropped target is an unaudited one.
    #[test]
    fn an_unknown_target_kind_is_a_parse_error() {
        let text = format!("{SAMPLE}\n[[targets]]\nkind = \"gitlab\"\nname_with_owner = \"a/b\"\n");
        let err = EngagementConfig::from_toml(&text, Path::new("engagement.toml"))
            .expect_err("`gitlab` is not a target kind");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
    }

    const SAMPLE: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks across the selected repositories."
client = "Acme"

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
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
    fn the_generated_config_carries_the_supplied_key() {
        let key = SecretKey::new("sk-or-v1-per-engagement");
        let text = generate(SAMPLE, &key, Path::new("engagement.toml")).expect("generates");

        let loaded = EngagementConfig::from_toml(&text, Path::new("engagement.toml"))
            .expect("the generated file loads on the recipient's machine");
        assert_eq!(loaded.openrouter_key.expose(), "sk-or-v1-per-engagement");
        assert!(!text.contains("sk-or-v1-not-a-real-key"), "{text}");
    }

    /// The substitution is one field wide: pins, labels, board credentials and
    /// anything a newer generator added all survive verbatim.
    #[test]
    fn generating_preserves_every_other_field() {
        // The scalars go BEFORE `[tools]`: appending them after a table header
        // would nest them inside it. `generate` re-emits them in the order TOML
        // requires regardless, which is why the round trip below is safe.
        let source = format!(
            "engagement = \"2026-Q3\"\nfuture_field = 7\n{SAMPLE}\
             \n[models]\nreviewer = \"anthropic/claude-opus-4.8\"\n\
             \n[boards.linear]\napi_key = \"lin_api_secret\"\n"
        );
        let text = generate(
            &source,
            &SecretKey::new("sk-or-v1-new"),
            Path::new("engagement.toml"),
        )
        .expect("generates");

        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("loads");
        assert_eq!(loaded.client.as_deref(), Some("Acme"));
        assert_eq!(loaded.engagement.as_deref(), Some("2026-Q3"));
        assert_eq!(loaded.tools.trusty_review.version(), "0.15.1");
        assert_eq!(
            loaded.models.reviewer.as_deref(),
            Some("anthropic/claude-opus-4.8")
        );
        assert_eq!(
            loaded
                .boards
                .linear
                .as_ref()
                .expect("linear survives")
                .api_key
                .expose(),
            "lin_api_secret"
        );
        assert!(text.contains("future_field"), "{text}");
    }

    /// The board credentials a template holds, as an auditor's reused template
    /// carries them from the last engagement.
    fn template_with_boards() -> String {
        format!(
            "{SAMPLE}\n[boards.jira]\nurl = \"https://client-a.atlassian.net\"\n\
             email = \"auditor@client-a.example\"\ntoken = \"jira-token-client-a\"\n\
             \n[boards.linear]\napi_key = \"lin_api_client_a\"\n"
        )
    }

    /// 🔴 #5861: client A's board credentials must not reach client B.
    ///
    /// `generate` is a substitution, so against `6d937ba66` every field the
    /// template held survived verbatim into the next engagement's config —
    /// including `[boards.jira]`. `engagement.toml` is deliberately readable by
    /// the recipient, so the previous client's token was legible to the next
    /// one, and the outbound scan in `crate::package` cannot see it: that scan
    /// guards what leaves on the way back, not what was carried over on the way
    /// in.
    #[test]
    fn a_generated_config_never_carries_the_templates_board_credentials() {
        let path = Path::new("engagement.toml");
        let (text, dropped) = generate_for_new_engagement(
            &template_with_boards(),
            &SecretKey::new("sk-or-v1-client-b"),
            path,
        )
        .expect("generates");

        assert!(!text.contains("jira-token-client-a"), "{text}");
        assert!(!text.contains("lin_api_client_a"), "{text}");
        assert!(!text.contains("client-a.atlassian.net"), "{text}");
        assert!(!text.contains("auditor@client-a.example"), "{text}");
        assert_eq!(dropped, ["boards.jira", "boards.linear"]);

        // What DOES ship: the new key, and everything that is not a credential.
        let loaded = EngagementConfig::from_toml(&text, path).expect("loads");
        assert_eq!(loaded.openrouter_key.expose(), "sk-or-v1-client-b");
        assert_eq!(loaded.tools.trusty_review.version(), "0.15.1");
        assert_eq!(loaded.client.as_deref(), Some("Acme"));
        assert!(loaded.boards.jira.is_none());
        assert!(loaded.boards.linear.is_none());
        assert!(
            loaded.configured_secrets().len() == 1,
            "a credential rode along"
        );
    }

    /// The other direction, and the reason this is two functions rather than a
    /// flag: `crate::cli::credential` re-renders the RECIPIENT's own config to
    /// save a key they just entered. Stripping there would delete the board
    /// credentials that recipient configured.
    #[test]
    fn an_in_place_render_keeps_the_recipients_own_board_credentials() {
        let text = generate(
            &template_with_boards(),
            &SecretKey::new("sk-or-v1-re-entered"),
            Path::new("engagement.toml"),
        )
        .expect("generates");

        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("loads");
        assert_eq!(loaded.openrouter_key.expose(), "sk-or-v1-re-entered");
        assert_eq!(
            loaded
                .boards
                .jira
                .as_ref()
                .expect("jira survives an in-place render")
                .token
                .expose(),
            "jira-token-client-a"
        );
    }

    /// A template that names no board drops nothing and says so.
    #[test]
    fn a_template_with_no_boards_drops_nothing() {
        let (_, dropped) = generate_for_new_engagement(
            SAMPLE,
            &SecretKey::new("sk-or-v1-x"),
            Path::new("engagement.toml"),
        )
        .expect("generates");
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    /// The generated file is validated HERE, so a template that would produce
    /// an unloadable config never reaches a recipient's machine.
    #[test]
    fn generating_a_config_that_would_not_load_fails_here() {
        // Pins two of the four required tools.
        let broken = "instructions = \"x\"\n\n[tools]\ntga = \"2.9.4\"\n";
        let error = generate(
            broken,
            &SecretKey::new("sk-or-v1-new"),
            Path::new("engagement.toml"),
        )
        .expect_err("an unloadable config is not a deliverable");
        assert!(matches!(error, AuditError::Parse { .. }), "{error}");

        let not_toml = "this is not = = toml";
        let error = generate(
            not_toml,
            &SecretKey::new("sk-or-v1-new"),
            Path::new("engagement.toml"),
        )
        .expect_err("a template that is not TOML is refused");
        assert!(matches!(error, AuditError::Parse { .. }), "{error}");
    }

    /// A model rename must be fixable in the engagement file, and a config that
    /// says nothing about models must keep loading (#5671).
    #[test]
    fn a_models_table_loads_and_is_optional() {
        let bare =
            EngagementConfig::from_toml(SAMPLE, Path::new("engagement.toml")).expect("parses");
        assert_eq!(bare.models, ModelPins::default());

        let text = format!("{SAMPLE}\n[models]\nreviewer = \"anthropic/claude-opus-4.8\"\n");
        let pinned =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("parses");
        assert_eq!(
            pinned.models.reviewer.as_deref(),
            Some("anthropic/claude-opus-4.8")
        );
        assert!(pinned.models.verifier.is_none());
    }

    /// A misspelled role must not fall back to the built-in slug in silence —
    /// the operator would only find out from the bill.
    #[test]
    fn a_typo_in_the_models_table_is_a_parse_error() {
        let text = format!("{SAMPLE}\n[models]\nreviewr = \"anthropic/claude-opus-4.8\"\n");
        let err = EngagementConfig::from_toml(&text, Path::new("engagement.toml"))
            .expect_err("`reviewr` is not a role");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
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

    /// The board table loads, and neither credential reaches a `Debug` render
    /// — the same guarantee the OpenRouter key has carried since #5473 (#5822).
    #[test]
    fn board_credentials_load_and_redact() {
        let text = format!(
            "{SAMPLE}\n[boards.jira]\nurl = \"https://acme.atlassian.net\"\n\
             email = \"auditor@acme.example\"\ntoken = \"jira-token-secret\"\n\
             \n[boards.linear]\napi_key = \"lin_api_secret\"\n"
        );
        let cfg = EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("parses");

        let jira = cfg.boards.jira.as_ref().expect("jira is configured");
        assert_eq!(jira.url, "https://acme.atlassian.net");
        assert_eq!(jira.email, "auditor@acme.example");
        assert_eq!(jira.token.expose(), "jira-token-secret");
        assert_eq!(
            cfg.boards
                .linear
                .as_ref()
                .expect("linear is configured")
                .api_key
                .expose(),
            "lin_api_secret"
        );

        let debug = format!("{cfg:?}");
        assert!(
            !debug.contains("jira-token-secret"),
            "Debug leaked: {debug}"
        );
        assert!(!debug.contains("lin_api_secret"), "Debug leaked: {debug}");
    }

    /// The list both credential guards draw from holds every secret the config
    /// carries, and shrinks to just the OpenRouter key when no board is given.
    ///
    /// The count assertions are the point: a new secret field added to the
    /// schema and not to `configured_secrets` fails here, rather than reaching
    /// a child's log or a handoff archive unnoticed.
    #[test]
    fn configured_secrets_covers_every_credential() {
        let text = format!(
            "{SAMPLE}\n[boards.jira]\nurl = \"https://acme.atlassian.net\"\n\
             email = \"auditor@acme.example\"\ntoken = \"jira-token-secret\"\n\
             \n[boards.linear]\napi_key = \"lin_api_secret\"\n"
        );
        let cfg = EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("parses");

        let secrets = cfg.configured_secrets();
        assert!(
            secrets.contains(&cfg.openrouter_key.expose()),
            "{secrets:?}"
        );
        assert!(secrets.contains(&"jira-token-secret"), "{secrets:?}");
        assert!(secrets.contains(&"lin_api_secret"), "{secrets:?}");
        assert_eq!(secrets.len(), 3, "{secrets:?}");

        let bare = EngagementConfig::from_toml(SAMPLE, Path::new("engagement.toml"))
            .expect("parses without boards");
        assert_eq!(
            bare.configured_secrets(),
            vec![bare.openrouter_key.expose()]
        );
    }

    /// `boards.jira.url` is a plain `String` an operator may have pasted
    /// credentials into — `crate::validate`'s own fixture does exactly that —
    /// and the derived `Debug` printed them verbatim (#5822).
    #[test]
    fn a_url_carrying_credentials_never_reaches_debug() {
        const URL: &str = "https://auditor:hunter2@acme.atlassian.net/jira";
        let text = format!(
            "{SAMPLE}\n[boards.jira]\nurl = \"{URL}\"\n\
             email = \"auditor@acme.example\"\ntoken = \"jira-token-secret\"\n"
        );
        let cfg = EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("parses");

        // The value itself is untouched — the request still has to carry it.
        assert_eq!(
            cfg.boards.jira.as_ref().expect("jira is configured").url,
            URL
        );

        let debug = format!("{cfg:?}");
        assert!(
            !debug.contains("hunter2"),
            "Debug leaked the password: {debug}"
        );
        assert!(
            !debug.contains("auditor:"),
            "Debug leaked the userinfo: {debug}"
        );
        assert!(
            !debug.contains("jira-token-secret"),
            "Debug leaked: {debug}"
        );
        assert!(
            debug.contains("acme.atlassian.net/jira"),
            "the site must still be identifiable: {debug}"
        );
    }

    #[test]
    fn redaction_leaves_a_url_without_userinfo_alone() {
        for url in [
            "https://acme.atlassian.net",
            "https://acme.atlassian.net/rest/api/3?a=b",
            "https://acme.atlassian.net/path@notuserinfo",
            "not a url at all",
            "",
        ] {
            assert_eq!(redact_userinfo(url), url, "{url}");
        }
    }

    /// A repo-only engagement never names a board, so the table defaults.
    #[test]
    fn a_config_with_no_boards_table_still_loads() {
        let cfg =
            EngagementConfig::from_toml(SAMPLE, Path::new("engagement.toml")).expect("parses");
        assert!(cfg.boards.jira.is_none());
        assert!(cfg.boards.linear.is_none());
    }

    /// 🔴 #6247: the engagement gets a place to declare the investigation
    /// budget. Before this the two keys had no home in this file at all, so an
    /// operator asking for a wider sample had nowhere to ask.
    #[test]
    fn a_declared_investigation_budget_loads() {
        let cfg = EngagementConfig::from_toml(
            &format!(
                "{SAMPLE}\n[report]\ninvestigate_max_files = 240\ninvestigate_max_bytes = 2457600\n"
            ),
            Path::new("engagement.toml"),
        )
        .expect("parses");
        assert_eq!(cfg.report.investigate_max_files, Some(240));
        assert_eq!(cfg.report.investigate_max_bytes, Some(2_457_600));
    }

    /// Every config written before the section existed still loads, and reads
    /// as "declares nothing" rather than as "declares zero".
    #[test]
    fn a_config_with_no_report_table_still_loads() {
        let cfg =
            EngagementConfig::from_toml(SAMPLE, Path::new("engagement.toml")).expect("parses");
        assert!(cfg.report.investigate_max_files.is_none());
        assert!(cfg.report.investigate_max_bytes.is_none());
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

    /// #5797: the guided flow reports on a working directory that may hold no
    /// config yet, so absence is a state to report rather than a failure.
    #[test]
    fn an_absent_config_is_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("engagement.toml");
        assert!(
            EngagementConfig::load_if_present(&missing)
                .expect("absent is fine")
                .is_none()
        );
    }

    /// The other half of that contract: `load_if_present` tolerates absence and
    /// nothing else. A config that is present and wrong must not read as one
    /// that is not there — that would silently drop the pins.
    #[test]
    fn a_malformed_config_is_still_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("engagement.toml");
        std::fs::write(&path, "this is not toml = = =").expect("write");
        let err = EngagementConfig::load_if_present(&path).expect_err("malformed must fail");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
    }
}
