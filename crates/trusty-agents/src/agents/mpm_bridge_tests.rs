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

#[test]
fn drop_warning_lists_only_unmapped_keys() {
    let dropped = unmapped_keys(MPM_ARTIFACT);
    assert_eq!(dropped, vec!["effort", "agent_type", "version", "skills"]);
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
