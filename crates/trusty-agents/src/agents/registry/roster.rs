//! PM-prompt roster rendering from the live `AgentRegistry`.
//!
//! Why: The PM's system prompt used to hardcode a single agent
//! (`python-engineer`), causing the LLM to over-delegate there regardless of
//! task content. Rendering the live registry ensures the PM sees every
//! user-installed and bundled agent at runtime, so new agents drop in without
//! prompt edits.
//! What: `build_roster_section` renders a deterministic Markdown bullet list;
//! `inject_roster_into_prompt` substitutes it for the `{{available_agents}}`
//! placeholder (or appends a new section when the placeholder is absent).
//! Test: `build_roster_section_includes_all_registered_agents`,
//! `inject_roster_replaces_placeholder`.

use super::AgentRegistry;

/// Render a Markdown roster section describing every agent in `registry`.
///
/// Why: The PM's system prompt used to hardcode a single agent
/// (`python-engineer`), causing the LLM to over-delegate there regardless of
/// task content. Injecting the live registry ensures the PM sees every
/// user-installed and bundled agent (including `.trusty-agents/agents/`,
/// `.claude/agents/`, and home-dir overrides) at runtime, so new agents drop
/// in without prompt edits.
/// What: Produces a deterministic bullet list, one agent per line, with
/// name + description + role/language/framework/tag annotations (only when
/// present). Empty lists and blank descriptions are elided so the prompt
/// stays compact. Intended to be substituted for the `{{available_agents}}`
/// placeholder in `pm.toml`.
/// Test: `build_roster_section_includes_all_registered_agents`,
/// `build_roster_section_omits_empty_capability_fields`.
pub fn build_roster_section(registry: &AgentRegistry) -> String {
    let mut lines = Vec::new();
    for summary in registry.list() {
        // Skip the PM itself from the roster — PM does not delegate to itself.
        if summary.name == "pm" {
            continue;
        }
        let mut line = format!("- **{}**", summary.name);
        let desc = summary.description.trim();
        if !desc.is_empty() {
            line.push_str(&format!(": {desc}"));
        }
        let mut parts: Vec<String> = Vec::new();
        if !summary.roles.is_empty() {
            parts.push(format!("role: {}", summary.roles.join("/")));
        }
        if !summary.languages.is_empty() {
            parts.push(format!("languages: {}", summary.languages.join(", ")));
        }
        if !summary.frameworks.is_empty() {
            parts.push(format!("frameworks: {}", summary.frameworks.join(", ")));
        }
        if !summary.tags.is_empty() {
            parts.push(format!("tags: {}", summary.tags.join(", ")));
        }
        if !parts.is_empty() {
            line.push_str(&format!("  ({})", parts.join("; ")));
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Inject the dynamic agent roster into a PM system prompt.
///
/// Why: Centralizes the substitution logic so both `run_pm()` and
/// `run_inspect_live()` produce identical PM prompts — divergence here would
/// silently alter Layer 2 harness results vs. production behavior.
/// What: If `{{available_agents}}` appears in `prompt`, replaces it with
/// the rendered roster. Otherwise appends a new `## Available Agents`
/// section so a pm.toml that pre-dates the placeholder still gets the
/// dynamic roster rather than silently missing it.
/// Test: `inject_roster_replaces_placeholder`,
/// `inject_roster_appends_when_placeholder_missing`.
pub fn inject_roster_into_prompt(prompt: &str, registry: &AgentRegistry) -> String {
    let roster = build_roster_section(registry);
    if prompt.contains("{{available_agents}}") {
        prompt.replace("{{available_agents}}", &roster)
    } else {
        format!("{prompt}\n\n## Available Agents\n{roster}")
    }
}
