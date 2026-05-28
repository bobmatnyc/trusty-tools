//! The `load_skill` tool.
//!
//! Why: Gives LLM agents a direct path to domain knowledge without baking
//! that knowledge into every system prompt. When a registry is present the
//! tool also supports a `query` fallback so an agent that does not know the
//! exact skill name can still discover the most relevant one.
//! What: `SkillLoaderTool` wraps a `SkillResolver` (+ optional `SkillRegistry`)
//! as a `ToolExecutor`.
//! Test: Covered by the resolver tests (exact-name resolution / missing-name)
//! and the registry tests in `skills::tests` (query handling).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::skills::{SkillRegistry, strip_frontmatter};
use crate::tools::traits::{SkillResolver, ToolExecutor, ToolResult};

/// `load_skill` tool — resolves and returns a named skill's content.
///
/// Why: Gives LLM agents a direct path to domain knowledge without baking
/// that knowledge into every system prompt. When a registry is present the
/// tool also supports a `query` fallback so an agent that does not know the
/// exact skill name can still discover the most relevant one.
/// What: Primarily calls `SkillResolver::resolve(name)`. When `registry` is
/// `Some(_)` and the caller supplied a `query` instead of `name`, ranks the
/// registry entries and returns an index plus the best match body.
/// Test: See `tests` below — exact name resolution and missing-name errors
/// are covered by the resolver tests; query handling is covered by the
/// registry tests in `skills::tests`.
pub struct SkillLoaderTool {
    resolver: Arc<dyn SkillResolver>,
    registry: Option<Arc<SkillRegistry>>,
}

impl SkillLoaderTool {
    /// Build a loader that only knows how to resolve by exact path.
    #[allow(dead_code)]
    pub fn new(resolver: Arc<dyn SkillResolver>) -> Self {
        Self {
            resolver,
            registry: None,
        }
    }

    /// Build a loader backed by both an fs resolver (for `.claude/skills/`
    /// style lookups) and a pre-scanned registry (for query ranking and
    /// frontmatter-aware rendering of `config/skills/*.md`).
    pub fn with_registry(resolver: Arc<dyn SkillResolver>, registry: Arc<SkillRegistry>) -> Self {
        Self {
            resolver,
            registry: Some(registry),
        }
    }
}

#[async_trait]
impl ToolExecutor for SkillLoaderTool {
    fn name(&self) -> &str {
        "load_skill"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "load_skill",
                "description": "Load the full Markdown content of a named skill (domain knowledge module). \
                    Provide `name` (exact match from list_skills) OR `query` (natural-language description; \
                    the best-ranked skill will be returned along with a short list of near-matches).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Exact skill name from list_skills output."
                        },
                        "query": {
                            "type": "string",
                            "description": "Natural-language query used to rank skills when `name` is omitted."
                        }
                    },
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let name_arg = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let query_arg = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if let Some(name) = name_arg {
            // Prefer the registry when available so the returned body has the
            // YAML frontmatter stripped; fall back to the generic resolver.
            if let Some(reg) = self.registry.as_ref()
                && reg.skills.iter().any(|s| s.name == name)
            {
                return match reg.load_content(name).await {
                    Ok(content) => ToolResult::ok(format!(
                        "## Skill: {name}\n\n{}",
                        strip_frontmatter(&content)
                    )),
                    Err(e) => ToolResult::err(e.to_string()),
                };
            }
            return match self.resolver.resolve(name) {
                Some(s) => ToolResult::ok(s),
                None => ToolResult::err(format!("Skill '{name}' not found.")),
            };
        }

        if let Some(query) = query_arg {
            let Some(reg) = self.registry.as_ref() else {
                return ToolResult::err(
                    "load_skill: 'query' requires a skill registry; provide 'name' instead",
                );
            };
            let matches = reg.search(query, 3);
            if matches.is_empty() {
                return ToolResult::ok(
                    "No matching skills found. Use list_skills to see all available.",
                );
            }
            let index = matches
                .iter()
                .map(|s| format!("- **{}**: {}", s.name, s.description))
                .collect::<Vec<_>>()
                .join("\n");
            let best = &matches[0];
            let body = reg
                .load_content(&best.name)
                .await
                .map(|c| strip_frontmatter(&c).to_string())
                .unwrap_or_default();
            return ToolResult::ok(format!(
                "## Matching Skills\n{index}\n\n## Best Match: {}\n\n{body}",
                best.name
            ));
        }

        ToolResult::err("load_skill: provide either 'name' or 'query'")
    }
}
