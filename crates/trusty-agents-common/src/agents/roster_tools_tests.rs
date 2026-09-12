//! The deployed-roster `tools:` conformance pin (#7683).
//!
//! Why: a dispatched subagent pays for every tool schema its allowlist admits,
//! and an MCP server it never calls is the most expensive kind — a measured
//! subagent turn carried 151 deferred MCP tool names plus their schemas. The
//! allowlist is also what suppresses the skills listing: a probe run on
//! 2026-09-12 showed an agent whose `tools:` omits `Skill` sees no skills
//! listing at all (+1.7K tokens for a two-tool agent versus +51.8K for
//! general-purpose). Neither saving is visible in the source `.md`; it is the
//! DEPLOYED file Claude Code reads, so the assertion has to run against a real
//! deploy, not against the asset string.
//!
//! What: writes the whole embedded roster to a temp source directory, runs the
//! real [`deploy_agents`] pipeline into a temp target, and asserts each
//! deployed file's `tools:` line is exactly [`EXPECTED_TOOLS`]. The table is
//! the specification — changing an agent's allowlist means changing this row,
//! which is what keeps a widening deliberate. Per
//! `docs/specs/agent-context-minimization.md` §C.

use std::collections::BTreeSet;
use std::fs;

use crate::agent_assets::AGENT_ASSETS;
use crate::agents::deployer::deploy_agents;

/// Built-ins every agent gets: read the tree, and nothing that writes it.
const READ_ONLY: &str = "Read, Bash, BashOutput, KillShell, Grep, Glob";

/// Built-ins for an agent that edits the tree.
const READ_WRITE: &str = "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob";

/// The per-agent Claude Code allowlist, exactly as the deployed file carries it.
///
/// Why: this is the table `docs/specs/agent-context-minimization.md` §C
/// specifies, pinned so a roster edit cannot silently restore the implicit
/// all-tools default (which is what an absent `tools:` key means).
/// What: `(agent file stem, expected `tools:` value)` for every dispatchable
/// roster agent. The five `BASE-*` templates are deliberately absent — see
/// `base_templates_declare_no_tools`.
///
/// An agent whose own body mandates authoring a file carries the write tool
/// that step needs, even when the rest of its role is read-only: `research`
/// takes `Write, Edit` for the "Capture Work" step that saves to
/// `docs/research/`, and `code-analyzer` takes `Write` for the
/// `scripts/code-review/` script its Large-Volume Analysis section generates.
const EXPECTED_TOOLS: &[(&str, &str)] = &[
    (
        "api-qa",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "code-analyzer",
        "Read, Write, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-memory, mcp__trusty-search",
    ),
    (
        "code-critic",
        "Read, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-review, mcp__trusty-search",
    ),
    (
        "dart-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "data-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "documentation",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "dotnet-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "elixir-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    ("gcp-ops", READ_WRITE),
    (
        "golang-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "java-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "javascript-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "local-ops",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, Skill, mcp__trusty-mpm",
    ),
    ("memory-manager", "Read, Grep, mcp__trusty-memory"),
    (
        "mpm-agent-manager",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-mpm",
    ),
    (
        "mpm-skills-manager",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, Skill, mcp__trusty-mpm",
    ),
    (
        "nextjs-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "phoenix-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "php-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "prompt-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "python-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "qa",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "react-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "refactoring-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "research",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, WebFetch, WebSearch, mcp__trusty-memory, mcp__trusty-search",
    ),
    (
        "ruby-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "rust-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, Skill, mcp__trusty-search",
    ),
    ("secrets-manager", READ_ONLY),
    (
        "security",
        "Read, Bash, BashOutput, KillShell, Grep, Glob, WebFetch, WebSearch, mcp__trusty-search",
    ),
    (
        "svelte-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    (
        "tauri-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    ("ticketing", READ_ONLY),
    (
        "typescript-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
    ("vercel-ops", READ_WRITE),
    (
        "version-control",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, Skill, mcp__trusty-review",
    ),
    (
        "web-qa",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__claude-in-chrome, mcp__trusty-search",
    ),
    (
        "web-ui-engineer",
        "Read, Write, Edit, Bash, BashOutput, KillShell, Grep, Glob, mcp__trusty-search",
    ),
];

/// Deploy the whole embedded roster into a temp target and return its path.
///
/// Why: the assertion has to read what Claude Code reads. Composing in memory
/// would skip the strict-YAML validation the deployer runs, which is exactly
/// the gate a malformed `tools:` line would trip.
/// What: writes every [`AGENT_ASSETS`] entry to a temp source dir, runs
/// [`deploy_agents`], and asserts nothing failed to compose.
fn deploy_roster() -> (tempfile::TempDir, tempfile::TempDir) {
    let src = tempfile::tempdir().expect("source tempdir");
    let tgt = tempfile::tempdir().expect("target tempdir");
    for (file_name, contents) in AGENT_ASSETS {
        fs::write(src.path().join(file_name), contents).expect("write roster asset");
    }
    let result = deploy_agents(src.path(), tgt.path()).expect("roster deploys");
    assert!(
        result.failed.is_empty(),
        "every roster agent must compose and validate; failed: {:?}",
        result.failed
    );
    (src, tgt)
}

/// Read the deployed `tools:` value for one agent, or `None` when absent.
fn deployed_tools(target: &std::path::Path, stem: &str) -> Option<String> {
    let raw = fs::read_to_string(target.join(format!("{stem}.md")))
        .unwrap_or_else(|e| panic!("deployed agent `{stem}.md` must exist: {e}"));
    raw.lines()
        .find_map(|line| line.strip_prefix("tools: ["))
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::to_string)
}

/// Every dispatchable roster agent deploys with exactly its table row.
///
/// Why: an absent or drifted `tools:` key silently restores the all-tools
/// default — the whole MCP schema set and the whole skills listing — with no
/// other signal that it happened.
/// What: deploys the roster and compares each file's `tools:` line to
/// [`EXPECTED_TOOLS`].
/// Test: this test.
#[test]
fn every_roster_agent_deploys_with_its_declared_tools() {
    let (_src, tgt) = deploy_roster();
    let mut wrong = Vec::new();
    for (stem, expected) in EXPECTED_TOOLS {
        match deployed_tools(tgt.path(), stem) {
            Some(actual) if actual == *expected => {}
            other => wrong.push(format!(
                "  {stem}\n    expected: {expected}\n    actual:   {}",
                other.unwrap_or_else(|| "<no `tools:` key — all tools allowed>".to_string())
            )),
        }
    }
    assert!(
        wrong.is_empty(),
        "{} deployed agent(s) carry the wrong `tools:` allowlist:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// The table covers the dispatchable roster exactly — no agent left implicit.
///
/// Why: a new agent added to the roster with no table row would deploy with
/// the all-tools default and nothing would say so. This is the gate that makes
/// adding a row mandatory.
/// What: compares the table's key set to `AGENT_ASSETS` minus the `BASE-*`
/// templates.
/// Test: this test.
#[test]
fn tools_table_covers_every_dispatchable_roster_agent() {
    let dispatchable: BTreeSet<String> = AGENT_ASSETS
        .iter()
        .map(|(file_name, _)| file_name.trim_end_matches(".md").to_string())
        .filter(|stem| !stem.starts_with("BASE-"))
        .collect();
    let tabled: BTreeSet<String> = EXPECTED_TOOLS
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect();
    assert_eq!(
        dispatchable, tabled,
        "every dispatchable roster agent needs exactly one EXPECTED_TOOLS row"
    );
}

/// The `BASE-*` templates declare no `tools:` key.
///
/// Why: `tools:` merges by OVERRIDE, so a value on a base template would be
/// inherited by any future agent that omits the key — silently granting a set
/// nobody chose for it. The templates stay neutral; every leaf declares.
/// What: asserts no deployed `BASE-*.md` carries a `tools:` line.
/// Test: this test.
#[test]
fn base_templates_declare_no_tools() {
    let (_src, tgt) = deploy_roster();
    for (file_name, _) in AGENT_ASSETS {
        let stem = file_name.trim_end_matches(".md");
        if !stem.starts_with("BASE-") {
            continue;
        }
        assert_eq!(
            deployed_tools(tgt.path(), stem),
            None,
            "`{stem}` must leave `tools:` unset so it cannot override a leaf's"
        );
    }
}

/// No roster agent declares trusty-code's `tcode_tools:` key.
///
/// Why: the two keys name two disjoint tool vocabularies (#7683). A roster
/// agent carrying both would deploy one allowlist to Claude Code and hand
/// trusty-code a second, and the pair would drift with nothing comparing them.
/// What: asserts `tcode_tools:` appears in no deployed roster file.
/// Test: this test.
#[test]
fn roster_agents_declare_no_tcode_tools() {
    let (_src, tgt) = deploy_roster();
    for (file_name, _) in AGENT_ASSETS {
        let raw = fs::read_to_string(tgt.path().join(file_name)).expect("deployed file");
        assert!(
            !raw.contains("tcode_tools:"),
            "`{file_name}` must not declare `tcode_tools:` — that key is \
             trusty-code's own vocabulary, and this roster deploys to Claude Code"
        );
    }
}
