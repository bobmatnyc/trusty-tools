//! trusty-mpm's own instructional content, read at runtime (#9012).
//!
//! Why: ADR-0064 makes skills, PM instruction sections, the instruction
//! package, the supervisor sections, output styles, the SM instructions and the
//! bundled docs runtime-only content. #9012 moved them out of the crate into
//! `content/` and dropped every `include_str!` of them, so each consumer reads
//! them from one loaded value instead of a compiled-in constant.
//! What: [`FrameworkContent::load`] reads every `skills/**` and
//! `instructions/**` file of one resolved source into memory and fails loud,
//! naming `tm content update`, when a file every process needs
//! ([`REQUIRED_INSTRUCTIONS`]) or the whole skills class is absent. Accessors
//! borrow from the loaded value; nothing here is global or cached.
//! Test: `the_repository_content_loads`, `a_source_without_skills_is_an_error`,
//! `a_source_missing_a_required_instruction_is_an_error`.

use std::collections::BTreeMap;

use trusty_agents_common::agent_content::describe_source;
pub use trusty_agents_common::agent_content::{AgentContentError, ContentSource, ResolvedContent};

/// The bundle class holding skills.
pub const SKILLS_CLASS: &str = "skills";

/// The bundle class holding instructions, output styles and SM instructions.
pub const INSTRUCTIONS_CLASS: &str = "instructions";

/// Instruction files (relative to `instructions/`) every process needs.
///
/// Why: a launch, a doctor run and an SM turn each read several of these; one
/// check at load turns a partial bundle into one named error instead of a
/// prompt quietly missing a section.
/// What: the 19 PM sections the package names, the self-improvement section,
/// the package and its schema, the eight supervisor sections, the four output
/// styles and the four SM instruction files. The bundled docs are not here:
/// only `tm install` reads them, and it requires them itself.
/// Test: `the_repository_content_loads`,
/// `a_source_missing_a_required_instruction_is_an_error`.
pub const REQUIRED_INSTRUCTIONS: &[&str] = &[
    "pm-instruction-package.json",
    "instruction-package.schema.json",
    "sections/identity.md",
    "sections/core.md",
    "sections/pm-allowlist.md",
    "sections/delegation-mechanics.md",
    "sections/agent-routing.md",
    "sections/subagent-re-engagement.md",
    "sections/phases.md",
    "sections/qa-gate.md",
    "sections/git-file-tracking.md",
    "sections/tickets-prs-releases.md",
    "sections/messages-reports-sessions.md",
    "sections/autonomous-execution.md",
    "sections/memory.md",
    "sections/search.md",
    "sections/workflow.md",
    "sections/agent-delegation.md",
    "sections/enforcement.md",
    "sections/non-overridable-rules.md",
    "sections/framework-guaranteed-conventions.md",
    "sections/prompt-self-improvement.md",
    "supervisor/identity.md",
    "supervisor/hard-limits.md",
    "supervisor/relay-protocol.md",
    "supervisor/evidence-labels.md",
    "supervisor/decisions.md",
    "supervisor/monitoring.md",
    "supervisor/tool-priority.md",
    "supervisor/prose-style.md",
    "output-styles/trusty-mpm.md",
    "output-styles/trusty-mpm-teacher.md",
    "output-styles/trusty-mpm-research.md",
    "output-styles/trusty-mpm-supervisor.md",
    "sm_instructions/SM_INSTRUCTIONS.md",
    "sm_instructions/SM_WORKFLOW.md",
    "sm_instructions/SM_TOOLS.md",
    "sm_instructions/BASE_SM.md",
];

/// The skills and instructions of one resolved content source.
///
/// Why: replaces the compiled-in skill table, section constants, package,
/// output styles and SM instructions with the same text, read from content.
/// What: skills keyed by bundle path (`skills/tm.md`), sorted; instructions
/// keyed by path relative to `instructions/` (`sections/core.md`). A loaded
/// value always has at least one skill and every [`REQUIRED_INSTRUCTIONS`]
/// file.
/// Test: `the_repository_content_loads`.
#[derive(Debug, Clone)]
pub struct FrameworkContent {
    source: ContentSource,
    skills: Vec<(String, String)>,
    instructions: BTreeMap<String, String>,
}

impl FrameworkContent {
    /// Reads every `skills/**` and `instructions/**` file of `content`.
    ///
    /// # Errors
    /// [`AgentContentError::Missing`] when the source has no skill or lacks a
    /// [`REQUIRED_INSTRUCTIONS`] file; [`AgentContentError::Content`] when a
    /// listed file cannot be read.
    pub fn load(content: &ResolvedContent) -> Result<Self, AgentContentError> {
        let origin = describe_source(content.source());
        let mut skills = Vec::new();
        for key in content.list(SKILLS_CLASS)? {
            let body = content.read_to_string(&key)?;
            skills.push((key, body));
        }
        if skills.is_empty() {
            return Err(AgentContentError::Missing {
                origin,
                path: format!("{SKILLS_CLASS}/"),
            });
        }
        let prefix = format!("{INSTRUCTIONS_CLASS}/");
        let mut instructions = BTreeMap::new();
        for key in content.list(INSTRUCTIONS_CLASS)? {
            let body = content.read_to_string(&key)?;
            if let Some(rel) = key.strip_prefix(&prefix) {
                instructions.insert(rel.to_string(), body);
            }
        }
        if let Some(missing) = REQUIRED_INSTRUCTIONS
            .iter()
            .find(|rel| !instructions.contains_key(**rel))
        {
            return Err(AgentContentError::Missing {
                origin,
                path: format!("{prefix}{missing}"),
            });
        }
        Ok(Self {
            source: content.source().clone(),
            skills,
            instructions,
        })
    }

    /// Where the content came from.
    pub fn source(&self) -> &ContentSource {
        &self.source
    }

    /// The source named for an error message (`dev checkout <root>`, a tag).
    pub fn origin(&self) -> String {
        describe_source(&self.source)
    }

    /// Every skill file as `(bundle path, body)`, sorted by bundle path.
    pub fn skills(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.skills
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_str()))
    }

    /// The body of skill file `path` (`skills/tm.md`), if present.
    pub fn skill(&self, path: &str) -> Option<&str> {
        self.skills
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, body)| body.as_str())
    }

    /// The instruction file at `rel` (`sections/core.md`), if present.
    pub fn instruction(&self, rel: &str) -> Option<&str> {
        self.instructions.get(rel).map(String::as_str)
    }

    /// Every instruction path (relative to `instructions/`) under `prefix`
    /// (`sections/`), sorted.
    pub fn instruction_paths<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.instructions
            .keys()
            .filter(move |k| k.starts_with(prefix))
            .map(String::as_str)
    }

    /// The instruction file at `rel`, or [`AgentContentError::Missing`].
    pub fn require_instruction(&self, rel: &str) -> Result<&str, AgentContentError> {
        self.instruction(rel)
            .ok_or_else(|| AgentContentError::Missing {
                origin: self.origin(),
                path: format!("{INSTRUCTIONS_CLASS}/{rel}"),
            })
    }

    /// A [`REQUIRED_INSTRUCTIONS`] file. [`FrameworkContent::load`] refused a
    /// source without it, so this is total for every listed path.
    ///
    /// # Panics
    /// In debug builds, when `rel` is not in [`REQUIRED_INSTRUCTIONS`] — a
    /// programming error the test suite reaches on every call site.
    pub fn required(&self, rel: &'static str) -> &str {
        debug_assert!(
            REQUIRED_INSTRUCTIONS.contains(&rel),
            "`{rel}` is not a required instruction"
        );
        self.instruction(rel).unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "framework_content_tests.rs"]
mod tests;
