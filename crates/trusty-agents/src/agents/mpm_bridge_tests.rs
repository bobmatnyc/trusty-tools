//! Unit tests for the trusty-mpm `.claude/agents` frontmatter bridge.
//!
//! Why: the correctness-critical part of this adapter is not what it maps but
//! what it deliberately REFUSES to map — `skills:` must never become a
//! permission grant, `tier` must never be declared, `extends:` must never be
//! propagated, and `tools:` must land in the exact-name slot rather than the
//! glob slot. Each of those is pinned by its own test so a future "helpful"
//! widening fails loudly.
//! What: covers scalar projection, body extraction, the unmapped-key set, the
//! four refusals above, and the `.claude/agents` directory predicate.
//! Test: this file.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;

/// Write `content` to `<dir>/<name>.md` and return its path.
fn write_agent(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(format!("{name}.md"));
    std::fs::write(&path, content).expect("write agent md");
    path
}

/// A realistic trusty-mpm deploy artifact: the flattened shape
/// `compose_agent` emits, block-style `skills:` and all.
const MPM_ARTIFACT: &str = "---\n\
name: rust-engineer\n\
role: engineer\n\
description: Rust 2024 specialist\n\
model: sonnet\n\
effort: balanced\n\
agent_type: engineer\n\
version: \"2.0.1\"\n\
skills:\n\
- toolchains-rust-core\n\
- git-workflow\n\
---\n\
\n\
# Rust Engineer\n\
\n\
Body prose.\n";

#[test]
fn projects_clean_scalars() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(tmp.path(), "rust-engineer", MPM_ARTIFACT);

    let cfg = load_mpm_agent(&path).expect("mpm artifact loads");

    assert_eq!(cfg.agent.name, "rust-engineer");
    assert_eq!(cfg.agent.role, "engineer");
    assert_eq!(cfg.agent.description, "Rust 2024 specialist");
    assert!(
        cfg.agent.model.contains("sonnet"),
        "declared model should survive resolve_model, got {}",
        cfg.agent.model
    );
    assert!(cfg.system_prompt.content.starts_with("# Rust Engineer"));
    assert!(cfg.system_prompt.content.ends_with("Body prose."));
}

/// `role` reaches `AgentInfo.role` NORMALIZED, never verbatim (#4502).
///
/// Why: `role` selects the tool-registry branch in `build_registry_for_agent`
/// and is checked against `ASSISTANT_ALLOWED_DELEGATE_ROLES` at every
/// delegation. A verbatim copy would let any string in a `.md` artifact reach
/// those gates directly, so this tier must fail CLOSED on anything the
/// reviewed table does not admit — `security` is a real trusty-mpm role with
/// no counterpart in the coarse vocabulary, and it must not become one.
#[test]
fn role_is_normalized_and_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let artifact =
        "---\nname: security\nrole: security\ndescription: Auditor\n---\n\n# Security\n\nBody.\n";
    let path = write_agent(tmp.path(), "security", artifact);

    let cfg = load_mpm_agent(&path).expect("mpm artifact loads");

    assert_eq!(
        cfg.agent.role,
        crate::agents::claude_mpm_role::UNMAPPED_ROLE,
        "an unmappable declared role must not survive verbatim"
    );
    assert!(
        !crate::runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES
            .contains(&cfg.agent.role.as_str()),
        "the fail-closed sentinel must never be role-eligible"
    );
}

/// `skills:` is a trusty-mpm CO-DEPLOYMENT DEPENDENCY list. trusty-agents'
/// `[skills].allow` is a PERMISSION GATE where `None` means "does not use
/// skill grants". Mapping one onto the other would silently turn a dependency
/// declaration into a grant — the single most dangerous same-name/
/// different-semantics collision between the two schemas.
#[test]
fn skills_never_become_a_permission_grant() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(tmp.path(), "rust-engineer", MPM_ARTIFACT);

    let cfg = load_mpm_agent(&path).expect("mpm artifact loads");

    assert!(
        cfg.skills.allow.is_none(),
        "[skills].allow must stay at its default `None` (does not use skill grants); \
         mapping trusty-mpm's dependency list here would forge a permission grant"
    );
    // The body-side prompt-skills slot must be untouched too.
    assert!(cfg.system_prompt.skills.is_none());
}

/// `tier` is DERIVED (`AgentInfo::tier()` -> `AgentTier::for_kind(role)`), so
/// an mpm-sourced sub-agent is L1 by construction. Declaring one here would be
/// meaningless at best and an escalation at worst.
#[test]
fn tier_is_never_populated() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(
        tmp.path(),
        "sneaky",
        "---\nname: sneaky\nrole: engineer\ntier: l0\n---\n\nBody.\n",
    );

    let cfg = load_mpm_agent(&path).expect("loads");

    assert!(
        cfg.agent.tier.is_none(),
        "a declared `tier:` in an mpm artifact must be dropped, not honoured"
    );
    assert_eq!(cfg.agent.tier(), crate::agents::AgentTier::L1Standard);
}

/// `.claude/agents` holds already-flattened DEPLOY artifacts, so this tier is
/// leaf-only: a residual `extends:` is warned about and ignored, and the field
/// is NOT propagated (propagating it would make `resolve_extends_in_map` chase
/// an mpm base name that is absent from this registry).
#[test]
fn extends_is_not_propagated() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(
        tmp.path(),
        "child",
        "---\nname: child\nrole: engineer\nextends: base-engineer\n---\n\nBody.\n",
    );

    let cfg = load_mpm_agent(&path).expect("loads despite extends");

    assert!(cfg.agent.extends.is_none());
    assert_eq!(cfg.agent.name, "child");
    assert_eq!(cfg.system_prompt.content, "Body.");
}

/// trusty-mpm's `tools:` is a flat list of EXACT tool names, so it maps to
/// `ToolsConfig::allowed` (exact-name allowlist) and never to
/// `ToolsConfig::allow` (glob patterns, where a trailing `*` widens).
#[test]
fn tools_map_to_exact_allowlist() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(
        tmp.path(),
        "narrow",
        "---\nname: narrow\nrole: qa\ntools: [Read, Grep]\n---\n\nBody.\n",
    );

    let cfg = load_mpm_agent(&path).expect("loads");

    assert_eq!(
        cfg.tools.allowed.as_deref(),
        Some(["Read".to_string(), "Grep".to_string()].as_slice())
    );
    assert!(
        cfg.tools.allow.is_none(),
        "the glob slot must stay unset — exact names routed through it could widen"
    );
}

/// `tools: []` is a deliberate deny-all on BOTH sides and must survive as
/// `Some(vec![])`, never collapse to the `None` that means "no restriction".
#[test]
fn empty_tools_list_stays_deny_all() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(
        tmp.path(),
        "denied",
        "---\nname: denied\nrole: qa\ntools: []\n---\n\nBody.\n",
    );

    let cfg = load_mpm_agent(&path).expect("loads");

    assert_eq!(cfg.tools.allowed, Some(Vec::new()));
}

/// Nothing loaded through this path grants a delegation target — catalog
/// population must never change reachability.
#[test]
fn projected_agent_grants_no_subagents() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(tmp.path(), "rust-engineer", MPM_ARTIFACT);

    let cfg = load_mpm_agent(&path).expect("loads");

    assert!(cfg.subagents.delegate_allowed.is_none());
}

/// #4511: `agent_type:` is CONSUMED now (it is the deployed dialect's `role:`),
/// so it must no longer appear in the "dropped, had no effect" warning. The
/// warning's whole job is to tell the truth about what took effect; leaving a
/// key it now reads on the dropped list is the same class of lie in reverse.
#[test]
fn drop_warning_lists_only_unmapped_keys() {
    let dropped = unmapped_keys(MPM_ARTIFACT);
    assert_eq!(dropped, vec!["effort", "version", "skills"]);
    assert!(
        !dropped.contains(&"agent_type".to_string()),
        "`agent_type` is read as the role fallback and must not be reported as dropped"
    );
}

#[test]
fn no_drop_warning_when_every_key_is_mapped() {
    let doc =
        "---\nname: plain\nrole: qa\ndescription: d\nmodel: sonnet\ntools: [Read]\n---\n\nB.\n";
    assert!(unmapped_keys(doc).is_empty());
}

#[test]
fn unmapped_keys_ignores_nested_and_body_lines() {
    let doc = "---\nname: n\nnested:\n  inner: v\n---\n\nbody_key: not a frontmatter key\n";
    assert_eq!(unmapped_keys(doc), vec!["nested"]);
}

#[test]
fn body_excludes_frontmatter() {
    assert_eq!(
        extract_body("---\nname: x\nskills:\n- a\n---\n\nPrompt text.\n"),
        "Prompt text."
    );
}

#[test]
fn body_keeps_interior_horizontal_rule() {
    let body = extract_body("---\nname: x\n---\n\nOne\n\n---\n\nTwo\n");
    assert!(body.contains("---"), "interior rule survived: {body}");
    assert!(body.starts_with("One") && body.ends_with("Two"));
}

#[test]
fn body_of_frontmatter_only_document_is_empty() {
    assert_eq!(extract_body("---\nname: x\n---\n"), "");
}

#[test]
fn malformed_frontmatter_falls_back_to_file_stem() {
    let tmp = TempDir::new().unwrap();
    // No opening fence at all: the shared reader yields default metadata and
    // the whole document becomes the prompt body.
    let path = write_agent(tmp.path(), "bodyonly", "Just a prompt, no frontmatter.\n");

    let cfg = load_mpm_agent(&path).expect("degrades rather than dropping the agent");

    assert_eq!(cfg.agent.name, "bodyonly");
    assert_eq!(cfg.agent.role, "agent");
    assert_eq!(cfg.system_prompt.content, "Just a prompt, no frontmatter.");
}

#[test]
fn missing_file_errors() {
    assert!(load_mpm_agent(Path::new("/nonexistent/agent.md")).is_err());
}

#[test]
fn claude_agents_dir_predicate_matches_both_tiers() {
    assert!(is_claude_agents_dir(Path::new(".claude/agents")));
    assert!(is_claude_agents_dir(Path::new("/home/user/.claude/agents")));
}

#[test]
fn claude_agents_dir_predicate_rejects_trusty_agents_dir() {
    assert!(!is_claude_agents_dir(Path::new(".trusty-agents/agents")));
    assert!(!is_claude_agents_dir(Path::new(
        "/home/u/.trusty-agents/agents"
    )));
    assert!(!is_claude_agents_dir(Path::new(".claude/skills")));
    assert!(!is_claude_agents_dir(Path::new("agents")));
}

// ---------------------------------------------------------------------------
// #4511: one role derivation for the catalog and for dispatch.
// ---------------------------------------------------------------------------

/// A claude-mpm-format deploy artifact: `agent_type:` carries the domain and
/// there is no `role:` key at all. This is the shape ~40 real deployed files
/// under `.claude/agents/` actually have.
const DEPLOYED_AGENT_TYPE_ARTIFACT: &str = "---\n\
name: aws-ops\n\
description: Cloud operations\n\
agent_type: ops\n\
version: \"1.0.0\"\n\
---\n\
\n\
# AWS Operations Agent\n";

/// The defect #4511 filed: this tier resolved `agent_type:`-only artifacts to
/// the fail-closed sentinel while the by-name dispatch tier resolved their
/// real domain, so the catalog and dispatch described the same file on disk
/// differently.
#[test]
fn agent_type_is_the_deployed_domain_fallback() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(tmp.path(), "aws-ops", DEPLOYED_AGENT_TYPE_ARTIFACT);

    let cfg = load_mpm_agent(&path).expect("mpm artifact loads");

    assert_eq!(cfg.agent.role, "ops");
}

/// The two dialect keys are a PREFERENCE, not a union — the ordering rule
/// lives in `normalize_role` and this tier must not re-derive or reorder it.
#[test]
fn declared_role_wins_over_agent_type() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(
        tmp.path(),
        "mixed",
        "---\nname: mixed\nrole: qa\nagent_type: engineer\n---\n\nBody.\n",
    );

    let cfg = load_mpm_agent(&path).expect("loads");

    assert_eq!(cfg.agent.role, "qa");
}

/// Reading a second key must not become a second chance at eligibility: an
/// `agent_type` with no counterpart in the coarse vocabulary still fails
/// closed, exactly as `role:` does.
#[test]
fn unmappable_agent_type_still_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let path = write_agent(
        tmp.path(),
        "sec",
        "---\nname: sec\nagent_type: security\n---\n\nBody.\n",
    );

    let cfg = load_mpm_agent(&path).expect("loads");

    assert_eq!(
        cfg.agent.role,
        crate::agents::claude_mpm_role::UNMAPPED_ROLE
    );
    assert!(
        !crate::runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES
            .contains(&cfg.agent.role.as_str())
    );
}

/// THE regression guard for #4511: the CATALOG path (this module) and the
/// by-name DISPATCH path (`claude_mpm_loader`) must derive the SAME role from
/// the SAME bytes. They read the file with different parsers by design, so
/// only a shared derivation keeps them in agreement — this test fails the
/// moment either side stops calling `claude_mpm_role::normalize_role` or
/// starts feeding it a different key set.
#[test]
fn catalog_and_dispatch_derive_the_same_role() {
    let tmp = TempDir::new().unwrap();
    for artifact in [
        DEPLOYED_AGENT_TYPE_ARTIFACT,
        MPM_ARTIFACT,
        "---\nname: x\nrole: research\n---\n\nB.\n",
        "---\nname: x\nagent_type: data-engineer\n---\n\nB.\n",
        // Fail-closed on both sides, not just on one.
        "---\nname: x\nagent_type: version-control\n---\n\nB.\n",
        "---\nname: x\nrole: security\nagent_type: engineer\n---\n\nB.\n",
        "---\nname: x\n---\n\nB.\n",
    ] {
        let path = write_agent(tmp.path(), "x", artifact);
        let catalog = load_mpm_agent(&path).expect("catalog path loads");
        let dispatch = crate::agents::claude_mpm_loader::parse_agent_file(&path, artifact)
            .expect("dispatch path parses");
        assert_eq!(
            catalog.agent.role, dispatch.role,
            "catalog and dispatch disagreed about {artifact:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// #4511 CAPABILITY-NEUTRALITY PROOF.
// ---------------------------------------------------------------------------

/// Mock runner that records every dispatch it is handed.
///
/// Why: "the delegation was refused" is only meaningful if the runner was
/// never reached — an assertion on the returned `ToolResult` alone would pass
/// for a tool that spawned the agent and then reported an error.
struct RecordingRunner {
    invoked: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::tools::traits::AgentRunner for RecordingRunner {
    async fn run(
        &self,
        agent_name: &str,
        _task: &str,
    ) -> anyhow::Result<crate::tools::traits::AgentOutput> {
        self.invoked.lock().unwrap().push(agent_name.to_string());
        Ok(crate::tools::traits::AgentOutput {
            content: "ok".into(),
            summary: None,
            usage: crate::perf::TokenUsage::default(),
        })
    }
}

/// Write the minimal fully-parseable agent TOML `AgentConfig::by_name_in`
/// requires, with an explicit role — the shape the delegate tool's role gate
/// actually resolves.
fn write_agent_toml_with_role(dir: &Path, name: &str, role: &str) {
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!(
            r#"
[agent]
name = "{name}"
role = "{role}"
model = "anthropic/claude-sonnet-4-6"
description = "test fixture"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
"#
        ),
    )
    .expect("write agent toml");
}

/// 🔴 The proof #4511 demands, not a claim: making a trusty-mpm-sourced agent
/// role-ELIGIBLE makes it reach NOTHING.
///
/// Why: `role` feeds two independent consumers — the tool-registry branch and
/// the coarse `ASSISTANT_ALLOWED_DELEGATE_ROLES` pre-filter — so widening the
/// set of agents that hold an allowlisted role is a security-relevant state
/// change and must be demonstrated capability-neutral rather than asserted to
/// be. This test stacks the deck as far as it can be legally stacked: the
/// delegator is an L0 assistant (no tier block), the role allowlist is the
/// real `ASSISTANT_ALLOWED_DELEGATE_ROLES` (so the newly-normalized `ops`
/// PASSES the role gate), the whitelist is seeded with the ENTIRE server-owned
/// floor (the most permissive posture any assistant can be configured into),
/// and the target resolves cleanly. It is still refused, because the floor is
/// a NAME list that role normalization cannot reach.
/// What: also runs the CONTROL — the same tool, same turn, reaching a real
/// floor name — so a regression that simply denied everything could not pass.
/// Test: this function IS the test.
#[tokio::test]
async fn normalized_mpm_role_still_reaches_nothing() {
    // 1. The state change: an artifact that was the inert sentinel before
    //    #4511 now carries a genuinely role-eligible domain.
    let artifacts = TempDir::new().unwrap();
    let path = write_agent(artifacts.path(), "aws-ops", DEPLOYED_AGENT_TYPE_ARTIFACT);
    let projected = load_mpm_agent(&path).expect("loads");
    assert_eq!(projected.agent.role, "ops", "the newly-derived role");
    assert!(
        crate::runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES
            .contains(&projected.agent.role.as_str()),
        "precondition: this role IS in the coarse allowlist, so the role gate \
         cannot be what refuses the delegation below"
    );

    // 2. The roster the delegate tool resolves against: the newly-eligible
    //    agent plus one real floor name as the control.
    let roster = TempDir::new().unwrap();
    write_agent_toml_with_role(roster.path(), "aws-ops", &projected.agent.role);
    write_agent_toml_with_role(roster.path(), "research-agent", "researcher");

    // 3. The assistant-tier delegate tool, wired exactly as
    //    `build_assistant_tier_registry` wires it, with the whole floor granted.
    let seeded: Vec<String> = crate::agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let runner = std::sync::Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = crate::tools::delegate::DelegateToAgentTool::new(runner.clone())
        .with_config_dirs(vec![roster.path().to_path_buf()])
        .with_allowed_target_roles(
            crate::runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        )
        .with_delegator(
            crate::runtime::tool_registry::ASSISTANT_TIER_ROLE,
            crate::agents::AgentTier::L0Orchestration,
            crate::tools::subagent_allow::SubagentAllowSet::over(
                crate::agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS,
                Some(&seeded),
            ),
        );

    // 4. REFUSED — role eligibility bought nothing.
    let refused = crate::tools::traits::ToolExecutor::execute(
        &tool,
        serde_json::json!({ "agent_name": "aws-ops", "task": "deploy" }),
    )
    .await;
    assert!(
        refused.is_error(),
        "a newly role-eligible mpm-sourced agent must still be refused: {}",
        refused.content()
    );
    assert!(
        runner.invoked.lock().unwrap().is_empty(),
        "the runner must never be reached"
    );

    // 5. CONTROL — the same tool DOES reach a floor name, so step 4 is a real
    //    refusal and not a suite that denies everything.
    let allowed = crate::tools::traits::ToolExecutor::execute(
        &tool,
        serde_json::json!({ "agent_name": "research-agent", "task": "investigate" }),
    )
    .await;
    assert!(
        !allowed.is_error(),
        "control: a floor name must still be reachable, got {}",
        allowed.content()
    );
    assert_eq!(
        runner.invoked.lock().unwrap().as_slice(),
        ["research-agent".to_string()]
    );
}

/// The other half of the proof: no mpm-sourced artifact can OCCUPY a floor
/// name and inherit its reachability.
///
/// Why: the floor is a name list, so "role normalization grants nothing" holds
/// only while an mpm artifact cannot become the agent a floor name resolves
/// to. `by_name_unresolved_src_in` tries the directory package, then
/// `<name>.toml`, then `<name>.md`, and only then the claude-mpm
/// `.claude/agents` fallback — so a bundled TOML always wins, in every
/// candidate directory, before an mpm artifact is consulted at all.
/// What: plants an mpm artifact NAMED after a floor entry, declaring an
/// eligible domain, next to the bundled-style TOML, and asserts the TOML is
/// what resolves.
/// Test: this function IS the test.
#[test]
fn an_mpm_artifact_cannot_shadow_a_floor_name() {
    let roster = TempDir::new().unwrap();
    for floor_name in crate::agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS {
        write_agent_toml_with_role(roster.path(), floor_name, "researcher");
        // A same-named mpm artifact declaring a role-eligible domain.
        write_agent(
            roster.path(),
            floor_name,
            "---\nname: usurper\nagent_type: engineer\n---\n\nBody.\n",
        );

        let resolved =
            crate::agents::AgentConfig::by_name_in(&[roster.path().to_path_buf()], floor_name)
                .expect("floor name resolves");

        assert_eq!(
            resolved.agent.name, *floor_name,
            "the bundled TOML must win the name, not the mpm artifact"
        );
        assert_eq!(
            resolved.agent.role, "researcher",
            "the mpm artifact's domain must not reach a floor name"
        );
    }
}
