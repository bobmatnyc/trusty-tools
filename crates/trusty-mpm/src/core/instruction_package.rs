//! Instruction package — the sectioned-JSON source format for the PM prompt.
//!
//! Why: the PM system prompt is composed today by concatenating four
//! `include_str!` markdown assets plus a handful of project overrides
//! ([`crate::core::instruction_pipeline`],
//! [`crate::core::instruction_overrides`]). That shape has no place to record
//! *who may override what*, and it has already shipped two composition bugs
//! that a typed source format makes structurally hard to repeat: the delivered
//! prompt named 8 agents because a computed 42-agent roster was dropped at the
//! final composition step (#4196), and the non-overridable floor could not be
//! reasoned about per-unit (#4194). Epic #4183 replaces the concatenated blobs
//! with one JSON package; this module is the schema half of that work (#4184).
//!
//! What: the [`InstructionPackage`] type and its JSON schema
//! (`assets/instructions/instruction-package.schema.json`, embedded as
//! [`SCHEMA_JSON`]). A package has two arrays:
//!
//! * `sections` — the closed eight-member taxonomy (five content sections plus
//!   the three absorbed BASE_PM floor sections) carrying the
//!   `customization_tier` axis. Declaration order is fixed and canonical; it
//!   has **no** effect on composed bytes.
//! * `blocks` — the ordered composition stream. Output is `blocks` in array
//!   order, and nothing else. A section may own several non-contiguous blocks,
//!   which is what lets a section be lifted out of the middle of a legacy asset
//!   without moving a byte.
//!
//! Determinism (the #4186 acceptance constraint — the composed prompt must be
//! byte-identical for the same package and roster):
//!
//! * order comes from `Vec`, never from a map — no hash iteration order can
//!   reach the output;
//! * every inter-block boundary is an explicit [`Join`] literal, so whitespace
//!   is declared rather than inferred;
//! * [`InstructionPackage::compose`] is pure — no clock, env, filesystem or
//!   randomness — so it is a total function of `(package, inputs)`;
//! * dropping content requires an explicit `optional: true`; anything else that
//!   would render empty is a hard error, so a roster can never be silently lost
//!   the way #4196 lost it.
//!
//! Strictness, and why it beats forward compatibility here. Every object in the
//! deserialization path carries `deny_unknown_fields`, so an unrecognised key is
//! a named parse error rather than dropped instruction content. The usual
//! argument against that — an older binary should be able to read a newer
//! package — does not apply to this format, because [`Self::validate`] gates on
//! [`SCHEMA_VERSION`] *before* any field is inspected: a package written against
//! a later schema is already rejected outright. Lenient field handling could
//! therefore only matter for a change that added a field while leaving
//! `schema_version` at 1, and that is exactly the case that must not be
//! tolerated — the old binary would compose a prompt missing the new field's
//! effect, silently, and report success. Since this artifact *is* the
//! instruction set for every PM and agent, a loud "unsupported schema_version"
//! is strictly better than a quiet, subtly wrong system prompt.
//!
//! The resulting evolution policy, which #4185/#4186 and any later schema work
//! must follow: **any field addition that can change composed bytes bumps
//! [`SCHEMA_VERSION`]**. Strict rejection is what makes that policy enforceable
//! rather than advisory.
//!
//! Version history:
//!
//! | version | change | ticket |
//! |---|---|---|
//! | 1 | initial format: `text` and `generated` bodies | #4184 |
//! | 2 | adds the `file` body kind | #4318 |
//!
//! v2 exists because #4318 made the bundled package an *authored JSON artifact*
//! rather than a Rust literal, and inlining every section's prose would have
//! turned `sections/core.md` into a single 23 KB JSON line. A `file` body names a
//! bundled markdown source instead; resolution goes through the compile-time
//! [`crate::core::instruction_pipeline::SECTION_SOURCES`] table, so the build
//! stays hermetic and a renamed section is a compile error. The bump is mandatory
//! under the policy above: a v1 build reading a v2 manifest would reject the
//! unknown `kind`, which is the correct loud failure.
//!
//! Floor block ordering is deliberately *not* constrained to canonical section
//! order — see `validate_floor_is_last` for the asset evidence.
//!
//! Scope: this module defines and validates the shape. It authors no
//! instruction content (#4185) and re-sources no build (#4186) — nothing in the
//! session-launch path calls it yet.
//!
//! Test: `instruction_package_tests.rs`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The schema major version this build implements.
///
/// Why: a package written against a future schema must be rejected outright
/// rather than partially interpreted — a half-understood instruction package is
/// a silently truncated system prompt.
/// What: the integer matched against [`InstructionPackage::schema_version`].
/// Test: `rejects_unsupported_schema_version`.
pub const SCHEMA_VERSION: u32 = 2;

/// The JSON Schema document describing this format, embedded at compile time.
///
/// Why: the schema is the artifact other tools (editors, validators, the
/// forthcoming `tm` authoring commands) consume; embedding it keeps it from
/// drifting away from the Rust types silently.
/// What: the raw contents of `assets/instructions/instruction-package.schema.json`.
/// Test: `schema_enums_match_rust_enums`, `schema_example_deserializes_validates_and_round_trips`.
pub const SCHEMA_JSON: &str =
    include_str!("../assets/instructions/instruction-package.schema.json");

/// The closed instruction section taxonomy.
///
/// Why: a closed enum makes the taxonomy reviewable and makes "every section is
/// accounted for" a compile-time-shaped question rather than a grep. Unknown
/// ids fail deserialization loudly instead of being dropped.
/// What: the five content sections (Core, Memory, Search, Workflow, Agent
/// Delegation) plus the three BASE_PM floor sections absorbed by #4183
/// (Identity, Non-Overridable Rules, Framework-Guaranteed Conventions).
///
/// Documented placement of each absorbed BASE_PM block:
///
/// | `BASE_PM.md` block | section |
/// |---|---|
/// | `## Identity` | [`SectionId::Identity`] |
/// | `## Non-Overridable Rules` | [`SectionId::NonOverridableRules`] |
/// | `## Customizing PM Behavior` | [`SectionId::NonOverridableRules`] |
/// | `## Trusty Tool Priority (Non-Overridable)` | [`SectionId::NonOverridableRules`] |
/// | `## Framework-Guaranteed Conventions (Non-Overridable)` | [`SectionId::FrameworkGuaranteedConventions`] |
///
/// The tool-priority block stays whole in the fixed floor deliberately: the
/// *mandate* to reach for memory and code search before grep is non-overridable
/// even though the memory/search *guidance* sections a project may tune are
/// tier `project`.
///
/// Test: `canonical_order_is_sorted_and_complete`, `schema_enums_match_rust_enums`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SectionId {
    /// Absorbed BASE_PM `## Identity` — who the PM is. Floor, tier `fixed`.
    Identity,
    /// The PM's core operating instructions (today's `PM_INSTRUCTIONS.md` body).
    Core,
    /// Memory protocol guidance.
    Memory,
    /// Code/architecture search guidance.
    Search,
    /// The phase workflow (today's `WORKFLOW.md`).
    Workflow,
    /// Delegation routing — dynamic, built from the deployed-agent roster.
    AgentDelegation,
    /// Absorbed BASE_PM non-overridable rules + customization contract + the
    /// Trusty tool-priority mandate. Floor, tier `fixed`.
    NonOverridableRules,
    /// Absorbed BASE_PM framework-guaranteed conventions (attribution footer,
    /// documentation proportionality, ticket attribution). Floor, tier `fixed`.
    FrameworkGuaranteedConventions,
}

impl SectionId {
    /// Every section id, in canonical declaration order.
    ///
    /// Why: `sections` must be declared in this order so a package manifest
    /// reads the same way in every project; the order is also the enum's `Ord`,
    /// so the check is a simple sortedness test.
    /// What: the eight ids, floor-first-and-last around the five content
    /// sections.
    /// Test: `canonical_order_is_sorted_and_complete`.
    pub const CANONICAL: [SectionId; 8] = [
        SectionId::Identity,
        SectionId::Core,
        SectionId::Memory,
        SectionId::Search,
        SectionId::Workflow,
        SectionId::AgentDelegation,
        SectionId::NonOverridableRules,
        SectionId::FrameworkGuaranteedConventions,
    ];

    /// Whether this section is part of the absorbed BASE_PM framework floor.
    ///
    /// Why: the floor's defining property — nothing may override it, and
    /// nothing overridable may follow it — has to be checkable per section, not
    /// per file (#4194: a floor rule shipped a broken directive that every
    /// obeying agent had to follow, with no per-unit handle on it).
    /// What: true for [`SectionId::Identity`], [`SectionId::NonOverridableRules`]
    /// and [`SectionId::FrameworkGuaranteedConventions`].
    /// Test: `rejects_floor_section_that_is_not_fixed`, `rejects_overridable_block_after_the_floor`.
    pub const fn is_floor(self) -> bool {
        matches!(
            self,
            SectionId::Identity
                | SectionId::NonOverridableRules
                | SectionId::FrameworkGuaranteedConventions
        )
    }
}

/// Who may override a section, by increasing permissiveness.
///
/// Why: the override permission axis is the whole point of the sectioned
/// format; encoding it as a type makes "can this be overridden, and by whom?" a
/// question with one answer instead of a per-file convention.
/// What: `Fixed < Project < User` (SPEC-PMINSTR-01 §4b). `Project` deliberately
/// excludes user-tier override so one operator's machine config can never leak
/// into a shared project; `User` implies project-tier may also override, with
/// project winning on collision (most-specific-wins).
/// Test: `tier_ordering_is_by_permissiveness`, `tier_permits_matrix`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustomizationTier {
    /// No override mechanism touches this section, at any tier.
    Fixed,
    /// A project-tier override may replace it. User tier may not.
    Project,
    /// Both project- and user-tier overrides may replace it.
    User,
}

/// The tier an override arrives from.
///
/// Why: [`CustomizationTier::permits`] needs to distinguish the *declared*
/// permission from the *incoming* attempt; conflating them is how "user config
/// silently changed a shared project" bugs happen.
/// What: the two writable tiers.
/// Test: `tier_permits_matrix`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrideTier {
    /// An override authored in the project (`<project>/.trusty-mpm/`).
    Project,
    /// An override authored in the operator's home config (`~/.trusty-mpm/`).
    User,
}

impl CustomizationTier {
    /// Whether an override arriving from `from` may replace this section.
    ///
    /// Why: the single decision point for override permission, so no caller can
    /// re-derive it differently.
    /// What: `Fixed` permits nothing; `Project` permits only
    /// [`OverrideTier::Project`]; `User` permits both.
    /// Test: `tier_permits_matrix`.
    pub const fn permits(self, from: OverrideTier) -> bool {
        match (self, from) {
            (CustomizationTier::Fixed, _) => false,
            (CustomizationTier::Project, OverrideTier::Project) => true,
            (CustomizationTier::Project, OverrideTier::User) => false,
            (CustomizationTier::User, _) => true,
        }
    }
}

/// A named composition-time input the host must supply.
///
/// Why: dynamic content (the deployed-agent roster, the detected stack profile,
/// the project's own addendum) cannot be authored in the package — it is
/// computed per project and session. Naming each one in the schema makes it a
/// declared dependency rather than something a composer may forget to pass.
/// What: the three generators the composer resolves from
/// [`CompositionInputs`].
/// Test: `rejects_package_that_never_consumes_the_roster`, `composes_blocks_in_array_order_with_declared_joins`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Generator {
    /// The rendered delegation roster built from the deployed agents.
    AgentRoster,
    /// The per-project detected stack profile.
    StackProfile,
    /// The project's additive instruction addendum.
    ProjectAddendum,
}

/// The literal bytes inserted before a block when it is not emitted first.
///
/// Why: byte-identical composition is only achievable if every boundary is
/// declared. Blocks split out of one legacy document must rejoin with a plain
/// paragraph break; distinct legacy assets must rejoin with the `---` rule.
/// Inferring that from context would make the output fragile.
/// What: `Rule` = `"\n\n---\n\n"` (matching
/// [`crate::core::instruction_pipeline::SECTION_SEPARATOR`]), `Blank` =
/// `"\n\n"`, `None` = `""`, `Literal` = verbatim.
/// Test: `join_literals_are_exact`, `composes_blocks_in_array_order_with_declared_joins`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Join {
    /// A Markdown horizontal rule: `"\n\n---\n\n"`. The default — it matches
    /// [`crate::core::instruction_pipeline::SECTION_SEPARATOR`], the boundary
    /// today's composer uses between top-level sections.
    #[default]
    Rule,
    /// A paragraph break: `"\n\n"`.
    Blank,
    /// No separator at all.
    None,
    /// Verbatim separator bytes.
    Literal(String),
}

impl Join {
    /// The exact bytes this join contributes.
    ///
    /// Test: `join_literals_are_exact`.
    pub fn as_str(&self) -> &str {
        match self {
            Join::Rule => "\n\n---\n\n",
            Join::Blank => "\n\n",
            Join::None => "",
            Join::Literal(s) => s,
        }
    }
}

/// Where a block's markdown comes from.
///
/// Why: separating authored text from host-supplied generated content lets
/// validation insist that generated content is actually consumed.
/// What: `Text` carries markdown authored in the package; `File` names a bundled
/// markdown source resolved through the compile-time
/// [`crate::core::instruction_pipeline::SECTION_SOURCES`] table; `Generated`
/// names the generator that supplies it.
///
/// `File` is the schema-v2 addition (#4318). It exists because the two obvious
/// alternatives are both bad: inlining `sections/core.md` verbatim turns 23 KB of
/// reviewable prose into one unreadable JSON line and moves the floor text out of
/// the files `scripts/check_instruction_floor.sh` greps, while resolving the path
/// on the filesystem at launch would make the delivered system prompt depend on
/// what happens to be on disk. Resolving through `include_str!` keeps the prose in
/// markdown, keeps the build hermetic, and makes a renamed section a compile error.
/// `Text` remains fully expressible, and is how a rule authored *in the manifest*
/// — rather than in a section file — is carried.
///
/// `deny_unknown_fields` is load-bearing, not decoration. Without it a key
/// misplaced *inside* `body` — `{"kind":"text","text":"…","join_before":"blank"}`
/// — was silently discarded, the block fell back to the [`Join::Rule`] default,
/// and a spurious horizontal rule appeared in the composed system prompt while
/// the package parsed, validated and composed without complaint. That is the
/// #4196 failure shape (a package reporting success while composing the wrong
/// instructions), and it also restores parity with the shipped JSON Schema,
/// which sets `additionalProperties: false` on both `body` variants. The tag
/// key itself (`kind`) is consumed by the internal tagging and never reported
/// as unknown.
/// Test: `rejects_unknown_fields_at_every_level_and_names_the_key`, `schema_document_closes_every_object`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BlockBody {
    /// Markdown authored inline in the package.
    Text {
        /// The markdown body. Trimmed before emission.
        text: String,
    },
    /// Markdown read from a bundled section source, named by path.
    File {
        /// Path relative to `assets/instructions/`, e.g. `sections/core.md`.
        /// Must be a key of
        /// [`crate::core::instruction_pipeline::SECTION_SOURCES`]; anything else
        /// is [`ValidationError::UnknownFileSource`]. Trimmed before emission.
        path: String,
    },
    /// Markdown supplied at composition time by a named generator.
    Generated {
        /// Which composition-time input supplies this block.
        generator: Generator,
    },
}

impl BlockBody {
    /// The authored markdown this body carries, before trimming.
    ///
    /// Why: `Text` and `File` differ only in where the bytes are stored, and
    /// every caller that cares about authored content — validation, composition,
    /// [`InstructionPackage::authored_run`], and the `CLAUDE.md` override
    /// application — must treat them identically. One accessor is what stops the
    /// v2 addition from having to be remembered at four call sites, which is
    /// exactly how a `File` block would otherwise become silently
    /// non-overridable.
    /// What: `None` for [`BlockBody::Generated`]; `Some(Ok(markdown))` for an
    /// authored body; `Some(Err(path))` for a `File` body naming a source that is
    /// not in the bundled table.
    /// Test: `file_body_resolves_through_the_bundled_table`,
    /// `unknown_file_source_is_rejected`.
    pub fn authored(&self) -> Option<Result<&str, &str>> {
        match self {
            BlockBody::Text { text } => Some(Ok(text.as_str())),
            BlockBody::File { path } => {
                Some(crate::core::instruction_pipeline::section_source(path).ok_or(path.as_str()))
            }
            BlockBody::Generated { .. } => None,
        }
    }
}

/// One unit of the ordered composition stream.
///
/// Why: composition order is block order, so the block is the smallest thing
/// that has a position. Attributing each block to a section (rather than
/// nesting blocks under sections) is what lets one section contribute at
/// several non-adjacent positions — the property a faithful, byte-identical
/// lift of today's assets needs.
/// What: the owning section, the body, the join preceding it, and whether it
/// may be dropped when its generator has nothing to supply.
/// Test: `schema_example_deserializes_validates_and_round_trips`, `optional_generated_block_is_dropped_with_its_join`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionBlock {
    /// The section this block's content belongs to.
    pub section: SectionId,
    /// Where the content comes from.
    pub body: BlockBody,
    /// Separator emitted before this block (ignored for the first emitted
    /// block). Defaults to [`Join::Rule`].
    #[serde(default, skip_serializing_if = "is_default_join")]
    pub join_before: Join,
    /// Whether this block may be dropped when its generator supplies nothing.
    ///
    /// Only a [`BlockBody::Generated`] block may set this. Everything else that
    /// would render empty is a hard [`CompositionError`], never a silent drop —
    /// the structural guard against #4196.
    #[serde(default, skip_serializing_if = "is_false")]
    pub optional: bool,
}

/// Serde helper: skip serializing `join_before` when it is the default.
fn is_default_join(join: &Join) -> bool {
    *join == Join::Rule
}

/// Serde helper: skip serializing `optional` when false.
fn is_false(b: &bool) -> bool {
    !*b
}

/// A declared section: identity plus its customization tier.
///
/// Why: the taxonomy and the tier axis are the package's contract with
/// override machinery; keeping them out of the block stream means a section's
/// permission cannot vary by position.
/// What: id, human title, tier, optional prose description.
/// Test: `schema_example_deserializes_validates_and_round_trips`, `rejects_floor_section_that_is_not_fixed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionSection {
    /// Which section this is.
    pub id: SectionId,
    /// Human-readable title (documentation only; never emitted).
    pub title: String,
    /// Who may override this section.
    pub customization_tier: CustomizationTier,
    /// Optional prose describing the section's remit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A complete PM instruction package.
///
/// Why: one versioned, validatable artifact replaces four `include_str!` blobs
/// whose ordering and override semantics lived only in prose.
/// What: schema version, identity, the eight-section taxonomy, and the ordered
/// block stream. Because the last block is trimmed before emission, the tail is
/// always either `X` or `X\n`; `trailing_newline` selects between those two,
/// which covers both legacy composers (`resolve_pm_prompt` emits none, a
/// file-shaped artifact emits one). Two or more trailing newlines are
/// deliberately inexpressible — if a future target needs them, they belong in a
/// final block's [`Join`], not in this flag.
/// Test: the whole of `instruction_package_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionPackage {
    /// Schema major version; must equal [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable package identity. Never affects composed bytes.
    pub package_id: String,
    /// Optional prose describing the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Append exactly one `\n` after the last emitted block.
    #[serde(default, skip_serializing_if = "is_false")]
    pub trailing_newline: bool,
    /// The section taxonomy, in canonical order, each id exactly once.
    pub sections: Vec<InstructionSection>,
    /// The ordered composition stream. Output order is exactly this order.
    pub blocks: Vec<InstructionBlock>,
}

/// A structural defect in a package.
///
/// Why: every variant here is a defect that would otherwise surface as a
/// quietly wrong system prompt — the failure mode #4194 and #4196 both had.
/// What: the checks [`InstructionPackage::validate`] performs, in the order it
/// performs them (first failure wins, deterministically).
/// Test: one test per variant in `instruction_package_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// `schema_version` is not [`SCHEMA_VERSION`].
    #[error("unsupported schema_version {found}; this build implements {SCHEMA_VERSION}")]
    UnsupportedSchemaVersion {
        /// The version the package declared.
        found: u32,
    },
    /// `package_id` is empty or whitespace.
    #[error("package_id must not be empty")]
    EmptyPackageId,
    /// `sections` is not exactly the canonical set in canonical order.
    #[error(
        "sections must list every canonical section exactly once, in canonical order; \
         got {found:?}"
    )]
    SectionsNotCanonical {
        /// The ids as declared.
        found: Vec<SectionId>,
    },
    /// A floor section declared a tier other than `fixed`.
    #[error("floor section {section:?} must be tier `fixed`, got {tier:?}")]
    FloorNotFixed {
        /// The offending section.
        section: SectionId,
        /// The tier it declared.
        tier: CustomizationTier,
    },
    /// The package emits nothing.
    #[error("blocks must not be empty")]
    NoBlocks,
    /// An authored (`text` or `file`) block whose content is blank.
    #[error("block {index} ({section:?}) has an empty authored body; remove it instead")]
    EmptyAuthoredBody {
        /// Index into `blocks`.
        index: usize,
        /// The owning section.
        section: SectionId,
    },
    /// An authored (`text` or `file`) block marked optional.
    #[error("block {index} ({section:?}) is an authored block and may not be `optional`")]
    OptionalAuthoredBlock {
        /// Index into `blocks`.
        index: usize,
        /// The owning section.
        section: SectionId,
    },
    /// A `file` block naming a source that is not bundled with this build.
    ///
    /// Why a hard error and not an empty block: a renamed or mistyped section
    /// path is the #4196 shape exactly — content that exists, is referenced, and
    /// never reaches the prompt. The manifest is embedded at compile time, so
    /// this is caught by `bundled_manifest_parses_and_validates` in CI and can
    /// never be reached by a shipped build.
    #[error(
        "block {index} ({section:?}) references section source `{path}`, which this build \
         does not bundle"
    )]
    UnknownFileSource {
        /// Index into `blocks`.
        index: usize,
        /// The owning section.
        section: SectionId,
        /// The unresolvable path as authored.
        path: String,
    },
    /// No block consumes [`Generator::AgentRoster`].
    #[error(
        "no block consumes the agent roster; the computed roster must reach the \
         composed prompt (issue #4196)"
    )]
    RosterNotConsumed,
    /// The roster block is marked optional.
    #[error("block {index} consumes the agent roster and may not be `optional` (issue #4196)")]
    OptionalRoster {
        /// Index into `blocks`.
        index: usize,
    },
    /// A declared section contributes no block.
    #[error("section {section:?} is declared but contributes no block")]
    SectionWithoutBlocks {
        /// The silent section.
        section: SectionId,
    },
    /// A declared section's blocks are all `optional`, so it may emit nothing.
    #[error(
        "section {section:?} contributes only `optional` blocks, so it can emit nothing \
         while still validating; give it at least one non-optional block"
    )]
    SectionOnlyOptionalBlocks {
        /// The section that is not guaranteed to emit.
        section: SectionId,
    },
    /// A non-floor block follows the first floor block.
    #[error(
        "block {index} ({section:?}) is overridable but follows the framework floor \
         (first floor block at {floor_index}); nothing overridable may follow the floor"
    )]
    OverridableAfterFloor {
        /// Index of the offending block.
        index: usize,
        /// The offending block's section.
        section: SectionId,
        /// Index of the first floor block.
        floor_index: usize,
    },
}

/// A failure while composing a validated package.
///
/// Why: composition failures must be loud. A missing generator input is the
/// exact shape of #4196 — where the roster existed, was computed, and simply
/// never reached the output.
/// What: an invalid package, or a required generator that supplied nothing.
/// Test: `missing_required_generator_input_is_a_hard_error`, `whitespace_only_generator_output_counts_as_absent`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompositionError {
    /// The package failed [`InstructionPackage::validate`].
    #[error("invalid instruction package: {0}")]
    Invalid(#[from] ValidationError),
    /// The bundled manifest could not be parsed or validated (#4318).
    ///
    /// Distinct from [`Self::Invalid`]: that one carries a typed defect in a
    /// package the caller already holds, while this one says the JSON artifact
    /// never became a package at all. Unreachable for the shipped manifest —
    /// `bundled_manifest_parses_and_validates` gates it in CI — and the caller
    /// degrades to the legacy assembly rather than emitting a partial prompt.
    #[error("bundled instruction manifest is unusable: {0}")]
    Manifest(String),
    /// A non-optional generated block had no content to emit.
    #[error(
        "block {index} requires generator {generator:?} but it supplied no content; \
         mark the block `optional` if that is intended"
    )]
    MissingGeneratedInput {
        /// Index into `blocks`.
        index: usize,
        /// The generator that came up empty.
        generator: Generator,
    },
}

/// The host-supplied, composition-time inputs.
///
/// Why: making the roster a required, non-`Option` field of the composer's
/// input means a caller cannot compose without producing one — the final-step
/// drop that caused #4196 is not expressible.
/// What: the rendered roster (required) plus the two optional project inputs.
/// Test: `composes_blocks_in_array_order_with_declared_joins`, `missing_required_generator_input_is_a_hard_error`.
#[derive(Debug, Clone, Default)]
pub struct CompositionInputs {
    /// Rendered delegation roster. Required — see #4196.
    pub agent_roster: String,
    /// Rendered per-project stack profile, if the host derived one.
    pub stack_profile: Option<String>,
    /// The project's additive instruction addendum, if present.
    pub project_addendum: Option<String>,
}

impl InstructionPackage {
    /// Parse a package from JSON.
    ///
    /// Why: unknown fields are rejected (`deny_unknown_fields` throughout) so a
    /// typo in a key becomes a parse error rather than silently dropped
    /// instruction content.
    ///
    /// The `schema_version` probe runs FIRST, before the package is
    /// deserialized, because strictness and version-gating otherwise collide:
    /// a package written against a later schema almost certainly carries a field
    /// this build does not know, so plain deserialization would report `unknown
    /// field \`whatever\`` — blaming a key that is perfectly valid in its own
    /// schema — instead of the actionable "you need a newer trusty-mpm". The
    /// probe deliberately ignores unknown fields; it reads nothing but the
    /// version.
    ///
    /// What: version probe, then `serde_json::from_str`. No structural
    /// validation — call [`Self::validate`] or [`Self::compose`] for that.
    /// Test: `schema_example_deserializes_validates_and_round_trips`,
    /// `rejects_unknown_fields_at_every_level_and_names_the_key`,
    /// `later_schema_version_is_reported_as_a_version_error_not_a_field_error`.
    pub fn from_json(raw: &str) -> Result<Self, serde_json::Error> {
        /// Reads only `schema_version`, tolerating every other key by design.
        #[derive(Deserialize)]
        struct VersionProbe {
            schema_version: u32,
        }

        let probe: VersionProbe = serde_json::from_str(raw)?;
        if probe.schema_version != SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported schema_version {}; this build implements {SCHEMA_VERSION}",
                probe.schema_version
            )));
        }
        serde_json::from_str(raw)
    }

    /// Serialize back to pretty JSON.
    ///
    /// Why: round-tripping must be lossless so tooling can rewrite a package
    /// without churning unrelated bytes.
    /// What: `serde_json::to_string_pretty`; field order is struct declaration
    /// order, and both arrays keep their order.
    /// Test: `schema_example_deserializes_validates_and_round_trips`.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Look up a declared section.
    ///
    /// Test: `section_lookup_reports_declared_tier`.
    pub fn section(&self, id: SectionId) -> Option<&InstructionSection> {
        self.sections.iter().find(|s| s.id == id)
    }

    /// Check every structural invariant, first failure wins.
    ///
    /// Why: composition assumes these hold; checking them in one deterministic
    /// order gives authors a stable error and reviewers one place to read the
    /// contract.
    /// What: version, identity, canonical section set/order, floor tiers,
    /// non-empty block stream, per-block text/optional sanity, every section
    /// contributing *and being guaranteed to emit* (owning blocks is not
    /// enough — a section covered only by `optional` blocks composes to
    /// nothing), the roster being consumed and non-optional, and nothing
    /// overridable following the floor.
    /// Test: one test per [`ValidationError`] variant.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedSchemaVersion {
                found: self.schema_version,
            });
        }
        if self.package_id.trim().is_empty() {
            return Err(ValidationError::EmptyPackageId);
        }

        let declared: Vec<SectionId> = self.sections.iter().map(|s| s.id).collect();
        if declared != SectionId::CANONICAL {
            return Err(ValidationError::SectionsNotCanonical { found: declared });
        }

        for section in &self.sections {
            if section.id.is_floor() && section.customization_tier != CustomizationTier::Fixed {
                return Err(ValidationError::FloorNotFixed {
                    section: section.id,
                    tier: section.customization_tier,
                });
            }
        }

        if self.blocks.is_empty() {
            return Err(ValidationError::NoBlocks);
        }

        for (index, block) in self.blocks.iter().enumerate() {
            let Some(authored) = block.body.authored() else {
                continue;
            };
            let body = match authored {
                Ok(body) => body,
                Err(path) => {
                    return Err(ValidationError::UnknownFileSource {
                        index,
                        section: block.section,
                        path: path.to_string(),
                    });
                }
            };
            if body.trim().is_empty() {
                return Err(ValidationError::EmptyAuthoredBody {
                    index,
                    section: block.section,
                });
            }
            if block.optional {
                return Err(ValidationError::OptionalAuthoredBlock {
                    index,
                    section: block.section,
                });
            }
        }

        // Roster checks run BEFORE section coverage so that an optional roster
        // block reports the precise `OptionalRoster` (#4196) diagnosis rather
        // than the broader "this section may emit nothing" one — both are true,
        // but only the first names the actual mistake.
        self.validate_roster()?;

        // A section must not merely OWN blocks — it must own at least one block
        // that is guaranteed to emit. A section covered only by `optional`
        // blocks passes the ownership check and still composes to nothing,
        // which is precisely the silent-missing-section shape this check exists
        // to prevent (#4069/#4196: the taxonomy claims the content is
        // delivered, the output does not contain it).
        let covered: BTreeSet<SectionId> = self.blocks.iter().map(|b| b.section).collect();
        let guaranteed: BTreeSet<SectionId> = self
            .blocks
            .iter()
            .filter(|b| !b.optional)
            .map(|b| b.section)
            .collect();
        for id in SectionId::CANONICAL {
            if !covered.contains(&id) {
                return Err(ValidationError::SectionWithoutBlocks { section: id });
            }
            if !guaranteed.contains(&id) {
                return Err(ValidationError::SectionOnlyOptionalBlocks { section: id });
            }
        }

        self.validate_floor_is_last()
    }

    /// The roster must reach the output, and must not be droppable (#4196).
    ///
    /// What today's launch path actually does, since #4186 lifts exactly these
    /// bytes: `instruction_overrides::resolve_pm_prompt` resolves its delegation
    /// section through `bundled_delegation`, which since #4196 (closing #4069)
    /// appends the LIVE computed roster from
    /// `delegation_authority::deployed_roster_section` to the bundled
    /// `AGENT_DELEGATION` asset, separated by `ROSTER_PRECEDENCE_NOTE`. On this
    /// path the delivered prompt therefore already carries the real roster; the
    /// stale `PipelineOutput::merged` path that originally caused #4069 is no
    /// longer the launch path.
    ///
    /// Consequence for #4186, stated because an earlier revision of this comment
    /// got it wrong: byte-identity and roster consumption are **not** in
    /// tension. Today's section is
    /// `<asset>\n\n<ROSTER_PRECEDENCE_NOTE>\n\n<roster>`, which this schema
    /// expresses directly as three blocks — [`BlockBody::Text`] for the asset, a
    /// [`Join::Blank`] [`BlockBody::Text`] for the note, and a [`Join::Blank`]
    /// [`BlockBody::Generated`] for [`Generator::AgentRoster`] — so a package can
    /// be byte-identical to today's output AND consume the computed roster.
    /// #4186 should therefore keep **strict byte-equality against today's
    /// output** as its acceptance gate; it is the strongest faithfulness check
    /// available and nothing here forces weakening it.
    ///
    /// Scope — including the other override configurations `resolve_pm_prompt`
    /// can emit, and which of them this schema can reproduce — is tracked on
    /// #4186, not here.
    /// Test: `todays_delegation_section_shape_is_expressible`.
    fn validate_roster(&self) -> Result<(), ValidationError> {
        let mut seen = false;
        for (index, block) in self.blocks.iter().enumerate() {
            if matches!(
                block.body,
                BlockBody::Generated {
                    generator: Generator::AgentRoster
                }
            ) {
                if block.optional {
                    return Err(ValidationError::OptionalRoster { index });
                }
                seen = true;
            }
        }
        if seen {
            Ok(())
        } else {
            Err(ValidationError::RosterNotConsumed)
        }
    }

    /// Once the floor starts, only floor blocks may follow.
    ///
    /// Why: the floor's guarantee is that it has the last word. Enforcing
    /// "nothing overridable after the floor" preserves that without dictating a
    /// single global order — which the byte-identical lift in #4186 needs to
    /// stay free to choose.
    ///
    /// Deliberately NOT enforced: that floor blocks run in [`SectionId`]
    /// canonical order relative to each other. That constraint looks free — the
    /// floor is the contiguous tail — but it would make a byte-identical lift of
    /// `assets/instructions/BASE_PM.md` impossible. That file's headings run:
    ///
    /// | `BASE_PM.md` heading | section | canonical index |
    /// |---|---|---|
    /// | `## Identity` | `Identity` | 0 |
    /// | `## Non-Overridable Rules` | `NonOverridableRules` | 6 |
    /// | `## Customizing PM Behavior` | `NonOverridableRules` | 6 |
    /// | `## Framework-Guaranteed Conventions (Non-Overridable)` | `FrameworkGuaranteedConventions` | 7 |
    /// | `## Trusty Tool Priority (Non-Overridable)` | `NonOverridableRules` | 6 |
    ///
    /// The tool-priority block sits *after* framework-guaranteed-conventions in
    /// the real asset, so the faithful block stream is `0, 6, 6, 7, 6` — a
    /// canonical-order inversion. Requiring non-decreasing floor order would
    /// reject the only lift this schema exists to enable, and would do it by
    /// forcing #4186 to move bytes. The floor is in fact the sharpest example of
    /// why `sections` and `blocks` are separate arrays: one section owning
    /// non-contiguous positions is the normal case, not the exotic one.
    ///
    /// The residual risk — a semantically incoherent floor order that still
    /// validates — is the same trade-off already accepted for the five content
    /// sections, and is caught by #4186's byte comparison against today's
    /// output rather than by structural validation.
    /// Test: `base_pm_floor_block_order_is_valid`.
    fn validate_floor_is_last(&self) -> Result<(), ValidationError> {
        let Some(floor_index) = self.blocks.iter().position(|b| b.section.is_floor()) else {
            return Ok(());
        };
        for (index, block) in self.blocks.iter().enumerate().skip(floor_index + 1) {
            if !block.section.is_floor() {
                return Err(ValidationError::OverridableAfterFloor {
                    index,
                    section: block.section,
                    floor_index,
                });
            }
        }
        Ok(())
    }

    /// Compose the package into the final prompt text.
    ///
    /// Why: this is the executable definition of the format's ordering and
    /// whitespace semantics — the thing #4186 diffs against today's
    /// `resolve_pm_prompt` output.
    ///
    /// What: validates, then walks `blocks` in array order. Each block's body is
    /// resolved (authored text, or the named generator's input) and trimmed; a
    /// block that resolves to nothing is dropped when `optional`, and is a hard
    /// [`CompositionError::MissingGeneratedInput`] otherwise. Every emitted
    /// block after the first is preceded by its declared [`Join`] bytes.
    /// `trailing_newline` appends one `\n`.
    ///
    /// Determinism: pure function of `(self, inputs)` — no map iteration, no
    /// clock, no environment, no filesystem. Two calls with equal arguments
    /// return byte-identical strings.
    ///
    /// Test: `composes_blocks_in_array_order_with_declared_joins`,
    /// `first_emitted_block_never_contributes_a_join`,
    /// `compose_is_byte_identical_across_repeats_and_a_json_round_trip`.
    pub fn compose(&self, inputs: &CompositionInputs) -> Result<String, CompositionError> {
        self.validate()?;

        let mut out = String::new();
        let mut emitted = false;

        for (index, block) in self.blocks.iter().enumerate() {
            let resolved: Option<&str> = match &block.body {
                BlockBody::Text { .. } | BlockBody::File { .. } => match block.body.authored() {
                    Some(Ok(body)) => Some(body),
                    // `validate` above rejects an unresolvable `file` path.
                    _ => unreachable!("validate rejects unknown file sources"),
                },
                BlockBody::Generated { generator } => match generator {
                    Generator::AgentRoster => Some(inputs.agent_roster.as_str()),
                    Generator::StackProfile => inputs.stack_profile.as_deref(),
                    Generator::ProjectAddendum => inputs.project_addendum.as_deref(),
                },
            };

            let body = resolved.map(str::trim).unwrap_or("");
            if body.is_empty() {
                if block.optional {
                    continue;
                }
                let BlockBody::Generated { generator } = &block.body else {
                    // Blank authored blocks are rejected by `validate`.
                    unreachable!("validate rejects blank authored blocks");
                };
                return Err(CompositionError::MissingGeneratedInput {
                    index,
                    generator: *generator,
                });
            }

            if emitted {
                out.push_str(block.join_before.as_str());
            }
            out.push_str(body);
            emitted = true;
        }

        if self.trailing_newline && !out.is_empty() {
            out.push('\n');
        }
        Ok(out)
    }

    /// Concatenate the AUTHORED blocks of `sections`, in block order.
    ///
    /// Why: the legacy assembly in
    /// [`crate::core::instruction_overrides::assemble_sections`] and the
    /// roster-free [`crate::core::instruction_pipeline::assemble_system_prompt`]
    /// both need whole multi-section runs as one string, and neither can call
    /// [`Self::compose`] — one composes a different configuration, the other has
    /// no agent roster to satisfy the required `agent-roster` generator. Before
    /// #4318 they rebuilt those runs from the `include_str!` constants directly,
    /// which was fine only while every byte of prose lived in a section file. Now
    /// that the manifest may author a rule inline, rebuilding from the constants
    /// would deliver that rule to package-composed sessions and silently withhold
    /// it from every other path — a content split-brain. Projecting the manifest
    /// is what keeps ONE source of truth.
    ///
    /// What: walks `blocks` in array order, keeps blocks owned by `sections` whose
    /// body is authored ([`BlockBody::Text`] or [`BlockBody::File`]), trims each,
    /// and joins with each block's declared [`Join`] — skipping the join on the
    /// first emitted block, exactly as [`Self::compose`] does. `generated` blocks
    /// are skipped: they have no authored bytes, and their host inputs are folded
    /// in by the callers separately. An unresolvable `file` path is skipped rather
    /// than panicking, because [`Self::validate`] already rejects it at the one
    /// place that matters.
    ///
    /// Determinism: same guarantees as [`Self::compose`] — array order, declared
    /// joins, no map iteration, no I/O.
    ///
    /// Test: `authored_run_projects_blocks_in_order_with_joins`,
    /// `authored_run_skips_generated_blocks`,
    /// `pm_instructions_is_its_three_sections`.
    pub fn authored_run(&self, sections: &[SectionId]) -> String {
        let mut out = String::new();
        let mut emitted = false;

        for block in &self.blocks {
            if !sections.contains(&block.section) {
                continue;
            }
            let Some(Ok(body)) = block.body.authored() else {
                continue;
            };
            let body = body.trim();
            if body.is_empty() {
                continue;
            }
            if emitted {
                out.push_str(block.join_before.as_str());
            }
            out.push_str(body);
            emitted = true;
        }

        out
    }
}

#[cfg(test)]
#[path = "instruction_package_tests.rs"]
mod tests;
