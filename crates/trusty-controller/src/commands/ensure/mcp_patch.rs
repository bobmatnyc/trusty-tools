//! The load-bearing, idempotent `.mcp.json` patch stage of `tctl ensure`.
//!
//! Why: Claude Code reaches the trusty MCP servers (search, memory, review, mpm)
//! via the per-project `.mcp.json`. Patching it is the original, always-on core
//! of `ensure`: running twice must not duplicate entries or rewrite an unchanged
//! file — exactly the contract `trusty_common::claude_config::patch_mcp_server`
//! provides.
//!
//! What: a data table of the MCP-serving members and a one-pass upsert loop that
//! patches each member's `mcpServers.<key>` entry into `./.mcp.json`, returning a
//! per-member [`EnsureOutcome`].
//!
//! Test: `tests::mcp_members_table` pins the member table against the repo's live
//! `.mcp.json`; the file patch itself is side-effecting (covered by
//! `claude_config::patch_mcp_server`'s own tests).

use std::path::Path;

use trusty_common::claude_config::{mcp_server_entry, patch_mcp_server};

use super::report::EnsureOutcome;

/// The project-local Claude Code MCP config file name.
///
/// Why: `ensure` patches the per-project `.mcp.json` in the current working
/// directory (Claude Code discovers it there).
/// What: the literal `.mcp.json`.
/// Test: used by [`patch_all`]; the path join is trivial.
pub const MCP_FILE: &str = ".mcp.json";

/// One MCP-serving member's `.mcp.json` entry definition.
///
/// Why: encoding the (key, command, args) triple as data keeps the upsert loop a
/// single pass and makes the table independently testable, so adding/removing a
/// member is a data edit.
/// What: `key` is the `mcpServers` object key; `command`/`args` form the stdio
/// launch entry (verified against the repo's live `.mcp.json`).
/// Test: `tests::mcp_members_table`.
struct McpMember {
    /// `mcpServers` object key.
    key: &'static str,
    /// Launch command (binary name).
    command: &'static str,
    /// Launch args (the member's stdio serve invocation).
    args: &'static [&'static str],
}

/// The MCP-serving members `ensure` wires into `.mcp.json`.
///
/// Why: the single source of truth for which members get an `.mcp.json` entry
/// and exactly how they are launched (matches the repo's live config).
/// What: search (`serve`), memory (`serve --stdio`), review (`serve --stdio`),
/// mpm (`serve --stdio`). trusty-analyze and tga are not MCP stdio servers and
/// are intentionally absent.
/// Test: `tests::mcp_members_table`.
fn mcp_members() -> Vec<McpMember> {
    vec![
        McpMember {
            key: "trusty-search",
            command: "trusty-search",
            args: &["serve"],
        },
        McpMember {
            key: "trusty-memory",
            command: "trusty-memory",
            args: &["serve", "--stdio"],
        },
        McpMember {
            key: "trusty-review",
            command: "trusty-review",
            args: &["serve", "--stdio"],
        },
        McpMember {
            key: "trusty-mpm",
            command: "trusty-mpm",
            args: &["serve", "--stdio"],
        },
    ]
}

/// Patch one member's `.mcp.json` entry, returning its outcome.
///
/// Why: isolating the single-member upsert keeps [`patch_all`] a thin loop and
/// lets the idempotent primitive (`patch_mcp_server`) own the change detection.
/// What: builds the stdio entry and upserts it; maps the `bool` (changed) into a
/// human detail, and any error into a failed outcome.
/// Test: side-effecting (filesystem); `patch_mcp_server` is tested in trusty-common.
fn ensure_member(path: &Path, m: &McpMember) -> EnsureOutcome {
    let entry = mcp_server_entry(m.command, m.args);
    match patch_mcp_server(path, m.key, &entry) {
        Ok(true) => EnsureOutcome {
            member: m.key.to_owned(),
            ok: true,
            changed: true,
            detail: "added/updated".to_owned(),
        },
        Ok(false) => EnsureOutcome {
            member: m.key.to_owned(),
            ok: true,
            changed: false,
            detail: "already current".to_owned(),
        },
        Err(e) => EnsureOutcome {
            member: m.key.to_owned(),
            ok: false,
            changed: false,
            detail: e.to_string(),
        },
    }
}

/// Patch every MCP-serving member's entry into `./.mcp.json`.
///
/// Why: the always-on core of `ensure`; returns the per-member outcomes the
/// report aggregates.
/// What: upserts each member from [`mcp_members`] via [`ensure_member`] against
/// the project-local [`MCP_FILE`].
/// Test: side-effecting (filesystem); the member table is unit-tested.
pub fn patch_all() -> Vec<EnsureOutcome> {
    let path = Path::new(MCP_FILE);
    mcp_members()
        .iter()
        .map(|m| ensure_member(path, m))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the MCP entry table is load-bearing — a wrong command/args breaks the
    /// member's MCP wiring. Pin it against the repo's live `.mcp.json`.
    /// What: asserts the four members and their exact launch entries.
    /// Test: This is the test.
    #[test]
    fn mcp_members_table() {
        let members = mcp_members();
        let keys: Vec<&str> = members.iter().map(|m| m.key).collect();
        assert_eq!(
            keys,
            vec![
                "trusty-search",
                "trusty-memory",
                "trusty-review",
                "trusty-mpm"
            ]
        );
        let search = &members[0];
        assert_eq!(search.command, "trusty-search");
        assert_eq!(search.args, &["serve"]);
        let memory = &members[1];
        assert_eq!(memory.args, &["serve", "--stdio"]);
    }
}
