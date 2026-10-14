//! The never-deny floors: MCP servers (#8988, Architect ruling Q2), and the
//! tools and skills an owner file can never subtract (#8992).
//!
//! Why: an owner `deny` reaches every subagent in the session, so a server an
//! agent's `tools:` line grants must never be deniable. tm's own builtins also
//! sit in the operator's user scope, so `allow = []` would otherwise deny them.
//! #8992: the same holds for a built-in tool an agent or the PM prompt uses,
//! and for a skill an agent preloads or a prompt loads by name.
//! What: [`is_floor`] answers for [`BUILTIN_MANAGED_MCP_SERVERS`] plus
//! [`EXTRA_FLOOR`]; [`is_tool_floor`] for [`TOOL_FLOOR`] and any
//! `mcp__<floor server>[__…]` rule; [`is_skill_floor`] for every
//! [`SKILL_FLOOR_PREFIX`] skill plus [`SKILL_FLOOR_EXACT`]. Compiled in; no
//! file can change them.
//! Test: `floor_tests.rs` — one drift test per source each floor must cover.

use crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS;

/// Floor servers beyond tm's builtins.
///
/// Why: `content/agents/web-qa.md` grants `mcp__claude-in-chrome`, and the PM
/// routes browser work to web-qa.
/// What: `["claude-in-chrome"]`.
/// Test: `floor_covers_every_agent_tools_mcp_grant`.
pub const EXTRA_FLOOR: &[&str] = &["claude-in-chrome"];

/// Whether `name` is a never-deny MCP server.
///
/// Test: `floor_holds_the_builtins_and_chrome`.
pub fn is_floor(name: &str) -> bool {
    BUILTIN_MANAGED_MCP_SERVERS.contains(&name) || EXTRA_FLOOR.contains(&name)
}

/// Built-in tools an owner `[tools] deny` can never name (#8992).
///
/// Why: a `permissions.deny` from `--settings` may reach subagents, so the
/// floor is written for that worst case: every built-in an agent `tools:` line
/// grants, every tool the PM prompt delegates or plans with, `TaskStop`, and
/// the `Cron*` tools session resume schedules through (`core/pm_tool_trim.rs`).
/// What: exact Claude Code tool names.
/// Test: `tool_floor_covers_every_agent_tools_builtin_grant`,
/// `every_tool_floor_name_is_used_by_a_source`,
/// `every_subagent_dispatch_and_stop_tool_is_in_the_tool_floor`.
pub const TOOL_FLOOR: &[&str] = &[
    "Agent",
    "Task",
    "SendMessage",
    // #8992: `TaskStop` cancels a subagent; its PostToolUse releases the tree claim.
    "TaskStop",
    "Skill",
    "ToolSearch",
    "TodoWrite",
    "Read",
    "Write",
    "Edit",
    "Bash",
    "BashOutput",
    "KillShell",
    "Grep",
    "Glob",
    "WebSearch",
    "WebFetch",
    "CronCreate",
    "CronDelete",
    "CronList",
];

/// Whether `rule` names a floor tool: a [`TOOL_FLOOR`] name, or an
/// `mcp__<server>` / `mcp__<server>__<tool>` rule whose server [`is_floor`].
///
/// What: compares case-insensitively, so a differently-cased spelling can only
/// make a file bad, never deny a floor tool by a matching rule.
/// Test: `the_tool_floor_holds_builtins_and_floor_servers`.
pub fn is_tool_floor(rule: &str) -> bool {
    if let Some(rest) = rule.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or(rest);
        return BUILTIN_MANAGED_MCP_SERVERS
            .iter()
            .chain(EXTRA_FLOOR)
            .any(|f| f.eq_ignore_ascii_case(server));
    }
    TOOL_FLOOR.iter().any(|t| t.eq_ignore_ascii_case(rule))
}

/// Every skill whose name starts with this is in the skill floor (#8992).
///
/// Why: the `tm-*` skills are the harness's own — the PM prompt loads them by
/// name and the Architect seeds `tm-fleet-check` / `tm-context-refresh`.
pub const SKILL_FLOOR_PREFIX: &str = "tm-";

/// Skills outside [`SKILL_FLOOR_PREFIX`] an owner `[skills] off` can never name.
///
/// Why: an agent preloads every skill on its frontmatter `skills:` line, and a
/// skill turned off may not preload (spike gap 4); the prompts also load some
/// by name with `Skill(skill="…")`.
/// What: exact skill names.
/// Test: `skill_floor_covers_every_agent_skills_grant`,
/// `skill_floor_covers_every_skill_the_prompts_load`,
/// `every_exact_skill_floor_name_has_a_source`.
pub const SKILL_FLOOR_EXACT: &[&str] = &[
    "cargo-publish",
    "git-workflow",
    "code-review-standards",
    "contract-driven-testing",
    "documentation-style",
    "rust-build-performance",
    "rust-delivery-workflow",
    "security-scanning",
    "systematic-debugging",
    "test-driven-development",
    "testing-anti-patterns",
];

/// Whether `name` is a floor skill; case-insensitive, as [`is_tool_floor`].
///
/// #8436 PR C: the Architect's shipped skills (`architect_skills::SKILLS`)
/// are floored too; its operating manual loads each one by name.
/// Test: `the_skill_floor_holds_the_prefix_and_the_exact_names`,
/// `skill_floor_covers_every_skill_the_prompts_load`.
pub fn is_skill_floor(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(SKILL_FLOOR_PREFIX)
        || SKILL_FLOOR_EXACT.contains(&lower.as_str())
        || crate::core::project_skill_tier::architect_skills::SKILLS
            .iter()
            .any(|skill| skill.stem == lower)
}

#[cfg(test)]
#[path = "floor_tests.rs"]
mod tests;
