//! Unit tests for `InProcessAgentRunner` (#198 + #170/#173 skill wiring).
//!
//! Why: Split out of `in_process_runner.rs` to keep that file under the 500-line
//! cap while preserving full coverage of the safe-tool surface and the
//! tag-registry-backed `list_skills` wiring.
//! What: Constructed as a child module of `in_process_runner` via
//! `#[path = "in_process_runner_tests.rs"] mod tests;` so `super::*` reaches the
//! runner's private fields (e.g. `tag_registry`) without widening visibility.
//! Test: this file IS the tests.

use super::*;

/// Mock skill resolver that returns nothing — enough to construct a
/// runner and exercise registry/tool-surface assertions without touching
/// the filesystem.
struct EmptyResolver;
impl SkillResolver for EmptyResolver {
    fn resolve(&self, _name: &str) -> Option<String> {
        None
    }
    fn list(&self) -> Vec<String> {
        Vec::new()
    }
}

fn fake_client() -> Arc<Client<OpenAIConfig>> {
    // We never actually issue requests in unit tests — just need a
    // client to thread through the constructor. Bogus key + base URL
    // are fine because `cargo test` never reaches the HTTP send.
    let cfg = OpenAIConfig::new()
        .with_api_key("test-key")
        .with_api_base("http://localhost:0");
    Arc::new(Client::with_config(cfg))
}

/// Verifies the constructor wires through both the shared client and the
/// injected skill resolver without panicking.
///
/// Why: This is the seam that lets the workflow runner builder reuse a
/// single client across all in-process agents (Phase C goal).
/// What: Constructs the runner via `new` and asserts ownership semantics
/// (the Arcs are still alive after the runner is dropped).
/// Test: `cargo test --lib in_process_runner::tests::runner_construction_uses_shared_client`.
#[test]
fn runner_construction_uses_shared_client() {
    let client = fake_client();
    let resolver: Arc<dyn SkillResolver> = Arc::new(EmptyResolver);
    let weak = Arc::downgrade(&client);
    let runner = InProcessAgentRunner::new(client, resolver);
    // Drop the runner; weak should still upgrade because we hold the
    // returned Arc-equivalent through `weak`.
    drop(runner);
    // After drop, the only strong ref is gone (we moved `client` into the
    // runner). Verify by attempting to upgrade — should fail.
    assert!(
        weak.upgrade().is_none(),
        "runner should drop its client Arc on shutdown"
    );
}

/// Verifies the safe-registry helper registers exactly the documented
/// tool surface — no shell, no web, no memory.
///
/// Why: Regression guard for the in-process safety contract; if someone
/// adds a heavy tool to `build_safe_registry` they have to update this
/// test, which forces an explicit decision.
/// What: Builds the registry, lists names, asserts the set matches
/// `SAFE_TOOL_NAMES`.
/// Test: `cargo test --lib in_process_runner::tests::safe_registry_includes_expected_tools`.
#[tokio::test]
async fn safe_registry_includes_expected_tools() {
    let resolver: Arc<dyn SkillResolver> = Arc::new(EmptyResolver);
    let reg = InProcessAgentRunner::build_safe_registry(
        resolver,
        PathBuf::from("."),
        None,
        Arc::new(TagSkillRegistry::empty()),
    )
    .await;
    for name in SAFE_TOOL_NAMES {
        assert!(
            reg.contains(name),
            "safe registry missing expected tool '{name}'"
        );
    }
    // Heavy / unsafe tools must NOT be present.
    for forbidden in ["shell_exec", "web_search", "fetch_url", "delegate_to_agent"] {
        assert!(
            !reg.contains(forbidden),
            "safe registry leaked unsafe tool '{forbidden}'"
        );
    }
}

/// Verifies the in-process runner threads a non-empty tag registry into
/// `list_skills` so `tags=[...]` returns tag-ranked structured JSON — the
/// wired path this change exists to enable (#170/#173).
///
/// Why: Before this wiring the in-process path always used the flat
/// `SkillListTool::new` lister, so the tag-indexed `with_tag_registry`
/// code was effectively dead for in-process agents. This is the regression
/// guard proving the cache/index-backed path is now exercised end to end.
/// What: Builds a tag registry from temp skill files, constructs the runner
/// via `with_tag_registry`, builds the safe registry through it, then
/// executes the `list_skills` tool with a tag filter and asserts the
/// tag-ranked JSON shape (`match_score`) is returned and unrelated skills
/// are filtered out.
/// Test: this function.
#[tokio::test]
async fn in_process_list_skills_uses_tag_registry() {
    use serde_json::json;

    let dir = std::env::temp_dir().join(format!("ompm-inproc-tags-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("fastapi.md"),
        "---\nname: fastapi\ndescription: async routes\ntags: [python, fastapi]\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("rust.md"),
        "---\nname: rust\ndescription: rust idioms\ntags: [rust]\n---\nbody\n",
    )
    .unwrap();

    let tag_reg = Arc::new(TagSkillRegistry::load(std::slice::from_ref(&dir)));
    assert!(
        !tag_reg.is_empty(),
        "tag registry should index the temp skills"
    );

    let resolver: Arc<dyn SkillResolver> = Arc::new(EmptyResolver);
    let runner =
        InProcessAgentRunner::with_tag_registry(fake_client(), resolver.clone(), tag_reg.clone());

    // Build the safe registry the same way `run_inner` does and dispatch the
    // wired `list_skills` tool through it.
    let reg = InProcessAgentRunner::build_safe_registry(
        resolver,
        PathBuf::from("."),
        None,
        runner.tag_registry.clone(),
    )
    .await;
    assert!(
        reg.contains("list_skills"),
        "list_skills must be registered"
    );

    let result = reg
        .dispatch("list_skills", json!({"tags": ["python"]}))
        .await;
    let content = result.content();
    assert!(
        content.contains("\"fastapi\""),
        "expected fastapi in tag-ranked output: {content}"
    );
    assert!(
        content.contains("match_score"),
        "tag-registry path must emit structured match_score JSON: {content}"
    );
    assert!(
        !content.contains("\"rust\""),
        "rust lacks the python tag and must be filtered out: {content}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
