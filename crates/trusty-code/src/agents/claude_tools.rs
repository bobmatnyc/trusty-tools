//! Claude Code `tools:` -> trusty-code allowlist translation (#8129).
//!
//! Why: an agent `.md` authored for Claude Code (trusty-mpm's whole roster,
//! and anything a user imports with `tcode paths import`) declares its grant
//! in Claude Code's vocabulary — `Read`, `Bash`, `mcp__trusty-search`. tcode's
//! registry matches allowlist entries by EXACT name, so that vocabulary
//! intersects it at zero tools; #7683 therefore made `project_to_agent_config`
//! ignore `tools:` outright rather than gate such an agent down to nothing.
//! Ignoring it is safe but not correct: an absent allowlist means ALL tools
//! allowed, so a disk agent whose author wrote `tools: [Read]` got write, edit
//! and shell — the opposite of the grant. Translating the vocabulary is what
//! turns a dropped grant back into an enforced one.
//! What: [`claude_tools_to_tcode_allowlist`] maps each recognised Claude Code
//! tool name onto the tcode tools that serve the same capability, drops the
//! ones tcode has no analogue for (`WebFetch`, `WebSearch`, every `mcp__*`
//! server except trusty-search), and always grants `finish_task`, which has no
//! Claude Code spelling and without which an agent cannot return a result at
//! all. [`apply_claude_tools_fallback`] is the single call site: it fills
//! `tools.allowed` ONLY when the document declared no `tcode_tools:` of its
//! own, so an explicit tcode allowlist always wins and the EMBEDDED roster —
//! which never takes this path — keeps its #7683 behaviour unchanged.
//! Test: `tests` in this file, and
//! `tests/paths_import_mpm_catalog_e2e.rs::imported_mpm_agents_parse_with_a_usable_tool_allowlist`.

use trusty_agents_common::agents::metadata::AgentMetadata;

use super::config::{AgentConfig, ToolsConfig};

/// Claude Code tool name -> the trusty-code tools that serve the same
/// capability, in tcode's canonical vocabulary order.
///
/// Why: a table, not a `match`, so the mapping is readable as data and a
/// reviewer can see the whole translation at once.
/// What: unmatched Claude Code names (`WebFetch`, `WebSearch`,
/// `mcp__trusty-memory`, `mcp__trusty-mpm`, `mcp__trusty-review`,
/// `mcp__claude-in-chrome`, `TodoWrite`, ...) are deliberately absent — tcode
/// hosts no equivalent, and inventing one would grant a capability the author
/// never asked for. `BashOutput`/`KillShell` fold into `bash` because tcode's
/// single `bash` tool covers running, reading and killing a command.
/// Test: `every_shared_roster_tool_name_is_translated_or_deliberately_dropped`.
const CLAUDE_TO_TCODE: &[(&str, &[&str])] = &[
    ("Read", &["read_file", "list_dir"]),
    ("Write", &["write_file", "write_files"]),
    ("Edit", &["edit"]),
    ("Grep", &["grep"]),
    ("Glob", &["glob"]),
    ("Bash", &["bash"]),
    ("BashOutput", &["bash"]),
    ("KillShell", &["bash"]),
    ("Skill", &["use_skill"]),
    ("Task", &["delegate_to_agent"]),
    ("mcp__trusty-search", &["search_code"]),
];

/// tcode's canonical tool order, used to make the translated allowlist a
/// function of the SET of granted tools rather than of declaration order.
///
/// Why: two agents granting the same capabilities must produce the same
/// allowlist, so a test can assert on it and a diff of two agents' effective
/// grants is meaningful.
/// What: the registry's tool names in the order tcode's own `.md` assets
/// declare them.
/// Test: `translation_order_is_canonical_not_declaration_order`.
const CANONICAL_ORDER: &[&str] = &[
    "read_file",
    "write_file",
    "write_files",
    "edit",
    "grep",
    "glob",
    "list_dir",
    "bash",
    "search_code",
    "use_skill",
    "delegate_to_agent",
    "finish_task",
];

/// Translate a Claude Code `tools:` grant into a trusty-code tool allowlist.
///
/// Why: see the module docs — this is what makes an imported Claude Code agent
/// carry an enforced grant instead of an ignored one.
/// What: unions the [`CLAUDE_TO_TCODE`] expansion of every recognised entry
/// with `finish_task`, de-duplicates, and emits the result in
/// [`CANONICAL_ORDER`]. An unrecognised name is dropped, not an error: a
/// Claude Code catalog carries tools tcode does not host, and refusing the
/// whole document over one of them would make the import unusable.
/// An EMPTY input yields `["finish_task"]`, preserving the author's deny-all
/// intent while leaving the agent able to return.
/// Test: `translates_the_shared_version_control_grant`,
/// `unknown_tool_names_are_dropped_not_errors`,
/// `empty_grant_still_yields_finish_task`.
pub(crate) fn claude_tools_to_tcode_allowlist(claude: &[String]) -> Vec<String> {
    let mut granted: Vec<&str> = vec!["finish_task"];
    for name in claude {
        if let Some((_, tcode)) = CLAUDE_TO_TCODE
            .iter()
            .find(|(claude_name, _)| claude_name.eq_ignore_ascii_case(name))
        {
            granted.extend(tcode.iter().copied());
        }
    }
    CANONICAL_ORDER
        .iter()
        .filter(|canonical| granted.contains(*canonical))
        .map(|t| (*t).to_string())
        .collect()
}

/// Fill a disk-loaded agent's allowlist from its Claude Code `tools:` grant
/// when it declared no `tcode_tools:` of its own.
///
/// Why: the one place the translation is applied. Keeping it out of
/// `md_loader::project_to_agent_config` is deliberate — that function is
/// shared with the EMBEDDED projection paths, whose #7683 contract (a roster
/// agent's Claude vocabulary is ignored, so it stays unrestricted) this change
/// does not touch.
/// What: no-op when the document declared `tcode_tools:` (including a
/// deliberate `tcode_tools: []` deny-all, which reaches here as `Some(vec![])`
/// and must not be overwritten) or declared no `tools:` either. Otherwise sets
/// `config.tools.allowed` to the translation.
/// Test: `explicit_tcode_tools_wins_over_the_claude_grant`,
/// `deny_all_tcode_tools_is_not_overwritten`,
/// `absent_tools_leaves_the_allowlist_unset`.
pub(crate) fn apply_claude_tools_fallback(config: &mut AgentConfig, meta: &AgentMetadata) {
    if meta.tcode_tools.is_some() {
        return;
    }
    let Some(claude) = meta.tools.as_ref() else {
        return;
    };
    config.tools = Some(ToolsConfig {
        allowed: Some(claude_tools_to_tcode_allowlist(claude)),
    });
}

#[cfg(test)]
#[path = "claude_tools_tests.rs"]
mod tests;
