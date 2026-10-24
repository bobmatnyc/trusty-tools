//! The Architect seed files, read from instructional content at runtime (#9132).
//!
//! Why: ADR-0064 compiles no skill into a binary. #9132 moved the two
//! Architect-only skills and the Architect `CLAUDE.md` template out of
//! `include_str!` into `content/instructions/architect/`. They sit under
//! `instructions/`, not `skills/`: every `skills/` file deploys to every PM's
//! tier, and these must reach the Architect only (df-D21, df-D22).
//! #10161 D3c adds the Architect agents (`architect/agents/<name>.md`).
//! What: [`ArchitectSeed::load`] reads the template, the skills and the agents
//! from one loaded [`FrameworkContent`] and fails closed. An absent file is
//! [`ArchitectSeedError::Missing`], a blank one [`ArchitectSeedError::Empty`],
//! and a source that cannot be resolved or loaded is
//! [`ArchitectSeedError::Content`]. It returns every file or an error, so a
//! caller that loads before it writes never writes a partial seed.
//! Test: `the_repository_content_seeds_the_architect`,
//! `a_source_without_the_seed_files_names_the_first_missing_one`,
//! `a_source_missing_one_skill_loads_nothing`, `a_blank_seed_file_is_refused`,
//! `an_unresolvable_source_is_a_content_error`,
//! `a_missing_or_blank_agent_source_loads_nothing`.

use std::path::Path;

use super::architect_agents::{SHIPPED_AGENTS, ShippedAgent};
use super::architect_skills::{ArchitectSkill, SHIPPED, ShippedSkill};
use crate::core::content_source::{self, AgentContentError, Fetch, FrameworkContent};
use crate::core::framework_content::INSTRUCTIONS_CLASS;

/// The Architect `CLAUDE.md` template, relative to `instructions/`.
pub const CLAUDE_MD_TEMPLATE: &str = "architect/templates/CLAUDE.md";

/// Why the Architect seed could not be read. Every arm writes nothing.
#[derive(Debug, thiserror::Error)]
pub enum ArchitectSeedError {
    /// The content source lacks a seed file.
    #[error(
        "the content from {origin} has no `{path}`, an Architect seed file; \
         run `tm content update` for a content release that carries it"
    )]
    Missing {
        /// Where the content came from.
        origin: String,
        /// The bundle path of the absent file.
        path: String,
    },
    /// A seed file is present but blank; it is never seeded blank.
    #[error("the Architect seed file `{path}` from {origin} is empty")]
    Empty {
        /// Where the content came from.
        origin: String,
        /// The bundle path of the blank file.
        path: String,
    },
    /// No content source resolved, or it did not load.
    #[error("the Architect seed files cannot be read: {0}")]
    Content(#[from] AgentContentError),
}

/// The Architect seed: the `CLAUDE.md` template, the shipped skills and the
/// shipped agents.
#[derive(Debug, Clone)]
pub struct ArchitectSeed {
    /// The `CLAUDE.md` template text.
    pub claude_md: String,
    /// One entry per [`SHIPPED`] skill, in that order.
    pub skills: Vec<ArchitectSkill>,
    /// One entry per [`SHIPPED_AGENTS`] agent, in that order; `stem` is the
    /// agent's name (#10161 D3c).
    pub agents: Vec<ArchitectSkill>,
}

impl ArchitectSeed {
    /// Reads every seed file from `content`, or none.
    ///
    /// # Errors
    /// [`ArchitectSeedError::Missing`] or [`ArchitectSeedError::Empty`] naming
    /// the first file that fails: `CLAUDE.md`, then the skills, then the agents.
    pub fn load(content: &FrameworkContent) -> Result<Self, ArchitectSeedError> {
        Self::load_from(content, SHIPPED)
    }

    /// [`Self::load`] with the skill table named; #9997 AC5 tests a table
    /// with one more entry through it.
    ///
    /// # Errors
    /// As [`Self::load`].
    pub fn load_from(
        content: &FrameworkContent,
        shipped: &[ShippedSkill],
    ) -> Result<Self, ArchitectSeedError> {
        Ok(Self {
            claude_md: seed_file(content, CLAUDE_MD_TEMPLATE)?.to_owned(),
            skills: load_skills_from(content, shipped)?,
            agents: load_agents_from(content, SHIPPED_AGENTS)?,
        })
    }

    /// [`Self::load`] from the content for work on `dir` (see
    /// [`content_source::resolve_for`]).
    ///
    /// # Errors
    /// As [`Self::resolve_in`].
    pub fn resolve_for(dir: &Path, fetch: Fetch) -> Result<Self, ArchitectSeedError> {
        let cwd = std::env::current_dir().ok();
        let cache = trusty_common::content::default_cache_dir();
        Self::resolve_in(dir, cwd.as_deref(), cache.as_deref(), fetch)
    }

    /// [`Self::resolve_for`] with the cwd and the cache named.
    ///
    /// # Errors
    /// [`ArchitectSeedError::Content`] when no source resolves or it does not
    /// load; otherwise as [`Self::load`].
    pub fn resolve_in(
        dir: &Path,
        cwd: Option<&Path>,
        cache: Option<&Path>,
        fetch: Fetch,
    ) -> Result<Self, ArchitectSeedError> {
        let resolved = content_source::resolve_for_in(Some(dir), cwd, cache, fetch)?;
        Self::load(&FrameworkContent::load(&resolved)?)
    }
}

/// The [`SHIPPED`] skills with their text from `content`, or the first error.
///
/// # Errors
/// As [`ArchitectSeed::load`].
pub fn load_skills(content: &FrameworkContent) -> Result<Vec<ArchitectSkill>, ArchitectSeedError> {
    load_skills_from(content, SHIPPED)
}

/// `shipped`'s skills with their text from `content`, in table order, or the
/// first error.
///
/// Why: #9997 AC5 — the loader iterates whatever table it is given, so a new
/// skill needs no code beyond its [`SHIPPED`] entry.
/// What: reads [`ShippedSkill::source`] for each entry, failing closed as
/// [`ArchitectSeed::load`] does.
/// Test: `one_more_shipped_entry_and_content_file_load_seed_and_deploy`.
///
/// # Errors
/// As [`ArchitectSeed::load`].
pub fn load_skills_from(
    content: &FrameworkContent,
    shipped: &[ShippedSkill],
) -> Result<Vec<ArchitectSkill>, ArchitectSeedError> {
    shipped
        .iter()
        .map(|shipped| {
            Ok(ArchitectSkill {
                stem: shipped.stem,
                contents: seed_file(content, &shipped.source())?.to_owned(),
                previous: shipped.previous,
            })
        })
        .collect()
}

/// `shipped`'s agents with their text from `content`, in table order, or the
/// first error; a missing or blank agent source fails like a skill's (#10161 D3c).
///
/// # Errors
/// As [`ArchitectSeed::load`].
pub fn load_agents_from(
    content: &FrameworkContent,
    shipped: &[ShippedAgent],
) -> Result<Vec<ArchitectSkill>, ArchitectSeedError> {
    shipped
        .iter()
        .map(|agent| {
            Ok(ArchitectSkill {
                stem: agent.name,
                contents: seed_file(content, &agent.source())?.to_owned(),
                previous: agent.previous,
            })
        })
        .collect()
}

/// The shipped skills from the local content for `dir`, never fetching: the
/// launch already resolved it, and `tm fleet status` is read-only.
///
/// # Errors
/// As [`ArchitectSeed::resolve_in`].
pub fn local_skills(dir: &Path) -> Result<Vec<ArchitectSkill>, ArchitectSeedError> {
    load_skills(&content_source::framework_content_local(Some(dir))?)
}

/// The non-blank text of the instruction at `rel`.
fn seed_file<'a>(content: &'a FrameworkContent, rel: &str) -> Result<&'a str, ArchitectSeedError> {
    let path = || format!("{INSTRUCTIONS_CLASS}/{rel}");
    let body = content
        .instruction(rel)
        .ok_or_else(|| ArchitectSeedError::Missing {
            origin: content.origin(),
            path: path(),
        })?;
    if body.trim().is_empty() {
        return Err(ArchitectSeedError::Empty {
            origin: content.origin(),
            path: path(),
        });
    }
    Ok(body)
}

/// The checkout's shipped Architect skills, for tests across the crate.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{ArchitectSkill, load_skills};

    /// Every shipped skill, read from the checkout's content.
    pub(crate) fn shipped() -> Vec<ArchitectSkill> {
        load_skills(crate::core::content_source::test_support::rc())
            .expect("the checkout carries the Architect skills")
    }

    /// The shipped text of `stem`.
    pub(crate) fn shipped_text(stem: &str) -> String {
        shipped()
            .into_iter()
            .find(|skill| skill.stem == stem)
            .map(|skill| skill.contents)
            .unwrap_or_else(|| panic!("{stem} is not shipped"))
    }
}

#[cfg(test)]
#[path = "architect_seed_tests.rs"]
mod tests;
