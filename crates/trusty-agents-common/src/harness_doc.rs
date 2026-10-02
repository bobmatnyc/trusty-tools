//! # Spec References
//!
//! - [`SPEC-HARNESS-UNDERSTANDING-01~draft`](docs/specs/harness-understanding.md)
//!
//! Canonical harness-understanding instructions shared between trusty-mpm SM and
//! future t-code overseer (DOC-21).
//!
//! Why: The harness mental model, per-harness signals, and intervention decision
//!      protocol must live in one place so both the SM prompt (trusty-mpm) and a
//!      future t-code overseer consume the same authoritative content. Since
//!      #9011 it is instructional content (ADR-0064), read at runtime from the
//!      same resolved source as the agent roster, never compiled in.
//! What: [`HarnessDoc::load`] reads the four files from one
//!       [`ResolvedContent`]; four accessors plus [`HarnessDoc::harness_understanding`],
//!       which joins all four for consumers that want the full doc.
//! Test: `harness_doc_names_the_relay_prefix`, `full_doc_sum_of_parts`,
//!       `the_legacy_bundle_key_still_resolves`,
//!       `a_missing_harness_doc_is_an_error` (`harness_doc_tests.rs`).

use crate::agent_content::{AgentContentError, ContentError, ResolvedContent, describe_source};

/// Bundle directories searched in order: the post-#8378 nested key, then the
/// `harness_understanding` class a content-v0.1.0 bundle carries.
pub const HARNESS_DOC_DIRS: [&str; 2] = [
    "instructions/harness_understanding",
    "harness_understanding",
];

/// The four section files, in assembly order.
const SECTION_FILES: [&str; 4] = [
    "HARNESS_AGNOSTIC.md",
    "HARNESS_MPM_SM.md",
    "HARNESS_TCODE.md",
    "HARNESS_OVERSEER.md",
];

/// The four harness-understanding sections of one content source.
///
/// Why: the free functions this replaces returned compiled-in `&'static str`;
/// content is runtime-only now, so the doc is a loaded value and a missing
/// file is an error, never an empty section.
/// What: each section's full text; see the accessors for what each covers.
/// Test: `harness_doc_names_the_relay_prefix`, `full_doc_sum_of_parts`,
/// `a_missing_harness_doc_is_an_error`.
#[derive(Debug, Clone)]
pub struct HarnessDoc {
    agnostic: String,
    mpm_session_manager: String,
    tcode: String,
    overseer: String,
}

impl HarnessDoc {
    /// Reads the four files from `content`. For each file the first directory
    /// of [`HARNESS_DOC_DIRS`] holding it wins; `NotFound` moves on to the
    /// next directory, any other error is returned, and a file in neither
    /// directory is [`AgentContentError::Missing`]. Both directories are read
    /// from this one source; there is no fallback to another source.
    ///
    /// Test: `a_missing_harness_doc_is_an_error`,
    /// `the_legacy_bundle_key_still_resolves`.
    pub fn load(content: &ResolvedContent) -> Result<Self, AgentContentError> {
        let [agnostic, mpm_session_manager, tcode, overseer] =
            SECTION_FILES.map(|file| read_section(content, file));
        Ok(Self {
            agnostic: agnostic?,
            mpm_session_manager: mpm_session_manager?,
            tcode: tcode?,
            overseer: overseer?,
        })
    }

    /// Harness-agnostic mental model: session lifecycle, pane/IO model, prompt
    /// shapes, completion/error signals, and the WHEN-TO-INTERVENE protocol
    /// (`HARNESS_AGNOSTIC.md`).
    pub fn agnostic(&self) -> &str {
        &self.agnostic
    }

    /// trusty-mpm session-manager specifics: OBSERVE/VERIFY wiring, the
    /// RawObservation/Summary two-tier model, the override convention
    /// (`HARNESS_MPM_SM.md`).
    pub fn mpm_session_manager(&self) -> &str {
        &self.mpm_session_manager
    }

    /// tcode-specific signals: task banners, `__OMPM_EVENT__` NDJSON lines,
    /// delegation patterns (`HARNESS_TCODE.md`).
    pub fn tcode(&self) -> &str {
        &self.tcode
    }

    /// The t-code-as-overseer contract: the `Overseer` trait, the
    /// `HarnessSource::Code` seam, event-to-decision mapping
    /// (`HARNESS_OVERSEER.md`).
    pub fn overseer(&self) -> &str {
        &self.overseer
    }

    /// All four sections, trimmed and joined with a plain `"\n\n"`.
    ///
    /// Why: SM prompt assembly joins top-level sections with
    /// `"\n\n---\n\n"`; using that separator here would split the harness
    /// block into spurious top-level sections. Each section opens with its own
    /// heading, so a blank line is enough.
    /// Test: `full_doc_contains_all_markers`, `full_doc_sum_of_parts`.
    pub fn harness_understanding(&self) -> String {
        [
            self.agnostic(),
            self.mpm_session_manager(),
            self.tcode(),
            self.overseer(),
        ]
        .iter()
        .map(|s| s.trim())
        .collect::<Vec<_>>()
        .join("\n\n")
    }
}

/// Reads one section file from the first [`HARNESS_DOC_DIRS`] entry holding it.
fn read_section(content: &ResolvedContent, file: &str) -> Result<String, AgentContentError> {
    for dir in HARNESS_DOC_DIRS {
        match content.read_to_string(&format!("{dir}/{file}")) {
            Ok(text) => return Ok(text),
            Err(ContentError::NotFound { .. }) => continue,
            Err(other) => return Err(other.into()),
        }
    }
    Err(AgentContentError::Missing {
        origin: describe_source(content.source()),
        path: format!("{}/{file}", HARNESS_DOC_DIRS[0]),
    })
}

#[cfg(test)]
#[path = "harness_doc_tests.rs"]
mod tests;
