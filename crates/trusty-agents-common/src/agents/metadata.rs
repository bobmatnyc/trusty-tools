//! Read-only projection of a deployed agent's frontmatter for display and
//! diagnostics (DOC-42, issue #2889).
//!
//! Why: `tm doctor`'s dangling-skill check (§SPEC-AGENTSKILLS-03) and the
//! `tm agent list`/`tm agent show` CLI (§SPEC-AGENTSKILLS-04) both need to
//! read an agent file's parsed frontmatter — including the new `skills:`
//! array — WITHOUT re-running full inheritance composition: a deployed file
//! is already flattened (see [`crate::agents::builder::compose_agent`]'s
//! `skills:` union-and-emit), and a source file with no `extends:` parses
//! identically either way. Duplicating [`crate::agents::builder`]'s
//! internal frontmatter grammar here would drift from the parser used at
//! compose/deploy time, so this module reuses
//! [`crate::agents::builder::split_frontmatter`] (`pub(crate)` for exactly
//! this purpose — `metadata` and `builder` are sibling modules in the same
//! crate) and projects its crate-private [`Frontmatter`] into a public,
//! stable-shaped struct external callers may read. Moved to
//! trusty-agents-common (#2892) from `trusty-mpm::core::agent_metadata`
//! alongside `agent_deployer`, which depends on it
//! (`deploy_agents_filtered` populates `DeployResult::declared_skills` via
//! [`agent_metadata_from_str`]); `trusty-mpm` re-exports every item here from
//! `core::agent_metadata` for source compatibility.
//! What: [`AgentMetadata`] plus [`agent_metadata_from_str`] (parse an
//! in-memory document — used by the co-deploy path, which already holds the
//! freshly composed string) and [`read_agent_metadata`] (read a file from
//! disk — used by doctor and the CLI, which look at already-deployed files).
//! Test: `metadata_from_str_reads_skills`, `metadata_from_missing_file_is_default`,
//! `metadata_from_str_malformed_frontmatter_is_default` in this file.

use std::path::Path;

use super::builder::{Frontmatter, split_frontmatter};

/// A deployed agent's parsed frontmatter, projected for display/diagnostics.
///
/// Why: callers outside the compose pipeline (doctor, `tm agent`) need to
/// read an agent's declared metadata — most importantly its `skills:` list —
/// without depending on `agent_builder`'s internal, inheritance-aware
/// [`Frontmatter`] type.
/// What: a plain-old-data mirror of the frontmatter fields trusty-mpm
/// recognizes. `skills` is always populated (empty `Vec` when absent), never
/// `None`, since an agent with no declared dependencies is the common case.
/// Test: `metadata_from_str_reads_skills`, `metadata_from_str_all_fields`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentMetadata {
    /// The `name:` field.
    pub name: Option<String>,
    /// The `role:` field.
    pub role: Option<String>,
    /// The `description:` field.
    pub description: Option<String>,
    /// The `model:` field.
    pub model: Option<String>,
    /// The `extends:` field (present on source files; a fully composed
    /// deployed file never carries it — see `compose_agent`'s doc comment).
    pub extends: Option<String>,
    /// Declared skill dependencies (DOC-42), in declaration order,
    /// de-duplicated. Empty when the agent declares none.
    pub skills: Vec<String>,
}

impl From<Frontmatter> for AgentMetadata {
    fn from(fm: Frontmatter) -> Self {
        Self {
            name: fm.name,
            role: fm.role,
            description: fm.description,
            model: fm.model,
            extends: fm.extends,
            skills: fm.skills,
        }
    }
}

/// Parse an in-memory agent document's frontmatter into [`AgentMetadata`].
///
/// Why: the co-deployment path (`agent_deployer::deploy_agents_filtered`)
/// already holds the freshly composed content string in memory after calling
/// `compose_agent` — re-reading it from disk would be a redundant IO round
/// trip for the exact bytes just written.
/// What: delegates to [`split_frontmatter`]; a parse failure (malformed or
/// unterminated frontmatter) yields [`AgentMetadata::default`] rather than
/// propagating an error — metadata projection is best-effort display/
/// diagnostic data, never a hard dependency.
/// Test: `metadata_from_str_reads_skills`,
/// `metadata_from_str_malformed_frontmatter_is_default`.
pub fn agent_metadata_from_str(raw: &str) -> AgentMetadata {
    match split_frontmatter(raw) {
        Ok((fm, _body)) => AgentMetadata::from(fm),
        Err(_) => AgentMetadata::default(),
    }
}

/// Read an agent file from disk and parse its frontmatter into
/// [`AgentMetadata`].
///
/// Why: doctor's dangling-skill probe and `tm agent list`/`show` both work
/// over already-deployed files on disk (`~/.claude/agents/*.md`), not
/// in-memory compose output.
/// What: reads `path`, then delegates to [`agent_metadata_from_str`]. A
/// missing or unreadable file yields [`AgentMetadata::default`] (empty
/// skills, no fields) rather than an error — callers treat "file absent" and
/// "file present with no metadata" identically for display purposes.
/// Test: `metadata_from_missing_file_is_default`, `read_metadata_from_disk`.
pub fn read_agent_metadata(path: &Path) -> AgentMetadata {
    match std::fs::read_to_string(path) {
        Ok(raw) => agent_metadata_from_str(&raw),
        Err(_) => AgentMetadata::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn metadata_from_str_reads_skills() {
        let doc = "---\nname: code-critic\nrole: qa\nskills: [code-review-standards, systematic-debugging]\n---\n\nBody.\n";
        let meta = agent_metadata_from_str(doc);
        assert_eq!(meta.name.as_deref(), Some("code-critic"));
        assert_eq!(meta.role.as_deref(), Some("qa"));
        assert_eq!(
            meta.skills,
            vec!["code-review-standards", "systematic-debugging"]
        );
    }

    #[test]
    fn metadata_from_str_all_fields() {
        let doc = "---\nname: rust-engineer\nrole: engineer\ndescription: Rust specialist\nmodel: sonnet\nextends: base-engineer\nskills: [toolchains-rust-core]\n---\n\nBody.\n";
        let meta = agent_metadata_from_str(doc);
        assert_eq!(meta.name.as_deref(), Some("rust-engineer"));
        assert_eq!(meta.description.as_deref(), Some("Rust specialist"));
        assert_eq!(meta.model.as_deref(), Some("sonnet"));
        assert_eq!(meta.extends.as_deref(), Some("base-engineer"));
        assert_eq!(meta.skills, vec!["toolchains-rust-core"]);
    }

    #[test]
    fn metadata_from_str_no_skills_is_empty_vec() {
        let doc = "---\nname: plain\nrole: engineer\n---\n\nBody.\n";
        let meta = agent_metadata_from_str(doc);
        assert!(meta.skills.is_empty());
    }

    #[test]
    fn metadata_from_str_malformed_frontmatter_is_default() {
        // Unterminated frontmatter (`split_frontmatter` errors) must degrade
        // to an empty default, never panic or propagate.
        let doc = "---\nname: broken\n\nno closing fence\n";
        let meta = agent_metadata_from_str(doc);
        assert_eq!(meta, AgentMetadata::default());
    }

    #[test]
    fn metadata_from_missing_file_is_default() {
        let meta = read_agent_metadata(Path::new("/nonexistent/agent.md"));
        assert_eq!(meta, AgentMetadata::default());
    }

    #[test]
    fn read_metadata_from_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("critic.md");
        std::fs::write(
            &path,
            "---\nname: critic\nrole: qa\nskills: [code-review-standards]\n---\n\nBody.\n",
        )
        .unwrap();
        let meta = read_agent_metadata(&path);
        assert_eq!(meta.name.as_deref(), Some("critic"));
        assert_eq!(meta.skills, vec!["code-review-standards"]);
    }
}
