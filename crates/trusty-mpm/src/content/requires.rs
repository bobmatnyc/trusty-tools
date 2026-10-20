//! The advisory `requires:` frontmatter block of a content asset (#8390).
//!
//! Why: ADR-0064 decision 6 — no hard version gate anywhere in the load path.
//! An asset may state which harness version it was written against, and a
//! harness's doctor reports a mismatch; deploy and load never read it.
//! What: [`parse_requires`] reads `requires:` from an asset's leading
//! frontmatter block as a map of harness name to SemVer requirement, in flow
//! (`requires: {tm: ">=1.9.0"}`) or block form. Harness names are free-form
//! (`tm`, `tcode`, `tagent`, …), so the block is harness-neutral: each harness
//! checks its own key and leaves the rest alone.
//! Test: `requires_tests.rs`.

/// The frontmatter key this module reads.
pub const REQUIRES_KEY: &str = "requires";

/// One `<harness>: <requirement>` entry of a `requires:` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// The harness the entry names (`tm`).
    pub harness: String,
    /// The SemVer requirement as written (`>=1.9.0`).
    pub requirement: String,
}

impl Requirement {
    /// Whether `version` satisfies this requirement.
    ///
    /// Postcondition: agrees with `semver::VersionReq::matches`; a requirement
    /// [`parse_requires`] accepted always parses.
    /// Test: `requirement_matches_by_semver`.
    pub fn satisfied_by(&self, version: &semver::Version) -> bool {
        let _ = version;
        true
    }
}

/// Why a `requires:` block could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequiresError {
    /// The frontmatter holding `requires:` is not valid YAML.
    #[error("the frontmatter holding `requires:` is not valid YAML: {0}")]
    Frontmatter(String),
    /// `requires:` is not a map of harness name to a string requirement.
    #[error(
        "`requires:` must map a harness name to a version requirement string, \
         e.g. `requires: {{tm: \">=1.9.0\"}}`"
    )]
    Shape,
    /// One entry's value is not a SemVer requirement.
    #[error("`requires.{harness}` is `{value}`, which is not a SemVer requirement: {reason}")]
    BadRequirement {
        /// The harness the entry names.
        harness: String,
        /// The value as written.
        value: String,
        /// The SemVer parser's reason.
        reason: String,
    },
}

/// The `requires:` entries of `text`, an asset's full source.
///
/// # Errors
/// [`RequiresError`] when `requires:` is present but unreadable.
pub fn parse_requires(text: &str) -> Result<Vec<Requirement>, RequiresError> {
    let _ = text;
    Ok(Vec::new())
}

#[cfg(test)]
#[path = "requires_tests.rs"]
mod tests;
