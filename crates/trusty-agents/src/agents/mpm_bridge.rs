//! Read a trusty-mpm-deployed `.claude/agents/*.md` artifact with the SHARED
//! frontmatter reader and project it onto trusty-agents' `AgentConfig`.
//!
//! Why: the owner's directive is "the same agents across the system — mpm,
//! code, agents" with as little rewriting as feasible. trusty-mpm composes its
//! agents and DEPLOYS them, already flattened, to `~/.claude/agents/*.md` and
//! `<project>/.claude/agents/*.md`. `AgentRegistry::load` already scans both
//! of those directories as a first-class roster tier, but it parses them with
//! `registry::md_agent::parse_md_agent` — a hand-rolled `serde_yaml` reader
//! shaped for trusty-agents' OWN `.md` overlay schema, not for trusty-mpm's.
//! The two schemas disagree (mpm's `tools:` is a flat name list; the overlay's
//! is a nested `ToolsConfig` map — feeding the former to the latter is a hard
//! `serde_yaml` error that silently drops the whole agent), so the tier that
//! reads mpm's artifacts should use mpm's own reader. That reader already
//! exists and is already a dependency of this crate:
//! `trusty_agents_common::agents::metadata::agent_metadata_from_str`, which
//! wraps the identical `split_frontmatter` grammar `compose_agent` emits with.
//! trusty-code adopted exactly this reader for exactly this artifact class in
//! #3539 (`plugins::agents::load_plugin_agent`); this module mirrors that
//! precedent so all three products read one grammar.
//!
//! LEAF-ONLY, by decision: this consumes the already-flattened DEPLOY artifact,
//! never trusty-mpm's pre-compose source tree. `compose_agent` resolves and
//! strips `extends:` before deploy, so there is nothing left to resolve —
//! a residual `extends:` is warned about and ignored, and `AgentInfo::extends`
//! is left `None` so `registry::resolve_extends_in_map` never chases an mpm
//! base name that does not exist in this registry.
//!
//! What: [`load_mpm_agent`] (read one file, warn about every field this crate
//! has no home for, project the rest) and [`is_claude_agents_dir`] (the
//! predicate that selects the `.claude/agents` tier).
//! Test: `tests::*` in `mpm_bridge_tests.rs` — scalar projection, `extends:`
//! ignored, `skills:` never becoming a permission grant, `tier` never
//! populated, `tools:` mapped to the exact-name allowlist, the aggregated
//! drop warning, and the directory predicate.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use trusty_agents_common::agents::frontmatter::parse_kv_line;
use trusty_agents_common::agents::metadata::{AgentMetadata, agent_metadata_from_str};

use crate::agents::{
    AgentCompressConfig, AgentConfig, AgentInfo, LlmParams, RunnerKind, SystemPrompt, ToolChoice,
    ToolsConfig,
};
use crate::llm::adapter::adapter_for_model;

/// The frontmatter keys this projection actually consumes.
///
/// Why: the drop warning is computed as "every key present minus these"
/// rather than from a hardcoded list of known-unsupported mpm keys. A
/// deny-list would go stale the moment trusty-mpm's schema grows a key; an
/// allow-list cannot, because a new key is unmapped by construction and shows
/// up in the warning on its first appearance.
/// What: lowercase keys, matching [`parse_kv_line`]'s case-folded output.
/// `extends` is listed here because it is deliberately consumed (to warn) —
/// it gets its own, more specific message rather than the aggregated one.
/// Test: `tests::drop_warning_lists_only_unmapped_keys`.
const CONSUMED_KEYS: &[&str] = &[
    "name",
    "role",
    "description",
    "model",
    "max_tokens",
    "tools",
    "extends",
];

/// Is `dir` a `.claude/agents` directory — the tier that holds trusty-mpm's
/// deployed artifacts?
///
/// Why: `AgentRegistry::load` walks a mixed list of search paths
/// (`agent_search_paths`): `.trusty-agents/agents` holds trusty-agents' OWN
/// `.md` overlay schema and must keep using `parse_md_agent`, while
/// `.claude/agents` holds trusty-mpm's deploy artifacts and must use this
/// module. The loader needs one predicate to tell the two apart, and matching
/// on the directory SHAPE (rather than on a string equal to a hardcoded path)
/// is what makes it work identically for the project-local
/// `.claude/agents` and the `$HOME/.claude/agents` entries.
/// What: `true` when `dir`'s final component is `agents` and its parent's
/// final component is `.claude`. Purely lexical — no IO, no canonicalization,
/// so a caller that already knows the directory exists pays nothing.
/// Test: `tests::claude_agents_dir_predicate_matches_both_tiers`,
/// `tests::claude_agents_dir_predicate_rejects_trusty_agents_dir`.
pub(crate) fn is_claude_agents_dir(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == "agents")
        && dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == ".claude")
}

/// Read one trusty-mpm-deployed agent file and project it onto `AgentConfig`.
///
/// Why: the single entry point `AgentRegistry::load` calls for the
/// `.claude/agents` tier, so the read/warn/project contract is written once.
/// What: reads `path`, parses its frontmatter with the SHARED
/// [`agent_metadata_from_str`] (never `compose_agent` — the artifact is
/// already flattened), emits the two warnings described on
/// [`warn_dropped_fields`] and below, then projects via
/// [`project_mpm_agent`]. An unreadable file is the only error path; a
/// malformed frontmatter block degrades to empty metadata (the shared
/// reader's own documented behavior) and the file-stem name, rather than
/// dropping the agent.
/// Test: `tests::projects_clean_scalars`, `tests::missing_file_errors`,
/// `tests::malformed_frontmatter_falls_back_to_file_stem`.
pub(crate) fn load_mpm_agent(path: &Path) -> anyhow::Result<AgentConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read trusty-mpm agent md {}", path.display()))?;

    let default_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let meta = agent_metadata_from_str(&raw);
    let agent_name = meta.name.as_deref().unwrap_or(default_name);

    if let Some(parent) = &meta.extends {
        tracing::warn!(
            agent = %agent_name,
            path = %path.display(),
            extends = %parent,
            "trusty-mpm agent declares `extends:` — .claude/agents holds already-flattened \
             DEPLOY artifacts, so this tier is leaf-only and the parent is NOT resolved; \
             loading this file's own frontmatter and body only"
        );
    }
    warn_dropped_fields(agent_name, path, &raw);

    Ok(project_mpm_agent(default_name, meta, extract_body(&raw)))
}

/// Warn ONCE, naming every frontmatter key this projection has no home for.
///
/// Why: trusty-mpm's schema is richer than trusty-agents' `.md` surface
/// (`skills:`, `initialPrompt:`, `agent_type:`, `version:`, `effort:`, …).
/// Silently discarding them is how a user ends up believing a declaration
/// took effect when it did not, so every drop is reported — but as ONE
/// aggregated line per agent rather than one line per key, mirroring
/// trusty-code's `plugins::agents::warn_unsupported_fields` (#3539).
/// What: scans the frontmatter block's top-level keys with the SHARED
/// [`parse_kv_line`] (presence detection only — the values that ARE mapped
/// were already parsed by `agent_metadata_from_str`), subtracts
/// [`CONSUMED_KEYS`], and logs the remainder. No-op when nothing is dropped.
/// Test: `tests::drop_warning_lists_only_unmapped_keys`,
/// `tests::no_drop_warning_when_every_key_is_mapped`.
fn warn_dropped_fields(agent_name: &str, path: &Path, raw: &str) {
    let dropped = unmapped_keys(raw);
    if dropped.is_empty() {
        return;
    }
    tracing::warn!(
        agent = %agent_name,
        path = %path.display(),
        dropped = %dropped.join(", "),
        "trusty-mpm agent declares frontmatter key(s) trusty-agents has no slot for — dropped; \
         note `skills:` in particular is a trusty-mpm CO-DEPLOYMENT DEPENDENCY list and is NOT \
         mapped onto trusty-agents' `[skills].allow`, which is a permission GATE"
    );
}

/// The top-level frontmatter keys in `raw` that [`project_mpm_agent`] does not
/// consume, in first-seen order, de-duplicated.
///
/// Why: split out from [`warn_dropped_fields`] so the key-set logic is
/// directly assertable in a test without capturing a `tracing` subscriber.
/// What: walks only the block between the opening `---` and the next `---`,
/// and only lines with no leading indentation (a nested map's or block
/// sequence's members are part of their parent key, not keys in their own
/// right). Returns empty for a document with no frontmatter fence.
/// Test: `tests::drop_warning_lists_only_unmapped_keys`,
/// `tests::unmapped_keys_ignores_nested_and_body_lines`.
fn unmapped_keys(raw: &str) -> Vec<String> {
    let mut lines = raw.trim_start_matches('\u{feff}').lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return Vec::new(),
    }
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        // Indented lines belong to the preceding key (nested map entries,
        // block-sequence items); they are never keys themselves.
        if line.starts_with([' ', '\t', '-']) {
            continue;
        }
        let Some((key, _)) = parse_kv_line(line) else {
            continue;
        };
        if !CONSUMED_KEYS.contains(&key.as_str()) && !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// The prose body of a composed agent document — everything after the closing
/// frontmatter fence.
///
/// Why: this becomes `system_prompt.content` verbatim. It is located
/// STRUCTURALLY (scan to the closing fence) rather than by re-reading parsed
/// frontmatter, which is the same guarantee trusty-code's
/// `md_loader::extract_body` documents: trusty-mpm's compose step can enrich
/// a role-derived `initialPrompt:` INTO the frontmatter, and that value must
/// never leak into a trusty-agents system prompt. Never inspecting the block's
/// contents makes the leak unrepresentable rather than merely unimplemented.
/// What: skips the opening `---` line and everything through the next `---`
/// line, returns the trimmed remainder. A document with no opening fence
/// returns the whole input trimmed (a body-only file is still a usable
/// prompt). An interior `---` horizontal rule in the prose survives, because
/// the scan stops at the FIRST closing fence.
/// Test: `tests::body_excludes_frontmatter`,
/// `tests::body_keeps_interior_horizontal_rule`,
/// `tests::body_of_frontmatter_only_document_is_empty`.
fn extract_body(raw: &str) -> String {
    let mut lines = raw.trim_start_matches('\u{feff}').lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return raw.trim().to_string(),
    }
    let mut in_frontmatter = true;
    let mut body: Vec<&str> = Vec::new();
    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        body.push(line);
    }
    body.join("\n").trim().to_string()
}

/// Project parsed trusty-mpm metadata + body onto trusty-agents'
/// `AgentConfig`.
///
/// Why: centralises the field-by-field mapping so [`load_mpm_agent`] stays a
/// thin IO wrapper, and so every deliberate NON-mapping below has one place to
/// be read and tested.
/// What, field by field:
/// - `name` -> `agent.name`, falling back to `default_name` (the file stem),
///   exactly as `parse_md_agent` identifies an unnamed file.
/// - `role` -> `agent.role`, defaulting to `"agent"` — byte-identical to
///   `parse_md_agent`'s default, so switching this tier's parser changes no
///   agent's role and therefore no agent's derived tier (see below).
/// - `description` -> `agent.description`; `model` -> `agent.model` after
///   `resolve_model` (the same `TAGENT_MODEL_*` / default env resolution
///   `parse_md_agent` applies on its non-`extends` path — this tier is always
///   that path, since it is leaf-only).
/// - `max_tokens` -> `llm.max_tokens`, defaulting to `parse_md_agent`'s
///   historical `8192` when the artifact declares none (which is the norm —
///   trusty-mpm agents do not set this key).
/// - `tools: Option<Vec<String>>` -> `tools.allowed`, a DIRECT map. Both sides
///   are an EXACT-NAME allowlist with identical three-way semantics: `None` =
///   no restriction, `Some(vec![])` = deny-all, `Some(list)` = exactly those
///   names (see `ToolsConfig::allowed` and
///   `agents_common::builder::Frontmatter::tools`). The alternative slot,
///   `tools.allow`, is a GLOB list where a trailing `*` is a suffix wildcard —
///   routing exact names through it would let a literal tool name containing
///   `*` widen into a pattern, which is strictly MORE permissive than what the
///   artifact declared. Where the two readings differ, the restrictive one
///   wins, so: `allowed`, and `allow` stays `None`. This is also the exact
///   mapping trusty-code made for the same artifact class (`md_loader`'s
///   `ToolsConfig { allowed: meta.tools }`). Note trusty-mpm OVERRIDE-merges
///   `tools:` across an `extends` chain while trusty-agents UNIONs it; the
///   difference is unreachable here because this tier resolves no chain at all.
/// - `skills` -> DROPPED, deliberately. trusty-mpm's `skills:` is a
///   co-deployment DEPENDENCY list ("ship these skill files alongside me");
///   trusty-agents' `[skills].allow` is a PERMISSION GATE whose `None` means
///   "this agent does not use skill grants" and whose `Some([])` means "grants
///   none". Mapping one to the other would silently convert a dependency
///   declaration into a grant, so `skills` is left at `SkillsConfig::default()`
///   and reported in the drop warning.
/// - `extends` -> NOT propagated to `agent.extends`. Deploy artifacts are
///   pre-flattened; leaving the field `Some` would make
///   `resolve_extends_in_map` chase an mpm base name absent from this registry
///   and log a resolution failure for an agent that has nothing to resolve.
/// - `tier` -> NOT populated. It is DERIVED (`AgentInfo::tier()` ->
///   `AgentTier::for_kind(role)`); an mpm-sourced sub-agent is L1 by
///   construction, and declaring a tier here would either restate that or
///   forge an escalation.
/// - `subagents`/`permissions`/`rbac`/`listeners`/`stores` -> defaults. In
///   particular `SubagentsConfig::default()` grants NO delegation targets, so
///   nothing loaded through this path becomes reachable from any assistant.
/// Test: `tests::projects_clean_scalars`, `tests::tools_map_to_exact_allowlist`,
/// `tests::skills_never_become_a_permission_grant`,
/// `tests::tier_is_never_populated`, `tests::extends_is_not_propagated`,
/// `tests::projected_agent_grants_no_subagents`.
fn project_mpm_agent(default_name: &str, meta: AgentMetadata, body: String) -> AgentConfig {
    let name = meta.name.unwrap_or_else(|| default_name.to_string());
    let model = crate::agents::resolve_model(&name, &meta.model.unwrap_or_default(), None).0;
    let adapter: Arc<dyn crate::llm::adapter::ModelAdapter> = Arc::from(adapter_for_model(&model));

    AgentConfig {
        agent: AgentInfo {
            name,
            role: meta.role.unwrap_or_else(|| "agent".to_string()),
            model,
            description: meta.description.unwrap_or_default(),
            persistent_session: false,
            runner: RunnerKind::Subprocess,
            capabilities: None,
            display_name: None,
            hidden: false,
            kind: "assistant".to_string(),
            prompt_label: None,
            // Leaf-only: see this function's doc comment.
            extends: None,
            // Derived, never declared: see this function's doc comment.
            tier: None,
        },
        llm: LlmParams {
            temperature: 0.2,
            max_tokens: meta.max_tokens.unwrap_or(8192),
            model_override: None,
            enable_prompt_caching: true,
            max_turns: 20,
            persona_max_turns: None,
            tool_choice: ToolChoice::Auto,
            use_finish_task: false,
            use_anthropic_direct: false,
            claude_allowed_tools: Vec::new(),
            aws_profile: None,
            aws_region: None,
            elevation_threshold: None,
            elevation_model: None,
            stop_sequences: Vec::new(),
            routing_model: None,
            thinking_enabled: None,
        },
        system_prompt: SystemPrompt {
            content: body,
            skills: None,
        },
        tools: ToolsConfig {
            allowed: meta.tools,
            ..ToolsConfig::default()
        },
        compress: AgentCompressConfig::default(),
        runner_config: crate::agents::RunnerConfig::default(),
        session: crate::agents::SessionCompressionConfig::default(),
        plugins: crate::agents::AgentPluginsConfig::default(),
        rbac: crate::agents::RbacConfig::default(),
        workstreams: crate::agents::WorkstreamContextConfig::default(),
        adapter,
        listeners: Vec::new(),
        stores: crate::stores::StoresConfig::default(),
        // Dependency list, NOT a permission grant: see this function's doc.
        skills: crate::agents::SkillsConfig::default(),
        permissions: crate::agents::PermissionsConfig::default(),
        subagents: crate::agents::SubagentsConfig::default(),
    }
}

#[cfg(test)]
#[path = "mpm_bridge_tests.rs"]
mod tests;
