//! `tcode paths import` proven against trusty-mpm's real agent catalog (#8129).
//!
//! Why: issue #8129's second closure condition is that the import path is
//! VERIFIED to carry trusty-mpm's agents, not assumed to. The unit tests under
//! `paths::import_tests` use synthetic fixtures, so they prove the copy plan is
//! deterministic and the refusals fire — they say nothing about whether the
//! bytes that land parse into a usable tcode agent. trusty-mpm's frontmatter is
//! a different dialect: `extends:` chains, `skills:`, and a `tools:` key in
//! CLAUDE CODE's vocabulary, which #7683 made this runtime ignore. An agent
//! that imports cleanly but projects to a config nothing can dispatch is the
//! failure this file exists to catch.
//!
//! What: stages a temp project whose `.claude/agents/` holds the REAL shared
//! assets — `trusty_agents_common::agent_assets::{TICKETING, VERSION_CONTROL}`
//! plus the two `BASE-*` templates their `extends:` chains resolve against,
//! exactly as a trusty-mpm deployment lays them out — then runs
//! `plan_import`/`apply_import` in-process and loads each imported file back
//! through `agents::load_md_agent`. Asserts the frontmatter's identity, model
//! and skills survive, and that the Claude Code `tools:` grant reaches tcode
//! as a NON-EMPTY allowlist naming tcode's own tools.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};

use trusty_agents_common::agent_assets;
use trusty_code::agents::load_md_agent;
use trusty_code::paths::import::{ImportAction, apply_import, plan_import};

/// The four files a trusty-mpm `.claude/agents/` deployment must carry for the
/// two agents under test to compose: the agents themselves plus the `BASE-*`
/// templates their `extends:` chains name.
///
/// `ticketing` extends `base-agent`; `version-control` extends `base-ops`,
/// which itself extends `base-agent`.
const CATALOG: &[(&str, &str)] = &[
    ("BASE-AGENT.md", agent_assets::BASE_AGENT),
    ("BASE-OPS.md", agent_assets::BASE_OPS),
    ("ticketing.md", agent_assets::TICKETING),
    ("version-control.md", agent_assets::VERSION_CONTROL),
];

/// Stage a project root whose `.claude/agents/` holds the real shared catalog.
fn staged_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let agents = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("create .claude/agents");
    for (name, content) in CATALOG {
        std::fs::write(agents.join(name), content).expect("stage catalog file");
    }
    tmp
}

/// The imported target for one agent filename.
fn imported_path(root: &Path, file: &str) -> PathBuf {
    root.join(".trusty-code").join("agents").join(file)
}

/// The dry-run plan lists every catalog file as a copy, and applying it creates
/// exactly those files under `.trusty-code/agents/`.
///
/// Why: the plan is what an operator reads before authorising the import, so a
/// trusty-mpm catalog file silently classified as a refusal would be invisible
/// until the agent turned up missing. Asserting plan-then-apply in one test is
/// what ties the listing to the result.
/// What: plans the staged project, asserts each catalog filename appears with
/// [`ImportAction::Copy`], applies it, and asserts `created` covers all four
/// and `refused` is empty.
/// Test: this test.
#[test]
fn dry_run_plan_and_apply_carry_the_whole_mpm_catalog() {
    let tmp = staged_project();
    let root = tmp.path();

    let plan = plan_import(root);
    for (name, _) in CATALOG {
        let entry = plan
            .entries
            .iter()
            .find(|e| e.to == imported_path(root, name))
            .unwrap_or_else(|| panic!("'{name}' must appear in the import plan"));
        assert_eq!(
            entry.action,
            ImportAction::Copy,
            "'{name}' must be planned as a copy, not refused"
        );
    }

    let report = apply_import(root, &plan);
    assert!(
        report.refused.is_empty(),
        "no catalog file may be refused: {:?}",
        report.refused
    );
    for (name, _) in CATALOG {
        let target = imported_path(root, name);
        assert!(
            report.created.contains(&target),
            "'{name}' must be reported as created"
        );
        assert!(target.is_file(), "'{name}' must exist on disk after apply");
    }
}

/// An imported trusty-mpm agent loads into a tcode `AgentConfig` whose tool
/// allowlist is non-empty and names TCODE's tools.
///
/// Why: this is #8129's acceptance criterion for the import path. Before the
/// `agents::claude_tools` translation, the shared `tools: [Read, Write, Edit,
/// Bash, ...]` was dropped on the floor: `tools.allowed` came back `None`,
/// which means "every tool allowed" — so the import silently WIDENED the
/// author's grant rather than honouring it, and no assertion on the config
/// could tell an imported agent from one that declared nothing.
/// What: applies the import, loads both agents with `load_md_agent`, and
/// asserts each carries its frontmatter identity, its declared model, its
/// `skills:` list where it has one, and a non-empty allowlist containing
/// `bash` and `finish_task` — plus, for `version-control`, the write/edit
/// tools its `tools: [..., Write, Edit, ...]` grant maps to, and for
/// `ticketing`, NOT those tools, since its grant omits them.
/// Test: this test.
#[test]
fn imported_mpm_agents_parse_with_a_usable_tool_allowlist() {
    let tmp = staged_project();
    let root = tmp.path();
    let plan = plan_import(root);
    apply_import(root, &plan);

    let ticketing =
        load_md_agent(&imported_path(root, "ticketing.md")).expect("imported ticketing must load");
    assert_eq!(ticketing.agent.name, "ticketing");
    assert_eq!(ticketing.agent.model.as_deref(), Some("sonnet"));

    let allowed = ticketing
        .tools
        .and_then(|t| t.allowed)
        .expect("ticketing's Claude Code grant must translate to a tcode allowlist");
    assert!(
        !allowed.is_empty(),
        "an imported agent must not end up with a deny-all allowlist"
    );
    assert!(
        allowed.iter().any(|t| t == "bash") && allowed.iter().any(|t| t == "finish_task"),
        "ticketing must keep shell access and the ability to return: {allowed:?}"
    );
    assert!(
        !allowed.iter().any(|t| t == "write_file" || t == "edit"),
        "ticketing's grant has no Write/Edit, so neither may appear: {allowed:?}"
    );

    let vc = load_md_agent(&imported_path(root, "version-control.md"))
        .expect("imported version-control must load");
    assert_eq!(vc.agent.name, "version-control");
    assert_eq!(vc.agent.model.as_deref(), Some("sonnet"));
    assert!(
        vc.system_prompt
            .append_skills
            .iter()
            .any(|s| s == "git-workflow"),
        "version-control's `skills:` must survive the import: {:?}",
        vc.system_prompt.append_skills
    );

    let vc_allowed = vc
        .tools
        .and_then(|t| t.allowed)
        .expect("version-control's Claude Code grant must translate");
    for expected in ["read_file", "write_file", "edit", "bash", "finish_task"] {
        assert!(
            vc_allowed.iter().any(|t| t == expected),
            "version-control must be granted '{expected}': {vc_allowed:?}"
        );
    }
}

/// The composed body of an imported agent is real prose, not an empty string,
/// and carries no leftover frontmatter fence.
///
/// Why: `extends:` composition is the part of trusty-mpm's dialect most likely
/// to degrade quietly — a chain that failed to resolve would still produce an
/// `AgentConfig`, just one whose system prompt is the leaf body alone or
/// nothing at all. Checking for a BASE-template sentence proves the chain
/// actually resolved against the imported `BASE-*.md` files.
/// What: asserts `version-control`'s composed prompt contains both a
/// leaf-body heading and a sentence that only `BASE-AGENT.md` supplies.
/// Test: this test.
#[test]
fn imported_agent_body_is_composed_through_its_extends_chain() {
    let tmp = staged_project();
    let root = tmp.path();
    let plan = plan_import(root);
    apply_import(root, &plan);

    let vc = load_md_agent(&imported_path(root, "version-control.md"))
        .expect("imported version-control must load");
    let body = &vc.system_prompt.content;

    assert!(
        !body.trim().is_empty(),
        "an imported agent's system prompt must not be empty"
    );
    assert!(
        !body.trim_start().starts_with("---"),
        "the frontmatter fence must not leak into the body"
    );
    assert!(
        body.contains("# Version Control Agent"),
        "the leaf body must be present"
    );
    assert!(
        body.contains("BASE-AGENT"),
        "the `extends:` chain must have pulled in BASE-AGENT's content"
    );
}
