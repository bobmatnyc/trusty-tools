//! `search_skills` — find relevant skills in the local registry.
//!
//! Why: Agents need to discover which skill bodies are relevant to a task
//! without memorizing the full catalog.
//! What: `SearchSkillsTool` holds an optional `Arc<dyn SkillResolver>`. When
//! present it enumerates known skills and substring-matches; otherwise it
//! walks `.open-mpm/skills/` on disk; otherwise it returns a graceful payload.
//! Test: See `super::tests` — `search_skills_executes_with_resolver` and
//! `search_skills_scans_config_skills_dir`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{SkillResolver, ToolExecutor, ToolResult};

use super::search_code::{DEFAULT_SKILL_TOP_N, SKILL_PREVIEW_MAX_CHARS};

/// `search_skills` — find relevant skills in the local registry.
///
/// Why: Agents need to discover which skill bodies are relevant to a task
/// without memorizing the full catalog.
/// What: Holds an optional `Arc<dyn SkillResolver>`. When the resolver is
/// present, enumerates known skills, filters names/content by case-insensitive
/// substring match, and returns name + first-line + short preview. When
/// absent, walks `.open-mpm/skills/` on disk directly so the tool is still
/// useful in the simplest wiring. When neither is viable, returns a graceful
/// "unavailable" payload.
/// Test: `search_skills_executes_with_resolver` exercises the DI path;
/// `search_skills_scans_config_skills_dir` tests the fs fallback.
pub struct SearchSkillsTool {
    resolver: Option<Arc<dyn SkillResolver>>,
    /// Filesystem fallback root — defaults to `.open-mpm/skills` under CWD.
    skills_dir: PathBuf,
}

impl SearchSkillsTool {
    /// Construct with default `.open-mpm/skills` fallback and no resolver.
    pub fn new() -> Self {
        Self {
            resolver: None,
            skills_dir: PathBuf::from(".open-mpm").join("skills"),
        }
    }

    /// Construct with an explicit skills directory for filesystem fallback.
    #[allow(dead_code)]
    pub fn with_skills_dir(mut self, dir: PathBuf) -> Self {
        self.skills_dir = dir;
        self
    }

    /// Construct with a `SkillResolver` that takes priority over the fs scan.
    pub fn with_resolver(resolver: Arc<dyn SkillResolver>) -> Self {
        Self {
            resolver: Some(resolver),
            skills_dir: PathBuf::from(".open-mpm").join("skills"),
        }
    }
}

impl Default for SearchSkillsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for SearchSkillsTool {
    fn name(&self) -> &str {
        "search_skills"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "search_skills",
                "description": "Find relevant skills by case-insensitive substring match against the skill name and first few lines of content. Returns {name, first_line, preview} entries.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "top_n": {"type": "integer", "description": "Default 3."}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("search_skills: missing 'query'");
        };
        let top_n = args
            .get("top_n")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_SKILL_TOP_N);

        let needle = query.to_lowercase();
        let mut hits: Vec<Value> = Vec::new();

        // Resolver path (preferred): enumerate names, load bodies, substring match.
        if let Some(resolver) = self.resolver.as_ref() {
            for name in resolver.list() {
                let body = resolver.resolve(&name).unwrap_or_default();
                if !name.to_lowercase().contains(&needle) && !body.to_lowercase().contains(&needle)
                {
                    continue;
                }
                let first_line = body
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .to_string();
                let preview: String = body
                    .chars()
                    .take(SKILL_PREVIEW_MAX_CHARS)
                    .collect::<String>();
                hits.push(json!({
                    "name": name,
                    "first_line": first_line,
                    "preview": preview,
                }));
                if hits.len() >= top_n {
                    break;
                }
            }
            let out = json!({
                "query": query,
                "hits": hits,
            });
            return ToolResult::ok(out.to_string());
        }

        // Filesystem fallback: scan `skills_dir` for `*.md` files.
        let entries = match std::fs::read_dir(&self.skills_dir) {
            Ok(e) => e,
            Err(e) => {
                let out = json!({
                    "error": format!(
                        "skills directory {} not readable: {e}",
                        self.skills_dir.display()
                    ),
                    "query": query,
                    "hits": []
                });
                return ToolResult::ok(out.to_string());
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let body = std::fs::read_to_string(&path).unwrap_or_default();
            if !name.to_lowercase().contains(&needle) && !body.to_lowercase().contains(&needle) {
                continue;
            }
            let first_line = body
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .to_string();
            let preview: String = body
                .chars()
                .take(SKILL_PREVIEW_MAX_CHARS)
                .collect::<String>();
            hits.push(json!({
                "name": name,
                "first_line": first_line,
                "preview": preview,
            }));
            if hits.len() >= top_n {
                break;
            }
        }

        let out = json!({
            "query": query,
            "hits": hits,
        });
        ToolResult::ok(out.to_string())
    }
}
