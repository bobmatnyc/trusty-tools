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

/// The stable tool name for `UseSkillTool` (matches `name()`/the schema's
/// `function.name` verbatim).
///
/// Why: (#2070) The agent loop must identify a skill-body-load result by tool
/// name to pin it against compaction (vision spec §5.4 item 5: "preserve
/// skill outputs... forever"); a shared constant — mirroring
/// `tools::finish_task::FINISH_TASK_TOOL_NAME`'s identical role for
/// `finish_task` — means that identification can never drift from the tool's
/// actual wire name.
/// Test: `tools::skill::tests::name_matches_constant`.
pub const USE_SKILL_TOOL_NAME: &str = "use_skill";

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
        USE_SKILL_TOOL_NAME
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
                "name": USE_SKILL_TOOL_NAME,
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

    /// `use_skill` rejects a path-traversal payload in a namespaced
    /// `<plugin>:<name>` argument end-to-end — through the REAL
    /// `skills::FsSkillResolver` (not the in-memory `StubResolver` the
    /// other tests here use), which is what actually backs this tool in
    /// production (code-critic PR #3547 review, CRITICAL 2).
    ///
    /// Why: `UseSkillTool::execute` forwards the LLM's raw `name` argument
    /// straight to `resolver.resolve()` with no validation of its own — the
    /// guard must live in the resolver, and this test proves it actually
    /// does, using the exact production wiring. A real "secret" file sits
    /// at the traversal target outside the plugin's `skills_dir`; if the
    /// guard did not fire, both payloads would resolve to it.
    /// What: a tempdir shaped `<root>/.claude/skills` (empty) +
    /// `<root>/.claude/plugins/my-plugin/skills/demo-skill/SKILL.md`;
    /// `name: "my-plugin:../../secret"` and `name: "my-plugin:.."` both
    /// error, and the returned error never contains the secret's content.
    /// Test: this test.
    #[tokio::test]
    async fn use_skill_rejects_plugin_traversal_end_to_end() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills_dir).expect("mkdir skills");

        let plugin_skills_dir = tmp
            .path()
            .join(".claude")
            .join("plugins")
            .join("my-plugin")
            .join("skills");
        std::fs::create_dir_all(plugin_skills_dir.join("demo-skill")).expect("mkdir");
        std::fs::write(
            plugin_skills_dir.join("demo-skill").join("SKILL.md"),
            "---\nname: demo-skill\n---\n\nDemo body.\n",
        )
        .expect("write demo skill");

        // The traversal target, one level above `.claude/plugins/my-plugin/skills/`.
        let secret_dir = tmp.path().join(".claude").join("plugins").join("secret");
        std::fs::create_dir_all(&secret_dir).expect("mkdir secret");
        std::fs::write(
            secret_dir.join("SKILL.md"),
            "---\nname: secret\n---\n\nSHOULD NEVER BE READ.\n",
        )
        .expect("write secret");

        let resolver: Arc<dyn SkillResolver> =
            Arc::new(crate::skills::FsSkillResolver::new(skills_dir));
        let tool = UseSkillTool::new(resolver);

        for payload in ["my-plugin:../secret", "my-plugin:.."] {
            let result = tool.execute(json!({"name": payload})).await;
            assert!(result.is_error(), "payload {payload:?} must be rejected");
            assert!(
                !result.content().contains("SHOULD NEVER BE READ"),
                "payload {payload:?} must never read the secret file, got: {}",
                result.content()
            );
        }
    }

    /// `use_skill` with `name: "my-plugin:leak"` — a validly-namespaced,
    /// validly-pathed name whose `SKILL.md` is a symlink escaping the
    /// plugin's `skills/` directory — is rejected end-to-end through the
    /// REAL `skills::FsSkillResolver`, and the secret content it points at
    /// never reaches the tool result (code-critic PR #3547 re-review,
    /// CRITICAL 5, CWE-59).
    ///
    /// Why: `is_valid_namespaced_name` and the directory guard both pass
    /// for `my-plugin:leak` — only the LEAF FILE identity is wrong. This
    /// proves `plugins::skills::resolve_plugin_skill_body`'s containment
    /// check actually fires on the exact production path `use_skill`
    /// drives.
    /// Test: this test.
    #[tokio::test]
    #[cfg(unix)]
    async fn use_skill_rejects_symlinked_plugin_skill_leak() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills_dir).expect("mkdir skills");

        let leak_skill_dir = tmp
            .path()
            .join(".claude")
            .join("plugins")
            .join("my-plugin")
            .join("skills")
            .join("leak");
        std::fs::create_dir_all(&leak_skill_dir).expect("mkdir leak dir");

        let secret_dir = tmp.path().join("outside");
        std::fs::create_dir_all(&secret_dir).expect("mkdir");
        let secret_path = secret_dir.join("id_rsa");
        std::fs::write(&secret_path, "SECRET_KEY_MATERIAL").expect("write secret");
        std::os::unix::fs::symlink(&secret_path, leak_skill_dir.join("SKILL.md")).expect("symlink");

        let resolver: Arc<dyn SkillResolver> =
            Arc::new(crate::skills::FsSkillResolver::new(skills_dir));
        let tool = UseSkillTool::new(resolver);

        let result = tool.execute(json!({"name": "my-plugin:leak"})).await;

        assert!(result.is_error(), "symlinked plugin skill must be rejected");
        assert!(
            !result.content().contains("SECRET_KEY_MATERIAL"),
            "the secret content must never appear in the tool result, got: {}",
            result.content()
        );
    }

    /// `name()` and the schema's `function.name` both match
    /// `USE_SKILL_TOOL_NAME` — no drift between the constant and the wire
    /// name (#2070 depends on this identifying skill outputs for pinning).
    ///
    /// Test: this test.
    #[test]
    fn name_matches_constant() {
        let tool = make_tool();
        assert_eq!(tool.name(), USE_SKILL_TOOL_NAME);
        assert_eq!(
            tool.schema()["function"]["name"].as_str(),
            Some(USE_SKILL_TOOL_NAME)
        );
        assert_eq!(USE_SKILL_TOOL_NAME, "use_skill");
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
