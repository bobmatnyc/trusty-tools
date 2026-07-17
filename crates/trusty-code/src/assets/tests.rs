//! Tests for the embedded default agents & skills tables.
//!
//! Why: kept out of `mod.rs` so the include-table file stays thin per the
//! module's own "keep it lean" convention; test/benchmark files get the
//! wider 1500-SLOC cap (see repo `CLAUDE.md` SLOC policy) so the fuller
//! assertions here don't threaten `mod.rs`'s production budget.
//! What: parses every embedded agent TOML and checks name consistency;
//! checks every embedded skill name is unique, non-empty, and frontmatter-fenced.
//! Test: this file — self-describing.

use super::*;
use crate::agents::AgentConfig;

/// Every embedded agent TOML parses, and its `[agent].name` matches the
/// table key it is filed under.
///
/// Why: A typo in either the TOML or the table entry would silently break
/// `agents::load_all_agents`'s embedded-fallback at runtime instead of
/// failing fast in CI.
/// Test: this test.
#[test]
fn default_agents_parse_and_names_match() {
    assert_eq!(DEFAULT_AGENTS.len(), 3);
    for agent in DEFAULT_AGENTS {
        let cfg = AgentConfig::from_toml_str(agent.toml)
            .unwrap_or_else(|e| panic!("embedded agent '{}' failed to parse: {e}", agent.name));
        assert_eq!(
            cfg.agent.name, agent.name,
            "table key must match [agent].name"
        );
    }
}

/// Every embedded skill name is unique and non-empty.
///
/// Why: `skills::discover_skill_metadata` sorts and dedupes by name; a
/// duplicate embedded name would silently shadow another skill.
/// Test: this test.
#[test]
fn default_skills_names_are_unique() {
    assert_eq!(DEFAULT_SKILLS.len(), 28);
    let mut names: Vec<&str> = DEFAULT_SKILLS.iter().map(|s| s.name).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), before, "no duplicate embedded skill names");
    for skill in DEFAULT_SKILLS {
        assert!(!skill.name.is_empty());
        assert!(
            skill.skill_md.trim_start().starts_with("---"),
            "embedded skill '{}' must open with a frontmatter fence",
            skill.name
        );
    }
}
