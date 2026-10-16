//! trusty-code's own instructional content, read at runtime (ADR-0064, #9013).
//!
//! Why: ADR-0064 makes agents and skills runtime-only content. A published
//! crate cannot `include_str!` a file outside its own directory, and a content
//! fix must not need a crate release. So tcode's own agents and skill catalog
//! live in the root `content/` tree and are read through the same resolver
//! trusty-mpm uses: a trusty-tools checkout above the cwd, else the
//! sha256-pinned bundle `tm content update` installed.
//! What: [`resolve_for_process`] picks the source. [`catalog_from`] builds the
//! tm agent catalog (shared roster plus tcode's 4 forks), [`AgentSources`]
//! adds tcode's 8 `Direct` agents, and [`default_skills_from`] /
//! [`default_skill_from`] read the default skill catalog. Every absent or
//! empty file is an [`AgentContentError`] naming the remedy; nothing here
//! answers with a partial set.
//! Test: `assets::content::tests::absent_own_agent_fails_closed_naming_the_file`,
//! `assets::content::tests::absent_default_skill_fails_closed_naming_the_file`,
//! `no_content_run_task_fails_closed_naming_the_remedy` (`tests/no_content_e2e.rs`).
//!
//! # Spec References
//! - ADR-0064 decision 5: `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use trusty_agents_common::agent_content::{
    AgentContentError, AgentRoster, ContentError, DevOverride, ResolvedContent, describe_source,
    resolve_content,
};

use super::{
    DEFAULT_AGENTS, DEFAULT_SKILL_NAMES, EmbeddedAgent, LOCAL_TM_AGENT_FORKS, SHARED_TM_AGENT_FILES,
};

/// Bundle directory of tcode's own agents: the 8 `Direct` defaults and the
/// 4 read-only forks. Nested under `agents/`, so the shared roster
/// (`AgentRoster`, top-level files only) never lists them.
pub const TCODE_AGENTS_DIR: &str = "agents/trusty-code";

/// Bundle directory of tcode's default skill catalog, one `<name>/SKILL.md`
/// each. Under `instructions/`, beside the Architect's own skills, so
/// trusty-mpm's `skills/` deploy never ships them.
pub const TCODE_SKILLS_DIR: &str = "instructions/trusty-code/skills";

/// Resolves instructional content for this process.
///
/// What: the trusty-tools checkout above the cwd (ADR-0064 dev override),
/// else the installed bundle; never fetches.
pub fn resolve_for_process() -> Result<ResolvedContent, AgentContentError> {
    let dev = std::env::current_dir().map_or(DevOverride::Off, DevOverride::DetectFrom);
    resolve_content(dev)
}

/// Reads the file at bundle path `path`, which must exist and hold text.
///
/// Why: an absent or empty default must fail the load, naming the file, so a
/// content release that lacks it is never served as "no such agent".
/// What: `Missing` for an absent file, `Invalid` for an empty one, the
/// resolver's own error otherwise.
/// Test: `assets::content::tests::absent_own_agent_fails_closed_naming_the_file`.
pub fn require_file(content: &ResolvedContent, path: &str) -> Result<String, AgentContentError> {
    let origin = || describe_source(content.source());
    match content.read_to_string(path) {
        Ok(body) if body.trim().is_empty() => Err(AgentContentError::Invalid {
            origin: origin(),
            path: path.to_owned(),
            reason: "the file is empty".to_owned(),
        }),
        Ok(body) => Ok(body),
        Err(ContentError::NotFound { .. }) => Err(AgentContentError::Missing {
            origin: origin(),
            path: path.to_owned(),
        }),
        Err(e) => Err(e.into()),
    }
}

/// The bundle path of tcode's own agent file `file` (`pm.md`).
pub fn own_agent_path(file: &str) -> String {
    format!("{TCODE_AGENTS_DIR}/{file}")
}

/// The tm agent catalog the extends-composer resolves against: every
/// [`SHARED_TM_AGENT_FILES`] entry from the shared roster, then the
/// [`LOCAL_TM_AGENT_FORKS`] from [`TCODE_AGENTS_DIR`] (31 entries).
///
/// Why: `agents::md_loader` and the roster deploy need one batch of
/// `(file name, markdown)` pairs.
/// What: any absent file is an error, so a partial catalog never composes.
/// Test: `assets::tests::embedded_tm_agent_sources_has_31_entries_and_unique_keys`.
pub fn catalog_from(content: &ResolvedContent) -> Result<Vec<(String, String)>, AgentContentError> {
    let roster = AgentRoster::load(content)?;
    let mut out = Vec::with_capacity(SHARED_TM_AGENT_FILES.len() + LOCAL_TM_AGENT_FORKS.len());
    for file in SHARED_TM_AGENT_FILES {
        out.push(((*file).to_owned(), roster.require(file)?.to_owned()));
    }
    for file in LOCAL_TM_AGENT_FORKS {
        out.push((
            (*file).to_owned(),
            require_file(content, &own_agent_path(file))?,
        ));
    }
    Ok(out)
}

/// The `.md` of the [`EmbeddedAgent::Direct`] agent `name`, or `Ok(None)` when
/// `name` is not one.
///
/// Why: a single-agent resolution reads one file, not the whole roster.
/// Test: `assets::content::tests::absent_own_agent_fails_closed_naming_the_file`.
pub fn direct_agent_from(
    content: &ResolvedContent,
    name: &str,
) -> Result<Option<String>, AgentContentError> {
    if !is_direct(name) {
        return Ok(None);
    }
    require_file(content, &own_agent_path(&format!("{name}.md"))).map(Some)
}

/// [`direct_agent_from`] over the content resolved for this process.
pub fn load_direct_agent(name: &str) -> Result<Option<String>, AgentContentError> {
    if !is_direct(name) {
        return Ok(None);
    }
    direct_agent_from(&resolve_for_process()?, name)
}

/// Whether `name` is an [`EmbeddedAgent::Direct`] entry of [`DEFAULT_AGENTS`].
pub fn is_direct(name: &str) -> bool {
    DEFAULT_AGENTS
        .iter()
        .any(|a| matches!(a, EmbeddedAgent::Direct { name: n } if *n == name))
}

/// Every text the built-in roster needs, read from one resolved source.
///
/// Why: a batch caller (the fallback loader, the roster deploy) resolves
/// content once and fails as a whole when any file is absent.
/// What: the [`catalog_from`] catalog plus one body per
/// [`EmbeddedAgent::Direct`], in [`DEFAULT_AGENTS`] order.
/// Test: `assets::tests::default_agents_parse_and_names_match`.
#[derive(Debug, Clone)]
pub struct AgentSources {
    catalog: Vec<(String, String)>,
    direct: Vec<(&'static str, String)>,
}

impl AgentSources {
    /// Reads every roster text from `content`.
    ///
    /// # Errors
    /// The first absent, empty or unreadable file, as an [`AgentContentError`].
    pub fn from_content(content: &ResolvedContent) -> Result<Self, AgentContentError> {
        let catalog = catalog_from(content)?;
        let mut direct = Vec::new();
        for agent in DEFAULT_AGENTS {
            if let EmbeddedAgent::Direct { name } = agent {
                let md = require_file(content, &own_agent_path(&format!("{name}.md")))?;
                direct.push((*name, md));
            }
        }
        Ok(Self { catalog, direct })
    }

    /// [`AgentSources::from_content`] over [`resolve_for_process`].
    pub fn load() -> Result<Self, AgentContentError> {
        Self::from_content(&resolve_for_process()?)
    }

    /// The tm agent catalog (see [`catalog_from`]).
    pub fn catalog(&self) -> &[(String, String)] {
        &self.catalog
    }

    /// The `.md` of the `Direct` agent `name`, if it is one.
    pub fn direct(&self, name: &str) -> Option<&str> {
        self.direct
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, md)| md.as_str())
    }

    /// Every `(name, md)` `Direct` pair, in [`DEFAULT_AGENTS`] order.
    pub fn directs(&self) -> impl Iterator<Item = (&'static str, &str)> + '_ {
        self.direct.iter().map(|(n, md)| (*n, md.as_str()))
    }
}

/// One default skill: its catalog name and raw `SKILL.md` source.
///
/// What: `name` is a [`DEFAULT_SKILL_NAMES`] entry; `skill_md` is the file
/// text (frontmatter fence + Markdown body).
#[derive(Debug, Clone)]
pub struct DefaultSkill {
    /// Catalog name, matching the frontmatter `name:` inside `skill_md`.
    pub name: &'static str,
    /// Raw `SKILL.md` source.
    pub skill_md: String,
}

/// The bundle path of default skill `name`'s `SKILL.md`.
pub fn skill_path(name: &str) -> String {
    format!("{TCODE_SKILLS_DIR}/{name}/SKILL.md")
}

/// Every [`DEFAULT_SKILL_NAMES`] skill, read from `content`.
///
/// # Errors
/// The first absent, empty or unreadable `SKILL.md`.
/// Test: `assets::tests::default_skills_names_are_unique`,
/// `assets::content::tests::absent_default_skill_fails_closed_naming_the_file`.
pub fn default_skills_from(
    content: &ResolvedContent,
) -> Result<Vec<DefaultSkill>, AgentContentError> {
    DEFAULT_SKILL_NAMES
        .iter()
        .map(|name| {
            Ok(DefaultSkill {
                name,
                skill_md: require_file(content, &skill_path(name))?,
            })
        })
        .collect()
}

/// [`default_skills_from`] over [`resolve_for_process`].
pub fn load_default_skills() -> Result<Vec<DefaultSkill>, AgentContentError> {
    default_skills_from(&resolve_for_process()?)
}

/// The `SKILL.md` of default skill `name`, or `Ok(None)` when `name` is not a
/// default skill. Resolves content only for a default name.
pub fn default_skill_from(
    content: &ResolvedContent,
    name: &str,
) -> Result<Option<String>, AgentContentError> {
    if !DEFAULT_SKILL_NAMES.contains(&name) {
        return Ok(None);
    }
    require_file(content, &skill_path(name)).map(Some)
}

/// [`default_skill_from`] over [`resolve_for_process`].
pub fn load_default_skill(name: &str) -> Result<Option<String>, AgentContentError> {
    if !DEFAULT_SKILL_NAMES.contains(&name) {
        return Ok(None);
    }
    default_skill_from(&resolve_for_process()?, name)
}

#[cfg(test)]
#[path = "content_tests.rs"]
mod tests;
