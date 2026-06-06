//! Dynamic skill registry with YAML-frontmatter indexing and relevance search.
//!
//! Why: Agents benefit from domain-specific Markdown guidance ("skills"), but
//! forcing every agent to know every skill name at config time is rigid and
//! brittle. A registry that scans `config/skills/*.md`, parses minimal YAML
//! frontmatter (name/description/tags), and ranks skills against a query lets
//! agents discover relevant context at runtime via `list_skills`/`load_skill`
//! tools or via automatic per-task injection in the workflow engine.
//! What: `SkillEntry` is the indexed record for one skill file;
//! `SkillRegistry::load` scans a directory and builds the index; `search`
//! returns the top-N matches for a query; `auto_inject` renders a prompt
//! prefix containing the best matches. All parsing is best-effort: files
//! without frontmatter are indexed with empty tags and the filename as name,
//! unreadable files are skipped with a warn log. The implementation is split
//! across `types` (records + relevance registry), `loader` (`SkillsLoader`
//! prompt assembly), and `llm` (LLM-backed selection); this module re-exports
//! their public surface so callers keep using `skills::*` paths unchanged.
//! Test: See `skills/mod_tests.rs` and the per-submodule tests.

pub mod global_cache;
pub mod index;
pub mod rating;
pub mod registry;
pub mod sources;

pub mod llm;
pub mod loader;
pub mod types;

// Preserve the historical flat `skills::*` public API after the #363 split.
pub use llm::{select_skills_via_llm, skill_llm_enabled};
pub use loader::SkillsLoader;
pub use types::{SkillEntry, SkillRegistry, strip_frontmatter};

#[cfg(test)]
mod mod_tests;
