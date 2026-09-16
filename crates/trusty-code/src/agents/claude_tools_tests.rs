//! Tests for the Claude Code `tools:` -> trusty-code allowlist translation
//! (#8129).
//!
//! Why: split out of `claude_tools.rs` so the production file stays well under
//! the 500-SLOC cap, matching the `describe.rs`/`describe_tests.rs` and
//! `protocol.rs`/`protocol_tests.rs` pairs already in this module.
//! What: covers the translation table, the drop-unknown rule, the
//! `finish_task` floor, and every precedence branch of
//! `apply_claude_tools_fallback`.
//! Test: this file — self-describing.

use super::*;
use crate::agents::config::{AgentConfig, AgentInfo, LlmParams, SystemPrompt};

fn owned(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

fn blank_config() -> AgentConfig {
    AgentConfig {
        agent: AgentInfo {
            name: "imported".to_string(),
            role: None,
            model: None,
            description: None,
        },
        llm: LlmParams {
            temperature: None,
            max_tokens: None,
            model_override: None,
        },
        system_prompt: SystemPrompt {
            content: String::new(),
            append_skills: Vec::new(),
        },
        tools: Some(ToolsConfig { allowed: None }),
        runner: None,
        permissions: None,
    }
}

/// The shared `version-control.md` grant translates to a working tcode
/// allowlist: read, write, edit, shell, skills, and `finish_task`.
///
/// Why: this is the exact frontmatter a `tcode paths import` of trusty-mpm's
/// catalog carries, and the agent it gates is the one that has to run `git`
/// and `gh`. A translation that lost `bash` would leave the imported agent
/// unable to do the only job its body describes.
/// What: feeds the literal `tools:` list from
/// `crates/trusty-agents-common/src/assets/agents/version-control.md` through
/// the translation and asserts the full expected allowlist, in canonical
/// order.
/// Test: this test.
#[test]
fn translates_the_shared_version_control_grant() {
    let claude = owned(&[
        "Read",
        "Write",
        "Edit",
        "Bash",
        "BashOutput",
        "KillShell",
        "Grep",
        "Glob",
        "Skill",
        "mcp__trusty-review",
    ]);
    assert_eq!(
        claude_tools_to_tcode_allowlist(&claude),
        owned(&[
            "read_file",
            "write_file",
            "write_files",
            "edit",
            "grep",
            "glob",
            "list_dir",
            "bash",
            "use_skill",
            "finish_task",
        ]),
        "`mcp__trusty-review` has no tcode analogue and is dropped; \
         `search_code` is therefore absent"
    );
}

/// A Claude Code tool tcode does not host is dropped, never an error, and
/// never silently widened into some other tool.
///
/// Why: an imported catalog is full of names tcode has no equivalent for. If
/// an unknown name aborted the load, `tcode paths import` of a real Claude
/// Code project would be unusable; if it mapped to something, the agent would
/// hold a capability its author never granted.
/// What: passes four unmapped names plus one mapped one and asserts only the
/// mapped one (plus the `finish_task` floor) survives.
/// Test: this test.
#[test]
fn unknown_tool_names_are_dropped_not_errors() {
    let claude = owned(&[
        "WebFetch",
        "WebSearch",
        "TodoWrite",
        "mcp__claude-in-chrome",
        "Grep",
    ]);
    assert_eq!(
        claude_tools_to_tcode_allowlist(&claude),
        owned(&["grep", "finish_task"])
    );
}

/// An empty Claude Code grant still yields `finish_task`.
///
/// Why: `tools: []` is a deliberate deny-all, and tcode must honour it — but
/// an agent with literally no tools cannot return a result, so the task would
/// hang until the turn budget ran out rather than completing with an empty
/// answer. `finish_task` is the protocol floor, not a capability.
/// What: asserts the empty input maps to exactly `["finish_task"]`.
/// Test: this test.
#[test]
fn empty_grant_still_yields_finish_task() {
    assert_eq!(
        claude_tools_to_tcode_allowlist(&[]),
        owned(&["finish_task"])
    );
}

/// The emitted order is tcode's canonical vocabulary order, not the order the
/// author happened to write the Claude Code names in.
///
/// Why: two agents granting the same capabilities must produce byte-identical
/// allowlists, so `agents.describe` output is comparable and a test can assert
/// on an exact list rather than a set.
/// What: translates the same grant twice with the declaration order reversed
/// and asserts the two results are equal.
/// Test: this test.
#[test]
fn translation_order_is_canonical_not_declaration_order() {
    let forward = claude_tools_to_tcode_allowlist(&owned(&["Read", "Bash", "Write"]));
    let reversed = claude_tools_to_tcode_allowlist(&owned(&["Write", "Bash", "Read"]));
    assert_eq!(forward, reversed);
    assert_eq!(
        forward,
        owned(&[
            "read_file",
            "write_file",
            "write_files",
            "list_dir",
            "bash",
            "finish_task"
        ])
    );
}

/// Every `tools:` name the shared roster actually uses is either translated or
/// deliberately dropped — no name reaches the translation unaccounted for.
///
/// Why: the table is only correct relative to the vocabulary it has to cover.
/// A new Claude Code tool appearing in the shared roster should be a
/// deliberate decision here, not a silent drop nobody noticed.
/// What: enumerates the distinct names across the shared roster's `tools:`
/// lines (as of #8129) and asserts each is either in [`CLAUDE_TO_TCODE`] or in
/// the explicitly-reviewed drop list.
/// Test: this test.
#[test]
fn every_shared_roster_tool_name_is_translated_or_deliberately_dropped() {
    const DELIBERATELY_DROPPED: &[&str] = &[
        "WebFetch",
        "WebSearch",
        "mcp__trusty-memory",
        "mcp__trusty-mpm",
        "mcp__trusty-review",
        "mcp__claude-in-chrome",
    ];
    const OBSERVED: &[&str] = &[
        "Read",
        "Write",
        "Edit",
        "Bash",
        "BashOutput",
        "KillShell",
        "Grep",
        "Glob",
        "Skill",
        // `Task` appears in no shared roster `tools:` line today — the roster's
        // delegating agent is trusty-mpm's PM, which this crate does not embed.
        // It is in the table because an IMPORTED Claude Code catalog may carry
        // it, and `delegate_to_agent` is the one grant a misread would hand an
        // agent the power to fan out with. Asserted concretely below.
        "Task",
        "WebFetch",
        "WebSearch",
        "mcp__trusty-search",
        "mcp__trusty-memory",
        "mcp__trusty-mpm",
        "mcp__trusty-review",
        "mcp__claude-in-chrome",
    ];
    for name in OBSERVED {
        let translated = CLAUDE_TO_TCODE.iter().any(|(k, _)| k == name);
        let dropped = DELIBERATELY_DROPPED.contains(name);
        assert!(
            translated ^ dropped,
            "'{name}' must be either translated or deliberately dropped, exactly one"
        );
    }

    // The two entries the table carries that no shared roster agent exercises,
    // pinned by value rather than by membership: `Task` is the delegation
    // grant, and `mcp__trusty-search` is the only `mcp__*` server with a tcode
    // analogue. A membership check alone would not catch either mapping being
    // rewired to the wrong tcode tool.
    assert_eq!(
        claude_tools_to_tcode_allowlist(&owned(&["Task"])),
        owned(&["delegate_to_agent", "finish_task"])
    );
    assert_eq!(
        claude_tools_to_tcode_allowlist(&owned(&["mcp__trusty-search"])),
        owned(&["search_code", "finish_task"])
    );
}

/// An explicit `tcode_tools:` wins: the Claude Code grant is never consulted.
///
/// Why: #7683's contract. A document that names tcode's own vocabulary has
/// said exactly what it wants; a Claude Code list beside it is for the other
/// runtime and must not narrow or widen the tcode grant.
/// What: builds metadata carrying BOTH keys, asserts the fallback leaves the
/// config's pre-projected `tcode_tools`-derived allowlist untouched.
/// Test: this test.
#[test]
fn explicit_tcode_tools_wins_over_the_claude_grant() {
    let mut meta = AgentMetadata::default();
    meta.tcode_tools = Some(owned(&["read_file", "finish_task"]));
    meta.tools = Some(owned(&["Read", "Write", "Edit", "Bash"]));

    let mut config = blank_config();
    config.tools = Some(ToolsConfig {
        allowed: meta.tcode_tools.clone(),
    });
    apply_claude_tools_fallback(&mut config, &meta);

    assert_eq!(
        config.tools.and_then(|t| t.allowed),
        Some(owned(&["read_file", "finish_task"]))
    );
}

/// A deliberate `tcode_tools: []` deny-all survives the fallback.
///
/// Why: `Some(vec![])` and `None` are different declarations everywhere else
/// in this loader, and conflating them here would silently re-grant every
/// translated tool to an agent whose author denied all of them.
/// What: metadata with `tcode_tools: Some([])` and a full Claude grant;
/// asserts the allowlist stays empty.
/// Test: this test.
#[test]
fn deny_all_tcode_tools_is_not_overwritten() {
    let mut meta = AgentMetadata::default();
    meta.tcode_tools = Some(Vec::new());
    meta.tools = Some(owned(&["Read", "Bash"]));

    let mut config = blank_config();
    config.tools = Some(ToolsConfig {
        allowed: Some(Vec::new()),
    });
    apply_claude_tools_fallback(&mut config, &meta);

    assert_eq!(config.tools.and_then(|t| t.allowed), Some(Vec::new()));
}

/// A document declaring NEITHER key stays unrestricted.
///
/// Why: an agent that declares no grant at all means "all tools", and that is
/// the pre-#8129 behaviour for every such document. The fallback must fire
/// only where there is a Claude Code grant to honour.
/// What: metadata with both keys `None`; asserts `tools.allowed` stays `None`.
/// Test: this test.
#[test]
fn absent_tools_leaves_the_allowlist_unset() {
    let meta = AgentMetadata::default();
    let mut config = blank_config();
    apply_claude_tools_fallback(&mut config, &meta);
    assert_eq!(config.tools.and_then(|t| t.allowed), None);
}
