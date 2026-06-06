//! The `list_skills` tool.
//!
//! Why: Agents need fast, deterministic skill discovery keyed by
//! language/framework/workflow tags (#168). When a tag-indexed registry is
//! available, results are ranked by tag-overlap score in O(1) per tag with
//! no LLM or embedding call on the path.
//! What: `SkillListTool` holds an optional legacy `SkillRegistry` (for the
//! pre-#168 fallback rendering) AND an optional tag-indexed `TagSkillRegistry`.
//! When `tags` are supplied, the tag registry wins; otherwise the tool falls
//! back to the legacy index or a flat resolver list.
//! Test: `super::list_skills_returns_by_tag` in the parent module.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::skills::SkillRegistry;
use crate::skills::registry::SkillRegistry as TagSkillRegistry;
use crate::tools::traits::{SkillResolver, ToolExecutor, ToolResult};

/// `list_skills` tool — enumerates available skill names (and descriptions
/// when a registry is provided).
///
/// Why: Agents need fast, deterministic skill discovery keyed by
/// language/framework/workflow tags (#168). When a tag-indexed registry is
/// available, results are ranked by tag-overlap score in O(1) per tag with
/// no LLM or embedding call on the path.
/// What: Holds an optional legacy `SkillRegistry` (for the pre-#168 fallback
/// rendering) AND an optional tag-indexed `TagSkillRegistry`. When `tags` are
/// supplied to the tool call, the tag registry wins; otherwise the tool
/// falls back to the legacy index or a flat resolver list.
/// Test: `list_skills_returns_by_tag` (in this module).
pub struct SkillListTool {
    resolver: Arc<dyn SkillResolver>,
    registry: Option<Arc<SkillRegistry>>,
    tag_registry: Option<Arc<TagSkillRegistry>>,
}

impl SkillListTool {
    /// Construct a lister backed by a resolver but without a rich registry.
    ///
    /// Why: Useful in tests and in call sites that don't yet own a
    /// `SkillRegistry`; the tool falls back to a flat name list.
    /// What: Plain struct literal with `registry = None`.
    /// Test: Implicit — covered by integration via the research-agent registry.
    #[allow(dead_code)]
    pub fn new(resolver: Arc<dyn SkillResolver>) -> Self {
        Self {
            resolver,
            registry: None,
            tag_registry: None,
        }
    }

    /// Build a lister that, when called, returns the registry's rich index
    /// (name + description + tags) instead of a flat name list.
    pub fn with_registry(resolver: Arc<dyn SkillResolver>, registry: Arc<SkillRegistry>) -> Self {
        Self {
            resolver,
            registry: Some(registry),
            tag_registry: None,
        }
    }

    /// Build a lister backed by BOTH the legacy registry (fallback rendering)
    /// AND the tag-indexed registry (#168) used for `tags`-parameter queries.
    ///
    /// #170: Wired into `build_registry_for_agent` in `src/main.rs` so every
    /// sub-agent's `list_skills` tool uses tag-ranked results when a non-empty
    /// `TagSkillRegistry` is available.
    pub fn with_tag_registry(
        resolver: Arc<dyn SkillResolver>,
        registry: Option<Arc<SkillRegistry>>,
        tag_registry: Arc<TagSkillRegistry>,
    ) -> Self {
        Self {
            resolver,
            registry,
            tag_registry: Some(tag_registry),
        }
    }
}

#[async_trait]
impl ToolExecutor for SkillListTool {
    fn name(&self) -> &str {
        "list_skills"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "list_skills",
                "description": "List skills available to load. When `tags` are provided, \
                    returns skills ranked by tag-overlap score (most matching tags first). \
                    Without `tags`, returns all skills sorted alphabetically.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "tags": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Tag filter. Example: [\"python\",\"fastapi\",\"pytest\"]. \
                                Common tag categories: language (python,rust,typescript,go), \
                                framework (fastapi,axum,react,django,flask), \
                                testing (pytest,jest,testing), \
                                workflow (wave-planning,tdd,docker)."
                        }
                    },
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        // Extract tag filter if any.
        let tag_strings: Vec<String> = args
            .get("tags")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // Tag-indexed path (#168): when a tag registry is present and the
        // caller supplied tags, rank by overlap and return structured JSON.
        if let Some(tag_reg) = self.tag_registry.as_ref() {
            if !tag_strings.is_empty() {
                let refs: Vec<&str> = tag_strings.iter().map(String::as_str).collect();
                let matches = tag_reg.find_by_tags(&refs);
                if matches.is_empty() {
                    return ToolResult::ok(
                        "{\"skills\":[],\"note\":\"no skills matched the given tags\"}".to_string(),
                    );
                }
                let items: Vec<Value> = matches
                    .iter()
                    .map(|m| {
                        let score = tag_reg.tag_overlap_score(&m.name, &refs);
                        json!({
                            "name": m.name,
                            "description": m.description,
                            "tags": m.tags,
                            "match_score": score,
                        })
                    })
                    .collect();
                return ToolResult::ok(json!({ "skills": items }).to_string());
            }
            // No tags → list all, alphabetically.
            let mut all: Vec<Value> = tag_reg
                .list()
                .into_iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "description": m.description,
                        "tags": m.tags,
                    })
                })
                .collect();
            all.sort_by(|a, b| {
                a.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
            });
            return ToolResult::ok(json!({ "skills": all }).to_string());
        }

        // Legacy fallback paths (pre-#168 behavior).
        if let Some(reg) = self.registry.as_ref()
            && !reg.is_empty()
        {
            return ToolResult::ok(reg.format_index());
        }
        let names = self.resolver.list();
        if names.is_empty() {
            return ToolResult::ok("(no skills available)");
        }
        ToolResult::ok(names.join("\n"))
    }
}
