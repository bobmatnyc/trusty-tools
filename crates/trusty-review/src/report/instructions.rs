//! Analyst instructions document loader (#2340 / wave 2).
//!
//! Why: beyond the template, an analyst hands the report generator a free-form
//! markdown brief — the specific focus areas, deal concerns, and questions the
//! review must address.  Two paths consume it: deterministically it is recorded
//! verbatim in the report as an "Analyst Instructions" section (every report
//! documents what it was asked to look for), and under `--synthesize` it is
//! injected into the synthesis prompt as focus directives that steer emphasis.
//! It never relaxes a guardrail: instructions steer emphasis, never authorize
//! invention.
//! What: [`Instructions`] holds the verbatim brief and its source path;
//! [`load_instructions`] reads the file, erroring on a missing path (a typo must
//! fail loudly) and warning-then-ignoring an empty file (proceed as if absent).
//! Test: `instructions_tests.rs` covers load, the missing-file error, and the
//! empty-file warn-and-ignore path.

use std::path::{Path, PathBuf};

use tracing::warn;

use super::error::ReportError;

/// A loaded analyst instructions brief.
///
/// Why: both the deterministic "Analyst Instructions" section and the synthesis
/// focus-directive injection read the same verbatim text; carrying the source
/// path lets the report record provenance (declared, from this file).
/// What: `text` is the verbatim markdown (trailing whitespace trimmed); `source`
/// is the path it was loaded from.
/// Test: `instructions_tests.rs::load_reads_verbatim`.
#[derive(Debug, Clone)]
pub struct Instructions {
    /// The verbatim instructions markdown.
    pub text: String,
    /// The path the instructions were loaded from.
    pub source: PathBuf,
}

/// Load an analyst instructions markdown file.
///
/// Why: the single entry point the CLI uses once it has resolved the instructions
/// path (CLI flag wins over the manifest key).  A missing file is a user error
/// (a mistyped path) and must fail loudly; an empty file is tolerated so an
/// analyst can stub the brief without breaking the run.
/// What: reads `path`; a read failure maps to [`ReportError::InstructionsNotFound`];
/// content that is empty after trimming logs a `warn!` and returns `Ok(None)`
/// (proceed as if absent); otherwise returns `Ok(Some(Instructions))` with the
/// trimmed text.
/// Test: `instructions_tests.rs::{load_reads_verbatim, missing_file_errors,
/// empty_file_is_ignored}`.
pub fn load_instructions(path: &Path) -> Result<Option<Instructions>, ReportError> {
    let raw =
        std::fs::read_to_string(path).map_err(|source| ReportError::InstructionsNotFound {
            path: path.to_path_buf(),
            source,
        })?;
    let text = raw.trim();
    if text.is_empty() {
        warn!(
            path = %path.display(),
            "analyst instructions file is empty; proceeding as if no instructions were given"
        );
        return Ok(None);
    }
    Ok(Some(Instructions {
        text: text.to_string(),
        source: path.to_path_buf(),
    }))
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
