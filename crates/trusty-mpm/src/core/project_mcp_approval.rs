//! Report Claude Code's OWN approval state for a project's `.mcp.json` (#7892).
//!
//! Why: the owner ruling hands `<cwd>/.mcp.json` back to Claude Code's native
//! approval flow — `enableAllProjectMcpServers`, `enabledMcpjsonServers` /
//! `disabledMcpjsonServers`, or the interactive prompt. tm neither pre-approves
//! nor suppresses those entries any more, so `tm mcp list` and `tm doctor` owe
//! an operator a READ of that state rather than a tm-specific verdict: "this
//! repo declares four servers; Claude Code has approved two, refused one, and
//! will ask about the fourth."
//!
//! What: [`project_mcp_state`] reads the declared names out of `.mcp.json` and
//! resolves each against the settings tiers Claude Code itself consults, most
//! specific first. Read-only and fail-quiet — a missing or malformed file
//! yields no entries rather than an error, because this surface is a
//! diagnostic and must never be the thing that fails a launch.
//!
//! ONE CAVEAT WORTH SAYING PLAINLY: tm's non-interactive panes run
//! `--dangerously-skip-permissions`, and a non-interactive Claude Code session
//! cannot show the `.mcp.json` prompt at all — for those sessions
//! [`Approval::Prompt`] means "connects without asking", not "waits". That is
//! the same posture every `claude -p` / Agent SDK / CI run already has.
//! Test: `project_mcp_approval_tests.rs`.

use std::path::Path;

use serde_json::Value;

/// What Claude Code will do with one `.mcp.json` entry.
///
/// Why: three states, not two — "not yet decided" is the common one on a fresh
/// clone and reads very differently from a refusal.
/// What: `Enabled` from `enableAllProjectMcpServers` or an `enabled` list;
/// `Disabled` from a `disabled` list; `Prompt` when no tier names it.
/// Test: `approval_prefers_the_project_tier`, `approval_defaults_to_prompt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// Approved — the session connects it.
    Enabled,
    /// Refused — the session does not connect it.
    Disabled,
    /// Undecided: Claude Code asks, or connects unasked in a headless pane.
    Prompt,
}

impl Approval {
    /// The one-word label `tm mcp list` and `tm doctor` print.
    ///
    /// Why: both surfaces render the same column, so the wording lives once.
    /// Test: `approval_defaults_to_prompt`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Enabled => "approved",
            Self::Disabled => "refused",
            Self::Prompt => "unapproved",
        }
    }
}

/// One `.mcp.json` entry and the state Claude Code holds for it.
///
/// Test: `project_mcp_state_reports_each_declared_server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMcpServer {
    /// The name as `.mcp.json` declares it.
    pub name: String,
    /// What Claude Code will do with it.
    pub approval: Approval,
}

/// Read `<cwd>/.mcp.json` and resolve each entry's Claude Code approval.
///
/// Why: see the module doc — the one place either diagnostic surface learns
/// what the project half of the session will actually load.
/// What: an empty vector when the project declares nothing (or the file will
/// not parse); otherwise one entry per declared name, sorted, each resolved
/// against `<cwd>/.claude/settings.local.json`, `<cwd>/.claude/settings.json`
/// and `<config_dir>/settings.json` in that order, then against the per-project
/// record Claude Code keeps in `<config_dir>/.claude.json`.
/// Test: `project_mcp_state_reports_each_declared_server`,
/// `project_mcp_state_is_empty_without_an_mcp_json`,
/// `project_mcp_state_honours_enable_all`.
pub fn project_mcp_state(cwd: &Path, config_dir: &Path) -> Vec<ProjectMcpServer> {
    let mut names = match read_json(&cwd.join(crate::core::mcp_config::MCP_JSON)) {
        Some(v) => v
            .get("mcpServers")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    names.sort();
    if names.is_empty() {
        return Vec::new();
    }
    let tiers = approval_tiers(cwd, config_dir);
    names
        .into_iter()
        .map(|name| ProjectMcpServer {
            approval: resolve(&name, &tiers),
            name,
        })
        .collect()
}

/// The settings objects Claude Code consults, most specific first.
///
/// Why: the precedence is Claude Code's, not tm's, so it is expressed as an
/// ordered list of whole objects and the first tier that NAMES a server wins —
/// rather than as a merged map, which would lose which tier decided.
/// What: project `settings.local.json`, project `settings.json`, the managed
/// `settings.json`, then the `projects.<cwd>` entry of the managed
/// `.claude.json` where Claude Code records prompt answers.
/// Test: `approval_prefers_the_project_tier`.
fn approval_tiers(cwd: &Path, config_dir: &Path) -> Vec<Value> {
    let mut tiers: Vec<Value> = [
        cwd.join(".claude").join("settings.local.json"),
        cwd.join(".claude").join("settings.json"),
        config_dir.join("settings.json"),
    ]
    .iter()
    .filter_map(|p| read_json(p))
    .collect();
    if let Some(record) = read_json(&config_dir.join(".claude.json"))
        .and_then(|v| v.get("projects").cloned())
        .and_then(|p| p.get(cwd.to_string_lossy().as_ref()).cloned())
    {
        tiers.push(record);
    }
    tiers
}

/// Resolve one name against the ordered tiers.
///
/// Why: `enableAllProjectMcpServers` is a blanket yes, but a tier that names
/// the server explicitly is more specific than a blanket flag in the SAME tier,
/// so the disabled/enabled lists are checked first within each tier.
/// Test: `approval_prefers_the_project_tier`, `project_mcp_state_honours_enable_all`.
fn resolve(name: &str, tiers: &[Value]) -> Approval {
    for tier in tiers {
        if names_server(tier, "disabledMcpjsonServers", name) {
            return Approval::Disabled;
        }
        if names_server(tier, "enabledMcpjsonServers", name) {
            return Approval::Enabled;
        }
        if tier
            .get("enableAllProjectMcpServers")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Approval::Enabled;
        }
    }
    Approval::Prompt
}

/// Does `tier`'s `key` array contain `name`?
fn names_server(tier: &Value, key: &str, name: &str) -> bool {
    tier.get(key)
        .and_then(Value::as_array)
        .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(name)))
}

/// Parse one JSON file, or `None` for anything that will not read or parse.
///
/// Why: every read here is diagnostic, so a broken file must degrade to "no
/// information" rather than to an error the caller has to handle.
/// Test: `project_mcp_state_is_empty_without_an_mcp_json`.
fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

#[cfg(test)]
#[path = "project_mcp_approval_tests.rs"]
mod tests;
