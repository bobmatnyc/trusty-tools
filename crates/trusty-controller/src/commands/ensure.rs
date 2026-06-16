//! `tctl ensure [--wait]` — idempotent per-project auto-config pass (DOC-8 §2).
//!
//! Why: The explicit manual entry point for the ensure-project pass. Its
//! load-bearing, idempotent core is patching the project's `.mcp.json` so Claude
//! Code can reach the trusty MCP servers (search, memory, review, mpm). Running
//! it twice must not duplicate entries or rewrite the file when nothing changed
//! — exactly the contract `trusty_common::claude_config::patch_mcp_server`
//! provides.
//!
//! What: for each MCP-serving member, upserts its `mcpServers.<key>` entry into
//! `./.mcp.json` via `patch_mcp_server` (idempotent), reports per-member whether
//! it was added/unchanged, and renders human or `--json`. Returns 0 on success.
//!
//! Scope note (#1332): the `.mcp.json` patch is implemented here in full. The
//! search-index registration, memory-palace creation, and `--wait` readiness
//! polling described in DOC-8 §2 require live-daemon HTTP coupling and design
//! decisions not yet settled; `--wait` is accepted and reported as "deferred"
//! rather than silently ignored. This keeps `ensure` doing real, idempotent work
//! today without fabricating a daemon-coupled subsystem.
//!
//! Exit codes: `0` = patched OK (and `--wait` was NOT requested); `2` = a patch
//! failed; `4` = the `.mcp.json` patch succeeded but `--wait` was requested and
//! the readiness-wait is deferred/not-implemented. The distinct `4` lets CI
//! callers detect they did NOT actually block on readiness, instead of being
//! lulled by a `0` they would read as "waited and ready".
//!
//! Test: `tests` covers the MCP-entry table (`mcp_members`) and the report
//! shaping; the actual file patch is side-effecting (covered by
//! `claude_config::patch_mcp_server`'s own tests).

use std::path::Path;

use serde::Serialize;
use trusty_common::claude_config::{mcp_server_entry, patch_mcp_server};

use crate::output::render_json;

/// The project-local Claude Code MCP config file name.
///
/// Why: `ensure` patches the per-project `.mcp.json` in the current working
/// directory (Claude Code discovers it there).
/// What: the literal `.mcp.json`.
/// Test: used by `run`; the path join is trivial.
const MCP_FILE: &str = ".mcp.json";

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

/// One member's ensure outcome.
///
/// Why: a typed per-member record keeps the `--json` report stable + testable.
/// What: `member` key; `changed` whether the entry was added/updated; `detail`
/// a human note (or the error on failure); `ok` whether the patch succeeded.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct EnsureOutcome {
    /// `mcpServers` key patched.
    pub member: String,
    /// Whether the patch succeeded.
    pub ok: bool,
    /// Whether the entry was actually changed (`false` = already current).
    pub changed: bool,
    /// Human detail / error.
    pub detail: String,
}

/// The aggregate ensure report.
///
/// Why: `--json` consumers want the rollup in one object.
/// What: holds the per-member outcomes, `all_ok`, and whether `--wait` was
/// requested-but-deferred.
/// Test: `tests::report_serialises`, `tests::report_all_ok`.
#[derive(Clone, Debug, Serialize)]
pub struct EnsureReport {
    /// Fixed command tag.
    pub command: &'static str,
    /// Per-member outcomes.
    pub members: Vec<EnsureOutcome>,
    /// Whether every patch succeeded.
    pub all_ok: bool,
    /// Whether `--wait` was requested (currently deferred; see module note).
    pub wait_deferred: bool,
}

impl EnsureReport {
    /// Build the report and derive `all_ok`.
    ///
    /// Why: one place derives the verdict so JSON + exit code agree.
    /// What: `all_ok = every outcome ok`.
    /// Test: `tests::report_all_ok`.
    fn build(members: Vec<EnsureOutcome>, wait_deferred: bool) -> Self {
        let all_ok = members.iter().all(|m| m.ok);
        Self {
            command: "ensure",
            members,
            all_ok,
            wait_deferred,
        }
    }

    /// Process exit code: 2 if any patch failed, 4 if patched OK but `--wait`
    /// was requested-but-deferred, else 0.
    ///
    /// Why: CI / setup scripts branch on this. A failed patch (`2`) must take
    /// precedence over the deferred-wait signal (`4`) — a broken `.mcp.json` is
    /// the more serious condition. `4` is distinct from `0` so a caller that
    /// passed `--wait` can detect it did NOT actually block on readiness.
    /// What: `2` when any patch failed; else `4` when `wait_deferred`; else `0`.
    /// Test: `tests::report_all_ok`, `tests::wait_deferred_exit_code`.
    fn exit_code(&self) -> i32 {
        if !self.all_ok {
            2
        } else if self.wait_deferred {
            4
        } else {
            0
        }
    }
}

/// Patch one member's `.mcp.json` entry, returning its outcome.
///
/// Why: isolating the single-member upsert keeps `run` a thin loop and lets the
/// idempotent primitive (`patch_mcp_server`) own the change detection.
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

/// Handle `tctl ensure [--wait]`.
///
/// Why: Phase-2 entry point — idempotently wire the project's `.mcp.json`.
/// What: upserts every MCP-serving member's entry into `./.mcp.json`, renders the
/// report, and returns the exit code. `--wait` is accepted but its readiness
/// polling is deferred: when `--wait` is requested the patch is still applied,
/// but the verb returns exit `4` (not `0`) so callers know they did NOT block on
/// readiness (see module note).
/// Test: side-effecting (filesystem); the table + report are unit-tested.
pub fn run(wait: bool, json: bool) -> i32 {
    let path = Path::new(MCP_FILE);
    let outcomes: Vec<EnsureOutcome> = mcp_members()
        .iter()
        .map(|m| ensure_member(path, m))
        .collect();
    let report = EnsureReport::build(outcomes, wait);

    if json {
        if render_json(&report).is_err() {
            eprintln!("tctl ensure: failed to write JSON output");
            return 1;
        }
    } else {
        println!("tctl ensure — {MCP_FILE} MCP servers");
        for m in &report.members {
            let mark = if m.ok { "ok" } else { "FAILED" };
            println!("  {:<18} {:<8} {}", m.member, mark, m.detail);
        }
        if report.wait_deferred {
            eprintln!(
                "tctl ensure: --wait is not yet wired (index/palace readiness \
                 polling is deferred); the .mcp.json patch above completed."
            );
        }
    }
    report.exit_code()
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

    /// Why: the JSON envelope is a contract; pin its shape including
    /// `wait_deferred`.
    /// What: builds a report and asserts keys.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let r = EnsureReport::build(
            vec![EnsureOutcome {
                member: "trusty-search".to_owned(),
                ok: true,
                changed: true,
                detail: "added/updated".to_owned(),
            }],
            true,
        );
        let v = serde_json::to_value(&r).expect("serialises");
        assert_eq!(v["command"], "ensure");
        assert_eq!(v["all_ok"], true);
        assert_eq!(v["wait_deferred"], true);
        assert_eq!(v["members"][0]["changed"], true);
    }

    /// Why: `all_ok` must be false if any patch failed; exit code tracks it.
    /// What: mixes ok + failed; asserts `all_ok = false` and exit 2.
    /// Test: This is the test.
    #[test]
    fn report_all_ok() {
        let bad = EnsureReport::build(
            vec![
                EnsureOutcome {
                    member: "a".to_owned(),
                    ok: true,
                    changed: false,
                    detail: "already current".to_owned(),
                },
                EnsureOutcome {
                    member: "b".to_owned(),
                    ok: false,
                    changed: false,
                    detail: "permission denied".to_owned(),
                },
            ],
            false,
        );
        assert!(!bad.all_ok);
        assert_eq!(bad.exit_code(), 2);

        let ok = EnsureReport::build(
            vec![EnsureOutcome {
                member: "a".to_owned(),
                ok: true,
                changed: false,
                detail: "already current".to_owned(),
            }],
            false,
        );
        assert!(ok.all_ok);
        assert_eq!(ok.exit_code(), 0);
    }

    /// Why: when `--wait` was requested but readiness-wait is deferred, the verb
    /// must return the distinct code `4` (not `0`) so CI callers know they did
    /// NOT actually block on readiness — while still reporting `all_ok`.
    /// What: all-ok + `wait_deferred = true` → exit 4; a failed patch still wins
    /// with exit 2 even when `wait_deferred` is set.
    /// Test: This is the test.
    #[test]
    fn wait_deferred_exit_code() {
        // Patched OK but --wait deferred → distinct exit 4.
        let deferred = EnsureReport::build(
            vec![EnsureOutcome {
                member: "a".to_owned(),
                ok: true,
                changed: true,
                detail: "added/updated".to_owned(),
            }],
            true,
        );
        assert!(deferred.all_ok);
        assert_eq!(deferred.exit_code(), 4);

        // A failed patch outranks the deferred-wait signal → exit 2.
        let failed_and_deferred = EnsureReport::build(
            vec![EnsureOutcome {
                member: "a".to_owned(),
                ok: false,
                changed: false,
                detail: "permission denied".to_owned(),
            }],
            true,
        );
        assert!(!failed_and_deferred.all_ok);
        assert_eq!(failed_and_deferred.exit_code(), 2);
    }
}
