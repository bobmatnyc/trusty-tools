//! The review record of the repo-named review skills a review used (#11899).
//!
//! Why: each repo can steer its own reviews with skill text, so verdicts stop
//! being comparable across repos unless the record names which skill text
//! shaped a review. A skipped skill must be visible too: the review never
//! fails because of a skill, so the record is where the skip is explained.
//! What: [`RepoSkillsRecord`] holds the base commit the skills were read at,
//! the fork flag, every loaded skill with its sha256, and every skipped skill
//! with a typed [`SkipKind`] and a reason.
//! Test: `loads_skill_and_records_name_and_hash`,
//! `every_skip_reason_is_recorded_and_named_in_the_body`.

use serde::{Deserialize, Serialize};

/// The repo skills one review asked for, and what became of each (#11899).
///
/// Why: acceptance criterion 4 of #11899 — the record lists the loaded skill
/// names and their content hashes.
/// What: `base_sha` is the commit every skill was read at (empty when none
/// was known); `fork` is true when the PR head is a fork and no skill was
/// read. Each requested skill appears in exactly one of `loaded`/`skipped`.
/// Test: `loads_skill_and_records_name_and_hash`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSkillsRecord {
    /// The base commit the skills were read at; empty when unknown.
    #[serde(default)]
    pub base_sha: String,
    /// The PR head is in a fork, so no repo skill text was read.
    #[serde(default)]
    pub fork: bool,
    /// Skills whose text reached the system prompt, in request order.
    #[serde(default)]
    pub loaded: Vec<LoadedSkill>,
    /// Skills that were requested but not used, with the reason.
    #[serde(default)]
    pub skipped: Vec<SkippedSkill>,
}

/// One skill whose text reached the system prompt (#11899).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedSkill {
    /// The skill name (the `.claude/skills/<name>` directory).
    pub name: String,
    /// Lowercase hex sha256 of the whole `SKILL.md` blob at the base commit.
    pub sha256: String,
    /// Size of that blob in bytes.
    pub bytes: usize,
}

/// One requested skill that was not used, and why (#11899).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedSkill {
    /// The requested name, as given (it may be an invalid name).
    pub name: String,
    /// The machine-readable class of the skip.
    pub kind: SkipKind,
    /// A one-line human reason, rendered into the review body.
    pub reason: String,
}

/// Why a requested skill was not used (#11899, DOC-93 RS-25).
///
/// Why: a consumer (and the tests) can branch on the class without parsing
/// the prose reason.
/// What: the DOC-93 RS-25 reasons, plus `malformed_entry`, `fork` and
/// `no_base`, which DOC-93 states in prose but does not name.
/// Test: `every_skip_reason_is_recorded_and_named_in_the_body`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipKind {
    /// `name` is absent or not a bare identifier (RS-5, RS-6).
    InvalidName,
    /// The same name appeared earlier; the first entry wins (RS-9).
    Duplicate,
    /// An `apply_to` glob did not parse (RS-8).
    InvalidGlob,
    /// No changed path matched `apply_to`; not a fault (RS-26).
    NotApplicable,
    /// No `SKILL.md` at that path at the base commit.
    NotFound,
    /// A read error, a symlink or non-file, non-UTF-8 text, or text holding
    /// the reserved block delimiter.
    Unreadable,
    /// RS-11, RS-12, RS-13 or RS-15 failed.
    InvalidFrontmatter,
    /// The file exceeds the per-skill cap (RS-23).
    OverSkillCap,
    /// Adding the skill would exceed the total cap (RS-24).
    OverTotalCap,
    /// A `[[review.skills]]` entry with an unknown key or a wrong-typed value.
    MalformedEntry,
    /// The PR head is a fork; no repository skill text is read (RS-21).
    Fork,
    /// No base commit is known or readable, so nothing can be read.
    NoBase,
}

impl SkipKind {
    /// The DOC-93 RS-25 label rendered in the review body.
    pub fn label(self) -> &'static str {
        match self {
            Self::InvalidName => "invalid name",
            Self::Duplicate => "duplicate",
            Self::InvalidGlob => "invalid glob",
            Self::NotApplicable => "not applicable",
            Self::NotFound => "not found",
            Self::Unreadable => "unreadable",
            Self::InvalidFrontmatter => "invalid frontmatter",
            Self::OverSkillCap => "over per-skill cap",
            Self::OverTotalCap => "over total cap",
            Self::MalformedEntry => "malformed entry",
            Self::Fork => "fork pull request",
            Self::NoBase => "no base commit",
        }
    }
}
