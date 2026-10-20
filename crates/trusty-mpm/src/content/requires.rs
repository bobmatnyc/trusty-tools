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
        semver::VersionReq::parse(&self.requirement).is_ok_and(|req| req.matches(version))
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
/// Why: the doctor row and the compatibility matrix must read the block the
/// same way, so one parser serves both.
/// What: finds the leading `---` frontmatter block; with no block, or no
/// top-level `requires:` line in it, the asset declares nothing. Otherwise the
/// block is parsed as YAML and `requires` must be a map of string harness
/// names to SemVer requirement strings; a null value declares nothing. Entries
/// keep declaration order. A YAML error elsewhere in a block without
/// `requires:` is not this parser's to report.
///
/// # Errors
/// [`RequiresError`] when `requires:` is present but unreadable.
/// Test: `requires_parses_flow_and_block_forms`,
/// `an_asset_without_requires_declares_none`, `a_malformed_requires_is_an_error`.
pub fn parse_requires(text: &str) -> Result<Vec<Requirement>, RequiresError> {
    let Some(block) = frontmatter(text) else {
        return Ok(Vec::new());
    };
    let prefix = format!("{REQUIRES_KEY}:");
    if !block.lines().any(|line| line.starts_with(&prefix)) {
        return Ok(Vec::new());
    }
    let doc: serde_yaml::Value =
        serde_yaml::from_str(block).map_err(|e| RequiresError::Frontmatter(e.to_string()))?;
    let entries = match doc.get(REQUIRES_KEY) {
        None | Some(serde_yaml::Value::Null) => return Ok(Vec::new()),
        Some(serde_yaml::Value::Mapping(map)) => map,
        Some(_) => return Err(RequiresError::Shape),
    };
    entries
        .iter()
        .map(|(harness, value)| {
            let (Some(harness), Some(value)) = (harness.as_str(), value.as_str()) else {
                return Err(RequiresError::Shape);
            };
            semver::VersionReq::parse(value).map_err(|e| RequiresError::BadRequirement {
                harness: harness.to_owned(),
                value: value.to_owned(),
                reason: e.to_string(),
            })?;
            Ok(Requirement {
                harness: harness.to_owned(),
                requirement: value.to_owned(),
            })
        })
        .collect()
}

/// The text between a leading `---` fence and the next `---` line, or `None`
/// when `text` opens with no terminated block.
fn frontmatter(text: &str) -> Option<&str> {
    let rest = text.trim_start_matches('\u{feff}').strip_prefix("---")?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some(&rest[..offset]);
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
#[path = "requires_tests.rs"]
mod tests;
