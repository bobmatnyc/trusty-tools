//! `use_skill` tool — the on-invoke half of progressive-disclosure skill
//! loading (#2069).
//!
//! Why: The prompt only ever injects the cheap skill *metadata* catalog
//! (`skills::format_skill_catalog`); an agent that decides a listed skill is
//! relevant needs a way to actually fetch its full body. `UseSkillTool` is
//! that seam — a thin `ToolExecutor` wrapping any `SkillResolver`, so the
//! LLM's tool call is the "invoke" moment that triggers the lazy disk read.
//! What: `UseSkillTool::new(resolver)` takes any `Arc<dyn SkillResolver>`
//! (production: `skills::FsSkillResolver`); `execute` looks up the
//! `{"name": ...}` argument via `resolver.resolve()` and returns the body, or
//! a recoverable error naming the unknown skill.
//! Test: `tools::skill::tests::*` — resolves a known skill, errors on an
//! unknown one, errors on a missing `name` argument, schema shape.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{SkillResolver, ToolExecutor, ToolResult};

/// `ToolExecutor` that lazily fetches a named skill's full Markdown body.
///
/// Why: Keeps the LLM-facing surface (`name()`, `schema()`, `execute()`)
/// independent of which `SkillResolver` implementation backs it.
/// What: Delegates every call to the wrapped resolver's `resolve()`.
/// Test: `use_skill_resolves_known_skill`.
pub struct UseSkillTool {
    resolver: Arc<dyn SkillResolver>,
}

impl UseSkillTool {
    /// Construct a `UseSkillTool` backed by `resolver`.
    ///
    /// Why: Constructor injection keeps the tool testable with a stub
    /// resolver, matching every other `ToolExecutor` in this crate.
    /// What: Stores the `Arc<dyn SkillResolver>` for use in `execute`.
    /// Test: `use_skill_resolves_known_skill`.
    pub fn new(resolver: Arc<dyn SkillResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl ToolExecutor for UseSkillTool {
    fn name(&self) -> &str {
        "use_skill"
    }

    /// OpenAI function-call schema for `use_skill`.
    ///
    /// Why: The LLM needs the skill `name` field to make the call; the
    /// description points back at the catalog the prompt already injected.
    /// Test: `use_skill_schema_has_required_name`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "use_skill",
                "description": "Load the full instructions for a skill listed in the 'Available skills' catalog. Only call this for a skill you actually intend to use — it returns the skill's complete body.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "The exact skill name as it appears in the catalog."
                        }
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Execute a `use_skill` tool call.
    ///
    /// Why: This is the moment progressive disclosure actually defers to —
    /// the body is read from disk only now, not at prompt-assembly time.
    /// What: Parses `{name}` from `args`, calls `resolver.resolve(name)`, and
    /// converts the result into a `ToolResult`.
    /// Test: `use_skill_resolves_known_skill`, `use_skill_errors_on_unknown`,
    /// `use_skill_errors_on_missing_name`.
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("use_skill: missing required argument 'name'");
        };

        match self.resolver.resolve(name) {
            Some(body) => ToolResult::ok(body),
            None => ToolResult::err(format!("use_skill: unknown skill '{name}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::skills::SkillMetadata;

    /// In-memory stub resolver so these tests do not touch the filesystem.
    struct StubResolver {
        bodies: Mutex<HashMap<String, String>>,
    }

    impl SkillResolver for StubResolver {
        fn resolve(&self, name: &str) -> Option<String> {
            self.bodies.lock().expect("lock").get(name).cloned()
        }
        fn list(&self) -> Vec<String> {
            self.bodies.lock().expect("lock").keys().cloned().collect()
        }
        fn metadata(&self) -> Vec<SkillMetadata> {
            self.bodies
                .lock()
                .expect("lock")
                .keys()
                .map(|name| SkillMetadata {
                    name: name.clone(),
                    description: String::new(),
                })
                .collect()
        }
    }

    fn make_tool() -> UseSkillTool {
        let mut bodies = HashMap::new();
        bodies.insert("demo-skill".to_string(), "full demo body".to_string());
        UseSkillTool::new(Arc::new(StubResolver {
            bodies: Mutex::new(bodies),
        }))
    }

    /// `use_skill` returns the resolved body for a known skill name.
    ///
    /// Why: Core happy-path contract for the on-invoke load.
    /// Test: this test.
    #[tokio::test]
    async fn use_skill_resolves_known_skill() {
        let tool = make_tool();
        let result = tool.execute(json!({"name": "demo-skill"})).await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert_eq!(result.content(), "full demo body");
    }

    /// `use_skill` returns a recoverable error for an unknown skill name.
    ///
    /// Why: An LLM hallucinating a skill name must get an actionable error,
    /// not a panic.
    /// Test: this test.
    #[tokio::test]
    async fn use_skill_errors_on_unknown() {
        let tool = make_tool();
        let result = tool.execute(json!({"name": "ghost-skill"})).await;
        assert!(result.is_error());
        assert!(result.content().contains("unknown skill"));
    }

    /// `use_skill` errors when the `name` argument is missing.
    ///
    /// Test: this test.
    #[tokio::test]
    async fn use_skill_errors_on_missing_name() {
        let tool = make_tool();
        let result = tool.execute(json!({})).await;
        assert!(result.is_error());
        assert!(result.content().contains("missing required argument"));
    }

    /// The schema requires `name`.
    ///
    /// Test: this test.
    #[test]
    fn use_skill_schema_has_required_name() {
        let tool = make_tool();
        let schema = tool.schema();
        let required = schema["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("name")));
    }
}
