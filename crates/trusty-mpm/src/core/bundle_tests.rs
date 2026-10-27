//! Tests for the bundle module.
//!
//! Why: `bundle.rs` is split to stay under the 500-line cap while keeping all
//! test coverage for embedded artifact integrity, policy correctness, and
//! agent inheritance-chain round-trips in one focused location.
//! What: unit and integration tests for every bundled artifact constant and
//! the [`crate::core::bundle::ALL`] table.
//! Test: this file is the test coverage; run with `cargo test -p trusty-mpm bundle`.
use super::*;
use crate::core::content_source::test_support::rc;
use crate::core::framework_content::tests::skill_with_references;

// #10058: pinned prose became rule-id markers; the registry and lookups.
#[path = "bundle_rule_markers_tests.rs"]
mod rule_markers;
use rule_markers::{has_rule, plain, rule_section, rule_sentence};

/// A skill file's text from the checkout's content (#9012: skills are runtime
/// content, no longer compiled-in constants).
fn skill_text(path: &str) -> &'static str {
    rc().skill(path)
        .unwrap_or_else(|| panic!("{path} must be in the content tree"))
}

/// An instruction file's text from the checkout's content (#9012).
fn instruction_text(path: &str) -> &'static str {
    rc().instruction(path)
        .unwrap_or_else(|| panic!("instructions/{path} must be in the content tree"))
}

/// The bundled docs `tm install` writes beside the skills (#9012).
const BUNDLED_DOCS: [&str; 2] = [
    "docs/WHAT-IS-TRUSTY-MPM.md",
    "docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md",
];

/// Every artifact `tm install` writes: the compiled-in hook policies, then the
/// content's skills and bundled docs (#9012: runtime content), in the shape the
/// pre-#9012 `ALL` table had so the registration tests keep their meaning.
fn deployed() -> Vec<BundledArtifact> {
    let docs = BUNDLED_DOCS
        .into_iter()
        .map(|rel| (rel, instruction_text(rel)));
    ALL.iter()
        .copied()
        .chain(
            rc().skills()
                .chain(docs)
                .map(|(rel_path, contents)| BundledArtifact {
                    rel_path,
                    contents,
                    install: InstallPolicy::Overwrite,
                }),
        )
        .collect()
}

#[test]
fn constants_are_non_empty() {
    // Every embedded artifact must carry real content — an empty
    // `include_str!` target would mean a missing or truncated asset file.
    assert!(!OPTIMIZER_TOML.trim().is_empty());
    assert!(!OVERSEER_TOML.trim().is_empty());
    // Increment 3 agents
    assert!(
        !skill_text("skills/code-review-standards.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/contract-driven-testing.md")
            .trim()
            .is_empty()
    );
    assert!(
        !instruction_text("output-styles/trusty-mpm.md")
            .trim()
            .is_empty()
    );
    assert!(
        !instruction_text("output-styles/trusty-mpm-teacher.md")
            .trim()
            .is_empty()
    );
    assert!(
        !instruction_text("output-styles/trusty-mpm-research.md")
            .trim()
            .is_empty()
    );
    assert!(!skill_text("skills/tm-doctor.md").trim().is_empty());
    // /tm- portfolio (tm-skills-portfolio epic)
    assert!(!skill_text("skills/tm-circuit-breaker.md").trim().is_empty());
    assert!(
        !skill_text("skills/tm-verification-protocols.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-tool-usage-guide.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-git-file-tracking.md")
            .trim()
            .is_empty()
    );
    assert!(!skill_text("skills/tm-adr.md").trim().is_empty());
    assert!(!skill_text("skills/tm-workflow.md").trim().is_empty());
    assert!(
        !skill_text("skills/tm-agent-architecture.md")
            .trim()
            .is_empty()
    );
    assert!(!skill_text("skills/tm-postmortem.md").trim().is_empty());
    assert!(!skill_text("skills/tm-prose-style.md").trim().is_empty());
    assert!(!skill_text("skills/tm-bug-reporting.md").trim().is_empty());
    assert!(
        !skill_text("skills/tm-teaching-templates.md")
            .trim()
            .is_empty()
    );
    assert!(!skill_text("skills/tm-ticketing.md").trim().is_empty());
    assert!(
        !skill_text("skills/tm-delegation-patterns.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-session-management.md")
            .trim()
            .is_empty()
    );
    assert!(!skill_text("skills/tm-session-pause.md").trim().is_empty());
    assert!(!skill_text("skills/tm-session-resume.md").trim().is_empty());
    assert!(!skill_text("skills/tm-init.md").trim().is_empty());
    assert!(!skill_text("skills/tm.md").trim().is_empty());
    assert!(!skill_text("skills/tm-issues-prune.md").trim().is_empty());
    assert!(!skill_text("skills/tm-cli-operations.md").trim().is_empty());
    assert!(
        !instruction_text("docs/WHAT-IS-TRUSTY-MPM.md")
            .trim()
            .is_empty()
    );
    // --- BEGIN issue #2911: documentation-style bundled skill (append-only) ---
    assert!(
        !skill_text("skills/documentation-style.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/documentation-style/references/spec.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/documentation-style/references/readme.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/documentation-style/references/file-level.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/documentation-style/references/class.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/documentation-style/references/method-function.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/documentation-style/references/block-inline.md")
            .trim()
            .is_empty()
    );
    // --- END issue #2911 ---
    assert!(
        !skill_text("skills/rust-build-performance.md")
            .trim()
            .is_empty()
    );
}

#[test]
fn tm_skills_are_in_bundle() {
    // The /tm- portfolio (tm-skills-portfolio epic) must be present in ALL so
    // `trusty-mpm install` deploys every skill offline.
    let skill_paths: Vec<&str> = deployed()
        .into_iter()
        .filter(|a| a.rel_path.starts_with("skills/tm-") || a.rel_path == "skills/tm.md")
        .map(|a| a.rel_path)
        .collect();

    for expected in &[
        "skills/tm-doctor.md",
        "skills/tm-circuit-breaker.md",
        "skills/tm-verification-protocols.md",
        "skills/tm-tool-usage-guide.md",
        "skills/tm-git-file-tracking.md",
        "skills/tm-adr.md",
        "skills/tm-workflow.md",
        "skills/tm-agent-architecture.md",
        "skills/tm-postmortem.md",
        "skills/tm-prose-style.md",
        "skills/tm-bug-reporting.md",
        "skills/tm-teaching-templates.md",
        "skills/tm-ticketing.md",
        "skills/tm-delegation-patterns.md",
        "skills/tm-session-management.md",
        "skills/tm-session-pause.md",
        "skills/tm-session-resume.md",
        "skills/tm-init.md",
        "skills/tm.md",
        "skills/tm-issues-prune.md",
        "skills/tm-cli-operations.md",
        "skills/tm-slack.md",
        "skills/tm-secrets.md",
        "skills/tm-architect-setup.md",
        // #8625: kuzu import with the masked refused-row review.
        "skills/tm-memory-import.md",
        // #8376: entry file plus its four reference files.
        "skills/tm-epic.md",
        "skills/tm-epic/references/tracker-template.md",
        "skills/tm-epic/references/phase-template.md",
        "skills/tm-epic/references/manual-procedure.md",
        "skills/tm-epic/references/anti-patterns.md",
    ] {
        assert!(
            skill_paths.contains(expected),
            "missing bundled /tm- skill: {expected}"
        );
    }
}

/// #5202: `tm-pr-workflow` is RETIRED, not aliased.
///
/// Why: the consolidation only holds if there is exactly ONE workflow skill. A
/// re-registered alias, or a re-added asset file, restores the second editable
/// source the issue removed — and it would do so silently, because every other
/// test here only asserts what IS present.
/// What: asserts the retired stem appears in no bundled artifact path, and that
/// the skill it was consolidated into is present.
/// Test: this IS the test.
#[test]
fn retired_pr_workflow_skill_is_not_bundled() {
    let paths: Vec<&str> = deployed().into_iter().map(|a| a.rel_path).collect();
    assert!(
        !paths.iter().any(|p| p.contains("tm-pr-workflow")),
        "tm-pr-workflow was retired into tm-workflow (#5202); re-registering it \
         restores the split this consolidation removed"
    );
    assert!(
        paths.contains(&"skills/tm-workflow.md"),
        "tm-workflow must remain bundled — it now carries the whole delivery chain"
    );
}

#[test]
fn tm_capabilities_is_in_bundle() {
    // Issue #2913: the generated tm-capabilities entry + all five
    // references/*.md files, plus the hand-authored references/workflows.md,
    // must be present in ALL so `trusty-mpm install` deploys them offline —
    // the same "registration here is what makes deploy_all_skill_tiers ship
    // it" lesson as the code-critic bundled-skills fix (issue #2890).
    let paths: Vec<&str> = deployed().into_iter().map(|a| a.rel_path).collect();
    for expected in &[
        "skills/tm-capabilities.md",
        "skills/tm-capabilities/references/cli.md",
        "skills/tm-capabilities/references/mcp-tools.md",
        "skills/tm-capabilities/references/agents.md",
        "skills/tm-capabilities/references/skills.md",
        "skills/tm-capabilities/references/doctor.md",
        "skills/tm-capabilities/references/framework.md",
        "skills/tm-capabilities/references/workflows.md",
    ] {
        assert!(paths.contains(expected), "missing bundled file: {expected}");
    }
}

#[test]
fn tm_capabilities_has_frontmatter() {
    // The entry file must carry the standard bundled-skill frontmatter shape
    // (name/description/user-invocable), same convention as every other
    // skill in ALL.
    assert!(skill_text("skills/tm-capabilities.md").starts_with("---\nname: tm-capabilities\n"));
    assert!(skill_text("skills/tm-capabilities.md").contains("user-invocable: false"));
}

#[test]
fn tm_capabilities_constants_are_non_empty() {
    assert!(!skill_text("skills/tm-capabilities.md").trim().is_empty());
    assert!(
        !skill_text("skills/tm-capabilities/references/cli.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-capabilities/references/mcp-tools.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-capabilities/references/agents.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-capabilities/references/skills.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-capabilities/references/doctor.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-capabilities/references/framework.md")
            .trim()
            .is_empty()
    );
    assert!(
        !skill_text("skills/tm-capabilities/references/workflows.md")
            .trim()
            .is_empty()
    );
}

#[test]
fn tm_capabilities_workflows_carries_the_context_tool_field_reference() {
    // #10300: the session_context_* MCP descriptions point here for the
    // field reference they no longer carry; losing it strands every caller.
    let workflows = skill_text("skills/tm-capabilities/references/workflows.md");
    assert!(
        workflows.contains("## 6. PM Pause and Resume"),
        "section 6 missing"
    );
    for field in [
        "snapshot_publish",
        "ref_error",
        "resume_slot",
        "skipped_dirty_worktrees",
        "resolved_via",
        "resolved_snapshot_superseded",
        "resume_verify",
        "own_ref_found",
        "sessions_next_offset",
        "over_budget",
    ] {
        assert!(workflows.contains(field), "workflows.md lost `{field}`");
    }
}

/// The rule id of the labels/project/milestone standard (#7274, #10058).
const STANDARD_RULE_ID: &str = "tm-ticketing/labels-project-milestone-standard";

#[test]
fn the_labels_project_milestone_standard_is_stated_once() {
    // #7274, owner ruling 2026-09-09: one standard covering issues AND pull
    // requests, stated in `tm-ticketing` and pointed at from the three files
    // that carry the mechanics. A second statement of the rule is the drift
    // this consolidation removed — #6939 and #7073 hand-edited two copies in
    // step, which worked only for as long as someone remembered to.
    // #10055: the canonical section is resident in the tm-ticketing entry;
    // tm-workflow's pointer may sit in its references.
    // #10058: the rule is its marker; "stated once" and the pointers read the
    // sentence and section through it, so rewording the rule breaks nothing.
    let canonical = skill_text("skills/tm-ticketing.md");
    assert!(
        has_rule(canonical, STANDARD_RULE_ID),
        "`tm-ticketing.md` must carry the marker `{STANDARD_RULE_ID}`"
    );
    let rule = rule_sentence(canonical, STANDARD_RULE_ID).expect("the standard's sentence");
    let name = rule_section(canonical, STANDARD_RULE_ID).expect("the standard's section");
    let stated: Vec<&str> = [
        ("tm-ticketing.md", skill_text("skills/tm-ticketing.md")),
        ("ticketing.md", agent("ticketing.md")),
        ("version-control.md", agent("version-control.md")),
        (
            "tm-workflow.md",
            skill_with_references("skills/tm-workflow.md"),
        ),
    ]
    .into_iter()
    .filter(|(_, body)| plain(body).contains(&rule))
    .map(|(name, _)| name)
    .collect();
    assert_eq!(
        stated,
        vec!["tm-ticketing.md"],
        "the rule sentence must appear in `tm-ticketing.md` and nowhere else"
    );

    // The three mechanics files reach the rule by naming the section, so a
    // reader landing in any of them can find it without knowing it exists.
    for (file, body) in [
        ("ticketing.md", agent("ticketing.md")),
        ("version-control.md", agent("version-control.md")),
        (
            "tm-workflow.md",
            skill_with_references("skills/tm-workflow.md"),
        ),
    ] {
        assert!(
            one_line(body).contains(name),
            "`{file}` must reference the shared section by name (`{name}`)"
        );
        assert!(
            body.contains("tm-ticketing"),
            "`{file}` must say which skill holds the shared section"
        );
    }

    // `version-control.md` also has to say what applies its half.
    assert!(
        has_rule(
            agent("version-control.md"),
            "version-control/tm-pr-open-applies-half"
        ),
        "`version-control.md` must say `tm pr open` applies the PR side (#7274)"
    );
}

/// The rule id of the milestone/relationships/state rule (#10045, #10058).
const EVERY_ISSUE_RULE_ID: &str = "tm-ticketing/every-issue-milestone-relationships-state";

/// `text` with every whitespace run collapsed to one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Why: owner rule 2026-10-19 (#10045) — every issue carries a milestone,
/// relationships and state, stated once in `tm-ticketing` and pointed at
/// from the ticketing agents and every skill that files, moves or closes an
/// issue.
/// What: `tm-ticketing` carries the rule, propose-a-milestone and checklist
/// markers; no other skill or agent holds the rule's sentence; each pointer
/// file names the rule's section and `tm-ticketing`; the ticketing agent
/// carries its mandatory, propose and report markers (#10058).
/// Test: this test.
#[test]
fn every_issue_milestone_relationships_state_is_stated_once_10045() {
    // #10055: entry files only — the rule and its pointers are resident text
    // that must load on invoke, never only from references/.
    let canonical = skill_text("skills/tm-ticketing.md");
    for id in [
        EVERY_ISSUE_RULE_ID,
        "tm-ticketing/propose-a-milestone-set",
        "tm-ticketing/status-and-label-name-one-stage",
    ] {
        assert!(
            has_rule(canonical, id),
            "`tm-ticketing.md` must carry `{id}` (#10045)"
        );
    }
    let rule = rule_sentence(canonical, EVERY_ISSUE_RULE_ID).expect("the rule's sentence");
    let section = rule_section(canonical, EVERY_ISSUE_RULE_ID).expect("the rule's section");

    let restated: Vec<&str> = rc()
        .skills()
        .chain([
            ("agents/ticketing.md", agent("ticketing.md")),
            ("agents/version-control.md", agent("version-control.md")),
        ])
        .filter(|(rel, body)| *rel != "skills/tm-ticketing.md" && plain(body).contains(&rule))
        .map(|(rel, _)| rel)
        .collect();
    assert!(
        restated.is_empty(),
        "the rule sentence belongs to `tm-ticketing.md` alone: {restated:?}"
    );

    let tcode_ticketing =
        std::fs::read_to_string(agent_assets_dir().join("trusty-code/ticketing.md"))
            .expect("content/agents/trusty-code/ticketing.md");
    let pointers = [
        ("agents/ticketing.md", agent("ticketing.md").to_string()),
        ("agents/trusty-code/ticketing.md", tcode_ticketing),
        ("skills/tm-epic.md", skill_text("skills/tm-epic.md").into()),
        (
            "skills/tm-epic/references/manual-procedure.md",
            skill_text("skills/tm-epic/references/manual-procedure.md").into(),
        ),
        (
            "skills/tm-issues-prune.md",
            skill_text("skills/tm-issues-prune.md").into(),
        ),
        (
            "skills/tm-workflow.md",
            skill_text("skills/tm-workflow.md").into(),
        ),
        (
            "skills/tm-bug-reporting.md",
            skill_text("skills/tm-bug-reporting.md").into(),
        ),
        (
            "skills/tm-delegation-patterns.md",
            skill_text("skills/tm-delegation-patterns.md").into(),
        ),
        (
            "skills/tm-postmortem.md",
            skill_text("skills/tm-postmortem.md").into(),
        ),
        (
            "skills/tm-tool-usage-guide.md",
            skill_text("skills/tm-tool-usage-guide.md").into(),
        ),
        (
            "skills/rust-delivery-workflow.md",
            skill_text("skills/rust-delivery-workflow.md").into(),
        ),
    ];
    for (file, body) in &pointers {
        let body = one_line(body);
        assert!(
            body.contains(section) && body.contains("tm-ticketing"),
            "`{file}` must point at `tm-ticketing`, section `{section}`"
        );
    }

    for id in [
        "ticketing/milestone-relationships-state-mandatory",
        "ticketing/propose-a-milestone",
        "ticketing/report-milestone-relationships-state",
    ] {
        assert!(
            has_rule(agent("ticketing.md"), id),
            "`ticketing.md` must carry `{id}` (#10045)"
        );
    }
}

/// Why: #9747 — gh 2.98 rejects `gh repo view --json autoMergeAllowed`, and
/// the shipped instruction to run it failed 61 times in 72 h.
/// What: the two files carrying the #8640 auto-merge read name the REST field
/// `allow_auto_merge`, and no deployed skill or the version-control agent
/// names the rejected field.
#[test]
fn auto_merge_read_uses_a_field_gh_accepts_9747() {
    // #10058: the read is pinned by its marker in each file.
    for (file, body, id) in [
        (
            "version-control.md",
            agent("version-control.md"),
            "version-control/auto-merge-read",
        ),
        (
            "tm-workflow.md",
            skill_with_references("skills/tm-workflow.md"),
            "tm-workflow/auto-merge-read",
        ),
    ] {
        assert!(
            has_rule(body, id),
            "`{file}` must carry the auto-merge read `{id}`"
        );
    }
    let named: Vec<&str> = rc()
        .skills()
        .chain([("agents/version-control.md", agent("version-control.md"))])
        .filter(|(_, body)| body.contains("autoMergeAllowed"))
        .map(|(rel, _)| rel)
        .collect();
    assert!(named.is_empty(), "gh rejects `autoMergeAllowed`: {named:?}");
}

#[test]
fn tm_skills_have_frontmatter() {
    // Every /tm- skill must carry YAML frontmatter with a tm-native `name:`
    // and must not leak unadapted claude-mpm references.
    let skills = [
        (
            "tm-circuit-breaker",
            skill_text("skills/tm-circuit-breaker.md"),
        ),
        (
            "tm-verification-protocols",
            skill_text("skills/tm-verification-protocols.md"),
        ),
        (
            "tm-tool-usage-guide",
            skill_text("skills/tm-tool-usage-guide.md"),
        ),
        (
            "tm-git-file-tracking",
            skill_text("skills/tm-git-file-tracking.md"),
        ),
        ("tm-adr", skill_text("skills/tm-adr.md")),
        ("tm-workflow", skill_text("skills/tm-workflow.md")),
        (
            "tm-agent-architecture",
            skill_text("skills/tm-agent-architecture.md"),
        ),
        ("tm-postmortem", skill_text("skills/tm-postmortem.md")),
        ("tm-prose-style", skill_text("skills/tm-prose-style.md")),
        ("tm-bug-reporting", skill_text("skills/tm-bug-reporting.md")),
        (
            "tm-teaching-templates",
            skill_text("skills/tm-teaching-templates.md"),
        ),
        ("tm-ticketing", skill_text("skills/tm-ticketing.md")),
        (
            "tm-delegation-patterns",
            skill_text("skills/tm-delegation-patterns.md"),
        ),
        (
            "tm-session-management",
            skill_text("skills/tm-session-management.md"),
        ),
        ("tm-session-pause", skill_text("skills/tm-session-pause.md")),
        (
            "tm-session-resume",
            skill_text("skills/tm-session-resume.md"),
        ),
        ("tm-init", skill_text("skills/tm-init.md")),
        ("tm", skill_text("skills/tm.md")),
        ("tm-issues-prune", skill_text("skills/tm-issues-prune.md")),
        (
            "tm-cli-operations",
            skill_text("skills/tm-cli-operations.md"),
        ),
        ("tm-slack", skill_text("skills/tm-slack.md")),
        ("tm-secrets", skill_text("skills/tm-secrets.md")),
        (
            "tm-architect-setup",
            skill_text("skills/tm-architect-setup.md"),
        ),
        ("tm-epic", skill_text("skills/tm-epic.md")),
        ("tm-memory-import", skill_text("skills/tm-memory-import.md")),
    ];
    for (name, content) in skills {
        assert!(
            content.starts_with("---\n"),
            "skill {name} is missing YAML frontmatter"
        );
        assert!(
            content.contains(&format!("name: {name}")),
            "skill {name} frontmatter name must match its file stem"
        );
        assert!(
            !content.contains("claude-mpm") || content.contains("trusty-mpm"),
            "skill {name} contains an unadapted claude-mpm reference"
        );
        // tm-session-management is the one deliberate exception: it documents
        // `tm session catchup`'s real dual-format cutover bridge (#1762),
        // which genuinely reads the legacy `.claude-mpm/sessions/` path
        // alongside the native one — this is accurate, not an unadapted
        // leftover, and the skill explicitly calls it "legacy".
        assert!(
            !content.contains(".claude-mpm/") || name == "tm-session-management",
            "skill {name} references the legacy .claude-mpm/ path"
        );
    }
}

#[test]
fn output_style_has_matching_frontmatter_name() {
    // Claude Code matches the `outputStyle` settings key against the
    // `name:` in the style file's frontmatter; a mismatch silently falls
    // back to the operator's default style.
    assert!(instruction_text("output-styles/trusty-mpm.md").contains("name: trusty-mpm"));
}

/// #9012: every registry style's body is runtime content — a required
/// instruction file whose frontmatter `name:` is the registry id.
#[test]
fn every_bundled_style_reads_from_content() {
    for style in OUTPUT_STYLES {
        assert!(
            crate::core::framework_content::REQUIRED_INSTRUCTIONS.contains(&style.content_path),
            "{} must be a required instruction, or a load would not guarantee it",
            style.content_path
        );
        // #9755: the Architect style is read at its new path and deployed under
        // the legacy file name its content still carries (D2d, R4).
        let content_file =
            crate::core::architect_compat::legacy_instruction_path(style.content_path)
                .unwrap_or_else(|| style.content_path.to_owned());
        assert_eq!(
            content_file,
            format!("output-styles/{}", style.file_name),
            "a style deploys under the file name its content carries"
        );
        let body = style.content(rc());
        assert!(
            body.contains(&format!("\nname: {}\n", style.id)),
            "{} must declare name: {}",
            style.content_path,
            style.id
        );
    }
}

#[test]
fn output_style_registry_has_four_distinct_ids() {
    // HR-4 bundles three PM styles; #8453 adds the supervisor style. Ids and
    // file names are distinct.
    assert_eq!(OUTPUT_STYLES.len(), 4);
    let mut ids: Vec<&str> = OUTPUT_STYLES.iter().map(|s| s.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4, "style ids must be distinct");

    let mut files: Vec<&str> = OUTPUT_STYLES.iter().map(|s| s.file_name).collect();
    files.sort_unstable();
    files.dedup();
    assert_eq!(files.len(), 4, "style file names must be distinct");
}

#[test]
fn the_pm_styles_are_every_style_but_the_architect_style() {
    // #8453: the PM-invariant tests iterate `pm_output_styles()`; it must drop
    // exactly the supervisor style and nothing else.
    let pm: Vec<&str> = pm_output_styles().map(|s| s.id).collect();
    assert_eq!(
        pm,
        ["trusty-mpm", "trusty-mpm-teacher", "trusty-mpm-research"]
    );
    assert!(OUTPUT_STYLES.iter().any(|s| s.id
        == crate::core::session_profile::ARCHITECT_WRITTEN_OUTPUT_STYLE_ID
        && s.content(rc()) == instruction_text("output-styles/trusty-mpm-supervisor.md")));
}

#[test]
fn output_style_registry_ids_match_frontmatter() {
    // Each registry id MUST equal the file's frontmatter `name:`, or Claude Code
    // silently falls back to the operator's default style.
    for style in OUTPUT_STYLES {
        assert!(
            !style.content(rc()).trim().is_empty(),
            "{} non-empty",
            style.id
        );
        assert!(
            style.content(rc()).contains(&format!("name: {}", style.id)),
            "{} frontmatter name must match registry id",
            style.id
        );
    }
}

#[test]
fn output_styles_carry_identity_protocol_and_load_marker() {
    // DOC-28 R2/R4(b): every bundled output style must carry the non-overridable
    // Identity & Self-Awareness Protocol section, and the marker line
    // `<!-- trusty-mpm-instructions-loaded: v1 -->` must be the literal first
    // line of that section (so a stale/degraded style is greppably distinct
    // from one carrying the current floor).
    // #10058: the load marker is the section's id, so the heading's wording is
    // not pinned. The probe commands stay pinned until their #10061 eval lands:
    // an output style carries no rule-id markers.
    const MARKER: &str = "<!-- trusty-mpm-instructions-loaded: v1 -->";
    for style in OUTPUT_STYLES {
        let body = style.content(rc());
        let marker_pos = body
            .find(MARKER)
            .unwrap_or_else(|| panic!("{} is missing the load marker", style.id));
        let next = body[marker_pos + MARKER.len()..]
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty());
        assert!(
            next.is_some_and(|line| line.starts_with("## ")),
            "{}: the load marker must immediately precede its section heading",
            style.id
        );
        // Forbidden shell-probe list must be greppable for regressions.
        assert!(body.contains("pip3 show"));
        assert!(body.contains("which claude-mpm"));
    }
}

#[test]
fn output_style_registry_default_resolves() {
    // The default id is present and points at the professional style.
    let default = OUTPUT_STYLES
        .iter()
        .find(|s| s.id == DEFAULT_OUTPUT_STYLE_ID)
        .expect("default style must be in the registry");
    assert_eq!(
        default.content(rc()),
        instruction_text("output-styles/trusty-mpm.md")
    );
    assert_eq!(default.file_name, "trusty-mpm.md");
}

#[test]
fn seed_once_constructor_builds_the_expected_artifact() {
    // Issue #3381: no shipped artifact currently uses `SeedOnce`, so the
    // `seed_once()` constructor (the counterpart to `overwrite()`) is only
    // exercised here rather than via a real `ALL` entry. Pins its shape so
    // a future user-owned artifact can rely on it.
    let artifact = all_inner::seed_once("instructions/CLAUDE.md", "stub contents");
    assert_eq!(artifact.rel_path, "instructions/CLAUDE.md");
    assert_eq!(artifact.contents, "stub contents");
    assert_eq!(artifact.install, InstallPolicy::SeedOnce);
}

#[test]
fn optimizer_toml_is_parseable() {
    // The shipped policy must be valid TOML or the installer would deploy
    // a file the daemon then fails to load.
    let parsed: toml::Value = toml::from_str(OPTIMIZER_TOML).expect("valid TOML");
    assert!(parsed.get("default").is_some());
}

/// Every regular file under `dir`, as `<prefix>/<relative path>`, skipping
/// dot-entries — a walk of the content tree on disk that shares no code with
/// the content loader, so a loader that drops or doubles a file disagrees
/// with it (#10055).
fn content_files_on_disk(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("a directory entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let key = format!("{prefix}/{name}");
        let kind = entry.file_type().expect("a file type");
        if kind.is_dir() {
            content_files_on_disk(&entry.path(), &key, out);
        } else if kind.is_file() {
            out.push(key);
        }
    }
}

/// Every way `produced` differs from `expected` read as a set of paths each
/// produced once: a path missing, a path nobody expected, a path produced
/// more than once.
fn inventory_gaps(expected: &[String], produced: &[&str]) -> Vec<String> {
    let mut seen: std::collections::BTreeMap<&str, usize> = Default::default();
    for path in produced {
        *seen.entry(path).or_default() += 1;
    }
    let mut gaps: Vec<String> = expected
        .iter()
        .filter(|path| !seen.contains_key(path.as_str()))
        .map(|path| format!("missing {path}"))
        .collect();
    for (path, times) in seen {
        if !expected.iter().any(|e| e == path) {
            gaps.push(format!("unexpected {path}"));
        }
        if times > 1 {
            gaps.push(format!("{path} produced {times} times"));
        }
    }
    gaps
}

/// Why: #10055 — a hard-coded artifact count turned every skill split into a
/// Rust change; owner rule: "content is not in code".
/// What: [`deployed`] is, path for path and once each, [`ALL`] plus an
/// independent walk of `content/skills` plus [`BUNDLED_DOCS`]; every
/// artifact has a non-empty path and body. Mutation: dropping one produced
/// reference file, or producing one path twice, is reported.
/// Test: this test; `inventory_gaps_reports_a_dropped_or_doubled_file_10055`.
#[test]
fn bundle_table_is_complete() {
    let mut expected: Vec<String> = ALL.iter().map(|a| a.rel_path.to_owned()).collect();
    let floor = expected.len();
    let skills_dir = crate::core::content_source::test_support::repo_root().join("content/skills");
    content_files_on_disk(&skills_dir, "skills", &mut expected);
    assert!(expected.len() > floor, "the walk found no skill file");
    expected.extend(BUNDLED_DOCS.iter().map(|doc| (*doc).to_owned()));

    let artifacts = deployed();
    let produced: Vec<&str> = artifacts.iter().map(|a| a.rel_path).collect();
    let gaps = inventory_gaps(&expected, &produced);
    assert!(
        gaps.is_empty(),
        "deployed artifacts != content tree: {gaps:#?}"
    );
    let data_file = KNOWN_REFERENCE_REACH_GAPS_FILE
        .rsplit('/')
        .next()
        .expect("a file name");
    assert!(
        !produced.iter().any(|path| path.ends_with(data_file)),
        "{KNOWN_REFERENCE_REACH_GAPS_FILE} is test data and must never ship"
    );
    for artifact in &artifacts {
        assert!(!artifact.rel_path.is_empty());
        assert!(!artifact.contents.trim().is_empty());
    }

    let reference = produced
        .iter()
        .position(|path| path.contains("/references/"))
        .expect("a references file");
    let mut dropped = produced.clone();
    let lost = dropped.remove(reference);
    assert_eq!(
        inventory_gaps(&expected, &dropped),
        vec![format!("missing {lost}")],
        "a dropped file must be reported"
    );
    let mut doubled = produced.clone();
    doubled.push(lost);
    assert_eq!(
        inventory_gaps(&expected, &doubled),
        vec![format!("{lost} produced 2 times")],
        "a doubled file must be reported"
    );
}

/// Why: #10055 — the count test's on-disk side must see a file the loader
/// missed, and must not count a dot-file the loader skips.
/// What: a temp tree of an entry, a reference and a dot-file walks to two
/// paths; a produced list missing the reference is red.
/// Test: this test.
#[test]
fn inventory_gaps_reports_a_dropped_or_doubled_file_10055() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let refs = tmp.path().join("probe/references");
    std::fs::create_dir_all(&refs).expect("references dir");
    std::fs::write(tmp.path().join("probe.md"), "entry").expect("entry");
    std::fs::write(refs.join("one.md"), "reference").expect("reference");
    std::fs::write(refs.join(".DS_Store"), "noise").expect("dot-file");
    let mut on_disk = Vec::new();
    content_files_on_disk(tmp.path(), "skills", &mut on_disk);
    on_disk.sort();
    assert_eq!(
        on_disk,
        ["skills/probe.md", "skills/probe/references/one.md"]
    );
    assert!(
        inventory_gaps(
            &on_disk,
            &["skills/probe.md", "skills/probe/references/one.md"]
        )
        .is_empty()
    );
    assert_eq!(
        inventory_gaps(&on_disk, &["skills/probe.md"]),
        ["missing skills/probe/references/one.md"]
    );
}

#[test]
fn overseer_toml_is_parseable() {
    // The shipped overseer policy must be valid TOML and ship disabled —
    // oversight is opt-in, so installing it must not silently enable it.
    let parsed: toml::Value = toml::from_str(OVERSEER_TOML).expect("valid TOML");
    assert_eq!(
        parsed
            .get("overseer")
            .and_then(|o| o.get("enabled"))
            .and_then(toml::Value::as_bool),
        Some(false)
    );
}

#[test]
fn overseer_toml_is_in_bundle() {
    // `ALL` must include the overseer policy so `trusty-mpm install`
    // deploys it.
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "hooks/overseer.toml"),
        "overseer.toml must be a bundled artifact"
    );
}

#[test]
fn new_concrete_agents_are_in_bundle() {
    // Every newly-ported concrete agent must be present in ALL so
    // `trusty-mpm install` deploys them offline.
    // #9011: the agents ship as content, written by `tm install` from the roster.
    let agent_paths: Vec<String> = crate::core::content_source::test_support::repo_roster()
        .iter()
        .map(|(name, _)| format!("agents/{name}"))
        .collect();

    for expected in &[
        // Increment 1 agents
        "agents/qa.md",
        "agents/research.md",
        "agents/security.md",
        "agents/documentation.md",
        "agents/data-engineer.md",
        "agents/version-control.md",
        "agents/ticketing.md",
        "agents/code-analyzer.md",
        // Increment 2 language engineers
        "agents/python-engineer.md",
        "agents/typescript-engineer.md",
        "agents/golang-engineer.md",
        "agents/rust-engineer.md",
        "agents/java-engineer.md",
        "agents/php-engineer.md",
        "agents/ruby-engineer.md",
        "agents/react-engineer.md",
        "agents/nextjs-engineer.md",
        "agents/svelte-engineer.md",
        // Increment 2 QA variants
        "agents/web-qa.md",
        "agents/api-qa.md",
        // Increment 3 agents
        "agents/javascript-engineer.md",
        "agents/elixir-engineer.md",
        "agents/phoenix-engineer.md",
        "agents/dart-engineer.md",
        "agents/dotnet-engineer.md",
        "agents/tauri-engineer.md",
        "agents/web-ui-engineer.md",
        "agents/refactoring-engineer.md",
        "agents/prompt-engineer.md",
        "agents/code-critic.md",
        "agents/gcp-ops.md",
        "agents/vercel-ops.md",
        "agents/local-ops.md",
        "agents/memory-manager.md",
        "agents/mpm-agent-manager.md",
        "agents/mpm-skills-manager.md",
    ] {
        assert!(
            agent_paths.iter().any(|p| p == expected),
            "missing bundled agent: {expected}"
        );
    }
}

#[test]
fn new_concrete_agents_have_extends_in_frontmatter() {
    // Each new concrete agent must declare `extends:` so the inheritance
    // chain resolves correctly at deploy time.
    let agents = [
        // Increment 1 agents
        ("qa", agent("qa.md")),
        ("research", agent("research.md")),
        ("security", agent("security.md")),
        ("documentation", agent("documentation.md")),
        ("data-engineer", agent("data-engineer.md")),
        ("version-control", agent("version-control.md")),
        ("ticketing", agent("ticketing.md")),
        ("code-analyzer", agent("code-analyzer.md")),
        // Increment 2 language engineers
        ("python-engineer", agent("python-engineer.md")),
        ("typescript-engineer", agent("typescript-engineer.md")),
        ("golang-engineer", agent("golang-engineer.md")),
        ("rust-engineer", agent("rust-engineer.md")),
        ("java-engineer", agent("java-engineer.md")),
        ("php-engineer", agent("php-engineer.md")),
        ("ruby-engineer", agent("ruby-engineer.md")),
        ("react-engineer", agent("react-engineer.md")),
        ("nextjs-engineer", agent("nextjs-engineer.md")),
        ("svelte-engineer", agent("svelte-engineer.md")),
        // Increment 2 QA variants
        ("web-qa", agent("web-qa.md")),
        ("api-qa", agent("api-qa.md")),
        // Increment 3 agents
        ("javascript-engineer", agent("javascript-engineer.md")),
        ("elixir-engineer", agent("elixir-engineer.md")),
        ("phoenix-engineer", agent("phoenix-engineer.md")),
        ("dart-engineer", agent("dart-engineer.md")),
        ("dotnet-engineer", agent("dotnet-engineer.md")),
        ("tauri-engineer", agent("tauri-engineer.md")),
        ("web-ui-engineer", agent("web-ui-engineer.md")),
        ("refactoring-engineer", agent("refactoring-engineer.md")),
        ("prompt-engineer", agent("prompt-engineer.md")),
        ("code-critic", agent("code-critic.md")),
        ("gcp-ops", agent("gcp-ops.md")),
        ("vercel-ops", agent("vercel-ops.md")),
        ("local-ops", agent("local-ops.md")),
        ("memory-manager", agent("memory-manager.md")),
        ("mpm-agent-manager", agent("mpm-agent-manager.md")),
        ("mpm-skills-manager", agent("mpm-skills-manager.md")),
    ];
    for (name, content) in agents {
        assert!(
            content.contains("extends:"),
            "agent {name} is missing `extends:` in frontmatter"
        );
        // Composed output must not contain `extends:` (builder strips it).
        // We verify the raw source has it; compose round-trip is covered
        // by agent_deployer integration tests.
    }
}

#[test]
fn new_concrete_agents_deploy_via_real_asset_files() {
    // Verify each new agent composes without error using the real bundled
    // asset files on disk (not temp fixtures) — catches typos in `extends:`
    // values and missing base templates.
    use crate::core::agent_builder::compose_agent;

    let assets_dir = agent_assets_dir();

    let agents = [
        // Increment 1 agents
        "qa",
        "research",
        "security",
        "documentation",
        "data-engineer",
        "version-control",
        "ticketing",
        "code-analyzer",
        // Increment 2 language engineers
        "python-engineer",
        "typescript-engineer",
        "golang-engineer",
        "rust-engineer",
        "java-engineer",
        "php-engineer",
        "ruby-engineer",
        "react-engineer",
        "nextjs-engineer",
        "svelte-engineer",
        // Increment 2 QA variants
        "web-qa",
        "api-qa",
        // Increment 3 agents
        "javascript-engineer",
        "elixir-engineer",
        "phoenix-engineer",
        "dart-engineer",
        "dotnet-engineer",
        "tauri-engineer",
        "web-ui-engineer",
        "refactoring-engineer",
        "prompt-engineer",
        "code-critic",
        "gcp-ops",
        "vercel-ops",
        "local-ops",
        "memory-manager",
        "mpm-agent-manager",
        "mpm-skills-manager",
    ];

    for name in agents {
        let composed = compose_agent(name, assets_dir)
            .unwrap_or_else(|e| panic!("compose_agent({name}) failed: {e}"));
        // Composed output must have a frontmatter block.
        assert!(
            composed.starts_with("---\n"),
            "composed {name} is missing frontmatter"
        );
        // `extends:` must not leak into the composed output.
        assert!(
            !composed.contains("extends:"),
            "composed {name} has leaked `extends:` in output"
        );
        // Must have non-trivial body content.
        assert!(
            composed.len() > 200,
            "composed {name} suspiciously short ({} bytes)",
            composed.len()
        );
    }
}

#[test]
fn base_agent_guidance_sections_survive_composition() {
    // Regression for #2501/#2502/#2610/#4792: BASE-AGENT.md's "Finishing Work —
    // Push, Report, Stop" and "PM Directives Do Not Bind You" sections — plus
    // the version-control persona's own CI reinforcement — must propagate into
    // the composed agent via the real bundled asset chain, not just exist in the
    // source files. Uses version-control (the worst parking offender) composed
    // from the real assets dir so a future refactor of the compose/extends
    // pipeline can't silently drop the guidance.
    use crate::core::agent_builder::compose_agent;

    let assets_dir = agent_assets_dir();

    let composed = compose_agent("version-control", assets_dir)
        .expect("compose_agent(version-control) must succeed");

    // #10058: each rule is a marker in its source file, and the composed body
    // carries each chain link's body verbatim, which also guards the order.
    let base = agent("BASE-AGENT.md");
    for (rule, issue) in [
        ("base-agent/never-block-on-ci", "#4792"),
        ("base-agent/report-dont-promise", "#2610"),
        ("base-agent/pm-directives-scope", "#2502"),
        ("base-agent/no-background-monitor", "#2610"),
    ] {
        assert!(
            has_rule(base, rule),
            "BASE-AGENT.md is missing `{rule}` ({issue})"
        );
    }
    assert!(
        has_rule(agent("version-control.md"), "version-control/ci-waits"),
        "version-control.md is missing its persona-level CI section (#4792)"
    );
    assert!(
        has_rule(agent("local-ops.md"), "local-ops/long-waits"),
        "local-ops.md is missing its persona-level long-wait section (#2610)"
    );
    // #4792: the retired blocking wait must not come back through ANY layer.
    assert!(
        !composed.contains("--watch --fail-fast"),
        "composed version-control prescribes the retired blocking CI wait (#4792)"
    );
    for (stem, link) in [
        ("version-control", "BASE-AGENT.md"),
        ("version-control", "version-control.md"),
        ("local-ops", "BASE-AGENT.md"),
        ("local-ops", "local-ops.md"),
    ] {
        assert_composed_carries(stem, link);
    }
}

/// Why: #10058 — the composer drops rule markers, so a rule's presence is
/// checked in its source and its arrival in a composed agent structurally.
/// What: `link`'s body, frontmatter removed and markers stripped, appears
/// verbatim in `stem`'s composed body; that also pins the link's section order.
/// Test: `base_agent_guidance_sections_survive_composition`,
/// `pm_authority_doctrine_survives_composition`,
/// `base_agent_prose_rules_survive_composition`.
fn assert_composed_carries(stem: &str, link: &str) {
    use trusty_agents_common::agents::rule_marker::strip_rule_markers;

    let composed = crate::core::agent_builder::compose_agent(stem, agent_assets_dir())
        .unwrap_or_else(|e| panic!("compose_agent({stem}): {e}"));
    let source = agent(link);
    let body = source
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(source, |(_, body)| body);
    let body = strip_rule_markers(body);
    let body = body.trim_matches('\n');
    assert!(
        !body.is_empty() && composed.contains(body),
        "composed {stem} does not carry {link}'s body verbatim (#10058)"
    );
}

#[test]
fn no_mpm_guidance_skills_remain_in_bundle() {
    // The tm-skills-portfolio epic removed the 11 Phase 1 (#770) mpm-*
    // guidance skills in favor of the /tm- portfolio; assert none linger.
    let mpm_skill_paths: Vec<&str> = deployed()
        .into_iter()
        .filter(|a| a.rel_path.starts_with("skills/mpm-"))
        .map(|a| a.rel_path)
        .collect();
    assert!(
        mpm_skill_paths.is_empty(),
        "mpm-* guidance skills must be fully removed, found: {mpm_skill_paths:?}"
    );
}

#[test]
fn no_example_skill_remains_in_bundle() {
    // A4 (tm-skills-portfolio epic): the example-skill.md placeholder must
    // never reappear in ALL.
    assert!(
        !deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/example-skill.md"),
        "example-skill.md must not be present in ALL"
    );
}

#[test]
fn tm_doctor_skill_is_wired_into_bundle() {
    // A3 (tm-skills-portfolio epic): tm-doctor.md existed as an asset file
    // but was never wired into a const or the ALL table, so it silently
    // never shipped. Assert both are now true, and that it carries valid
    // frontmatter naming the skill `tm-doctor`.
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/tm-doctor.md"),
        "tm-doctor.md must be present in the ALL bundle table"
    );
    assert!(skill_text("skills/tm-doctor.md").starts_with("---\n"));
    assert!(skill_text("skills/tm-doctor.md").contains("name: tm-doctor"));
}

#[test]
fn code_critic_declared_skills_are_in_bundle() {
    // Issue #2890: code-critic's `skills:` frontmatter declares
    // `code-review-standards` and `contract-driven-testing`. Both asset files
    // existed under `src/assets/skills/` but were not wired into a const or
    // the `ALL` table on first cut — the exact historical tm-doctor.md bug
    // (see `tm_doctor_skill_is_wired_into_bundle` above and
    // `bundle_tm_skills.rs`'s module doc): a file existing on disk is not
    // sufficient for `deploy_all_skill_tiers` to ever see it, since that
    // function reads from the framework-root `skills/` directory, which is
    // populated exclusively from `ALL` entries at `tm install` time.
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/code-review-standards.md"),
        "code-review-standards.md must be present in the ALL bundle table"
    );
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/contract-driven-testing.md"),
        "contract-driven-testing.md must be present in the ALL bundle table"
    );
    assert!(skill_text("skills/code-review-standards.md").starts_with("---\n"));
    assert!(skill_text("skills/code-review-standards.md").contains("name: code-review-standards"));
    assert!(skill_text("skills/contract-driven-testing.md").starts_with("---\n"));
    assert!(
        skill_text("skills/contract-driven-testing.md").contains("name: contract-driven-testing")
    );
    // The agent's declared `skills:` frontmatter must name both of these,
    // so the DOC-42 co-deployment mechanism has something real to resolve —
    // issue #2903 extended the declaration with four more batch-1 skills
    // code-critic's upstream counterpart also names (code-production-process,
    // software-patterns, systematic-debugging, verification-before-completion),
    // so this only asserts the two #2890-era names are still present, not the
    // full six-skill list (that full-list assertion lives in
    // `code_critic_declares_batch1_skills`).
    assert!(agent("code-critic.md").contains("code-review-standards"));
    assert!(agent("code-critic.md").contains("contract-driven-testing"));
    assert!(agent("code-critic.md").contains("skills: ["));
}

#[test]
fn code_review_standards_flags_github_token_protected_branch_pushes_8016() {
    // Issue #8016: `code-review-standards` had no check for a workflow
    // granted `contents: write` that pushes to a protected branch using
    // `GITHUB_TOKEN` — GitHub never allows the Actions identity as a
    // restriction or ruleset bypass actor, so the workflow is unworkable no
    // matter how green its first (nothing-to-publish) run looked.
    assert!(
        has_rule(
            skill_text("skills/code-review-standards.md"),
            "code-review-standards/github-token-protected-push"
        ),
        "code-review-standards must flag a GITHUB_TOKEN push to a protected \
         branch (#8016)"
    );
}

#[test]
fn code_review_standards_flags_format_interpolated_structured_payloads_7624() {
    // Issue #7624: `code-review-standards` had no check for a test helper
    // that builds a structured (JSON/YAML/TOML/SQL) payload by `format!`
    // interpolating a caller-supplied value into a string literal instead of
    // the format's own builder/serializer — the #7550 failure mode, where a
    // caller value containing the format's escape/delimiter characters
    // malformed the payload and the system under test answered it with a
    // permissive default indistinguishable from a real pass.
    assert!(
        has_rule(
            skill_text("skills/code-review-standards.md"),
            "code-review-standards/format-interpolated-payload"
        ),
        "code-review-standards must flag format!-interpolated structured \
         test payloads (#7624)"
    );
}

#[test]
fn code_critic_declares_batch1_skills() {
    // Issue #2903 ported the full upstream six-skill set. #4642 cuts it back to
    // the two that code-critic's own body tells it to load on turn one: the
    // harness renders every listed skill's FULL body into the prompt at
    // dispatch, so the other four (code-production-process, software-patterns,
    // systematic-debugging, verification-before-completion) were 30 KB of
    // reference material paid on every review. They remain bundled and
    // invokable via the Skill tool; only the resident declaration shrank.
    assert!(
        agent("code-critic.md")
            .contains("skills: [code-review-standards, contract-driven-testing]"),
        "code-critic's resident skills are its rubric and its contract-test \
         reference, nothing else (#4642)"
    );
}

#[test]
fn skill_port_batch1_skills_are_in_bundle() {
    // Issue #2903 (epic #2902): all 25 batch-1 universal/ skill entry points
    // must be present in `ALL` — mirrors `tm_doctor_skill_is_wired_into_bundle`
    // and `code_critic_declared_skills_are_in_bundle` above: a source file
    // existing under `src/assets/skills/` is not sufficient, only `ALL`
    // registration makes `deploy_all_skill_tiers` ship it.
    const BATCH1_ENTRIES: &[&str] = &[
        "skills/systematic-debugging.md",
        "skills/verification-before-completion.md",
        "skills/git-workflow.md",
        "skills/test-driven-development.md",
        "skills/requesting-code-review.md",
        "skills/writing-plans.md",
        "skills/json-data-handling.md",
        "skills/root-cause-tracing.md",
        "skills/internal-comms.md",
        "skills/brainstorming.md",
        "skills/software-patterns.md",
        "skills/security-scanning.md",
        "skills/api-design-patterns.md",
        "skills/database-migration.md",
        "skills/env-manager.md",
        "skills/web-performance-optimization.md",
        "skills/artifacts-builder.md",
        "skills/condition-based-waiting.md",
        "skills/model-context-builder.md",
        "skills/test-quality-inspector.md",
        "skills/testing-anti-patterns.md",
        "skills/webapp-testing.md",
        "skills/api-documentation.md",
        "skills/xlsx.md",
        "skills/code-production-process.md",
    ];
    assert_eq!(BATCH1_ENTRIES.len(), 25);
    for rel_path in BATCH1_ENTRIES {
        assert!(
            deployed().into_iter().any(|a| &a.rel_path == rel_path),
            "{rel_path} must be present in the ALL bundle table"
        );
    }
}

#[test]
fn skill_port_batch1_references_land_on_disk() {
    // A sample of multi-file batch-1 skills must carry their references/*.md
    // files as SEPARATE `ALL` entries alongside the entry-point SKILL.md — the
    // multi-file skill-directory extension this issue adds to the bundle/
    // deploy machinery (epic #2902 constraint #2). Real deploy-reachability
    // (landing under a deployed `.claude/skills/<name>/references/`) is
    // proven end-to-end in `tests_behavior_2903_skills_tests.rs`.
    for (entry, refs) in [
        (
            "skills/systematic-debugging.md",
            &[
                "skills/systematic-debugging/references/anti-patterns.md",
                "skills/systematic-debugging/references/examples.md",
                "skills/systematic-debugging/references/troubleshooting.md",
                "skills/systematic-debugging/references/workflow.md",
            ][..],
        ),
        (
            "skills/software-patterns.md",
            &[
                "skills/software-patterns/references/anti-patterns.md",
                "skills/software-patterns/references/code-smell-signals.md",
                "skills/software-patterns/references/decision-trees.md",
                "skills/software-patterns/references/examples.md",
                "skills/software-patterns/references/foundational-patterns.md",
                "skills/software-patterns/references/situational-patterns.md",
            ][..],
        ),
    ] {
        assert!(
            deployed().into_iter().any(|a| a.rel_path == entry),
            "{entry} must be present in the ALL bundle table"
        );
        for rel_path in refs {
            assert!(
                deployed().into_iter().any(|a| &a.rel_path == rel_path),
                "{rel_path} must be present in the ALL bundle table"
            );
        }
    }
}

#[test]
fn what_is_trusty_mpm_is_in_bundle() {
    // DOC-28 R1 acceptance: the canonical self-description doc must be a
    // bundled artifact so `tm install` deploys it to every framework root,
    // not just the trusty-tools repo's own source tree.
    let artifact = deployed()
        .into_iter()
        .find(|a| a.rel_path == "docs/WHAT-IS-TRUSTY-MPM.md")
        .expect("WHAT-IS-TRUSTY-MPM.md must be a bundled artifact");
    assert_eq!(
        artifact.install,
        InstallPolicy::Overwrite,
        "the doc is framework-owned and must track upgrades"
    );
    assert_eq!(
        artifact.contents,
        instruction_text("docs/WHAT-IS-TRUSTY-MPM.md")
    );
}

#[test]
fn what_is_trusty_mpm_disambiguates_claude_mpm() {
    // DOC-28 R1 acceptance: the doc identifies the project (Rust, tm, tcode)
    // and explicitly disambiguates it from the unrelated Python claude-mpm
    // package. #10058: that statement is pinned by its marker.
    assert!(
        has_rule(
            instruction_text("docs/WHAT-IS-TRUSTY-MPM.md"),
            "what-is-trusty-mpm/not-claude-mpm"
        ),
        "doc must explicitly disambiguate from claude-mpm, not mention it incidentally"
    );
}

#[test]
fn idle_park_mitigation_2833_guidance_survives_composition() {
    // Regression for #2833's code-critic review (MEDIUM finding): the prior
    // `base_agent_guidance_sections_survive_composition` test only asserted the
    // pre-existing #2501/#2610 strings — none of the #2833 idle-park-mitigation
    // content (the BASE-AGENT hand-back guidance, the PM-side re-engagement
    // protocol, the version-control persona bullets) was ever exercised
    // through the REAL bundled asset chain. This test closes that gap using the
    // same real-composition approach as its sibling: `compose_agent` reading
    // the actual `assets/agents` dir for the two agent-tier assertions, and
    // `assemble_system_prompt` (the real PM_INSTRUCTIONS/WORKFLOW/
    // AGENT_DELEGATION/BASE_PM assembly used at session launch) for the
    // PM-tier assertion — not a hand-built fixture string.
    use crate::core::instruction_pipeline::assemble_system_prompt;

    // #10058: each rule is a marker in its source; composition is covered by
    // `base_agent_guidance_sections_survive_composition`.
    // (a) BASE-AGENT.md's report-don't-promise subsection (which replaced the
    // retired chunked-repoll guidance in #4792), and (c) the update-branch
    // treadmill bullet.
    for rule in [
        "base-agent/report-dont-promise",
        "base-agent/update-branch-treadmill",
    ] {
        assert!(
            has_rule(agent("BASE-AGENT.md"), rule),
            "BASE-AGENT.md is missing `{rule}` (#4792)"
        );
    }

    // (b) The Parked-Subagent Re-Engagement TRIGGER must survive the real PM
    // system-prompt assembly (not just exist in the source .md file). The
    // trigger is what cannot be deferred: an agent handing back with CI pending
    // is the moment the PM must act, and a PM that never learns to act does not
    // know to load anything. #4969: the MECHANICS moved to
    // `tm-delegation-patterns`, so the resident text names the Skill call; the
    // section's marker pins that pointer.
    let section = instruction_text("sections/subagent-re-engagement.md");
    assert!(
        has_rule(
            section,
            "subagent-re-engagement/resume-via-pm-re-engagement"
        ),
        "the resident re-engagement trigger must name the Skill call that \
         carries the mechanics (#2833, #4792)"
    );
    let heading = section.lines().next().expect("the section's heading");
    let pm_prompt = assemble_system_prompt(rc());
    assert!(
        pm_prompt.contains(heading),
        "assembled PM system prompt is missing the section `{heading}` (#2833, #4792)"
    );
    assert!(
        !pm_prompt.contains("never a 30-second blind poll"),
        "the anti-spam MECHANICS belong to the skill now; re-inlining them in \
         the prompt is the redundancy this rule removes (#2833)"
    );

    // …and the mechanics must actually BE in the skill, or the pointer above is
    // a dangling reference. Asserted against the real bundled asset.
    // #10055: the mechanics may live in references/; the description below is
    // frontmatter, so it reads the entry file alone.
    let skill = skill_with_references("skills/tm-delegation-patterns.md");
    for rule in [
        "tm-delegation-patterns/no-blind-poll",
        "tm-delegation-patterns/pm-re-engagement",
    ] {
        assert!(
            has_rule(skill, rule),
            "tm-delegation-patterns must carry `{rule}`, which the prompt points at (#2833)"
        );
    }
    // The skill has to be REACHABLE by relevance too: its description is what a
    // relevance match sees, and "delegation matrices" alone never fires at the
    // moment an agent hands back with pending CI.
    let entry = skill_text("skills/tm-delegation-patterns.md");
    assert!(
        entry.contains("CI pending") && entry.contains("re-engagement"),
        "tm-delegation-patterns' frontmatter description must mention the \
         CI-wait / re-engagement trigger, or it cannot fire on its own (#4792)"
    );
}

/// 🔴 #8004 REGRESSION: PM Re-Engagement said `SendMessage` the same agent
/// unconditionally. A worktree-isolated subagent's clean tree was reclaimed
/// between turns, so the resumed agent landed in the main checkout, where
/// ADR-0061/#5649 forbid it from committing.
#[test]
fn pm_re_engagement_checks_worktree_survival_before_resuming_8004() {
    // #10058: the check (`git worktree list`, re-dispatch with isolation, never
    // into the main checkout, every resume) and the Retry Protocol's pointer to
    // it are pinned by their markers.
    for rule in [
        "tm-delegation-patterns/worktree-check-first",
        "tm-delegation-patterns/tree-must-exist-before-retry",
    ] {
        assert!(
            has_rule(
                skill_with_references("skills/tm-delegation-patterns.md"),
                rule
            ),
            "tm-delegation-patterns is missing the #8004 worktree-survival check `{rule}`"
        );
    }
}

/// 🔴 #8004 recurrence: the resident PM section routed only CI-pending and
/// goal-unmet hand-backs to PM Re-Engagement, so a resume after an owner
/// ruling or a released HOLD skipped the worktree-survival check.
#[test]
fn resident_re_engagement_routes_every_resume_to_the_worktree_check_8004() {
    // #10058: pinned by the section's marker, not its wording.
    assert!(
        has_rule(
            instruction_text("sections/subagent-re-engagement.md"),
            "subagent-re-engagement/resume-via-pm-re-engagement"
        ),
        "subagent-re-engagement.md must route every resume to the #8004 worktree check"
    );
}

#[test]
fn pm_authority_doctrine_survives_composition() {
    // Regression for the live refusal where a version-control agent treated a
    // PM-relayed operator authorization as an untrusted third party's word and
    // froze an authorized admin-merge. The doctrine — PM relays operator
    // authority; doubt escalates back to the PM; only OBJECTIVE gates (red/
    // pending CI, fabricated evidence, worktree discipline) stay the agent's
    // own non-negotiables — must reach a composed agent through the REAL
    // bundled asset chain, not just live in the source .md.
    // #10058: each half is a marker in its source file — (a) BASE-AGENT's PM
    // Authority & Escalation (the anti-distrust sentence, the injection-
    // skepticism scope, the report-back path), (b) version-control's own
    // relayed-merge authority with its `--admin` limit, (c) the objective-
    // safety gate — and the composed body carries both sources verbatim.
    for rule in [
        "base-agent/pm-authority",
        "base-agent/injection-skepticism-scope",
        "base-agent/authority-and-objective-safety",
    ] {
        assert!(
            has_rule(agent("BASE-AGENT.md"), rule),
            "BASE-AGENT.md is missing `{rule}`"
        );
    }
    assert!(
        has_rule(
            agent("version-control.md"),
            "version-control/pm-relayed-merge-authority"
        ),
        "version-control.md is missing its PR-workflow authority text"
    );
    assert_composed_carries("version-control", "BASE-AGENT.md");
    assert_composed_carries("version-control", "version-control.md");
}

#[test]
fn documentation_style_skill_is_in_bundle() {
    // Issue #2911: the documentation-style entry SKILL.md must be present in
    // `ALL` — mirrors `tm_doctor_skill_is_wired_into_bundle` and
    // `skill_port_batch1_skills_are_in_bundle` above: a source file existing
    // under `src/assets/skills/` is not sufficient, only `ALL` registration
    // makes `deploy_all_skill_tiers` ship it.
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/documentation-style.md"),
        "documentation-style.md must be present in the ALL bundle table"
    );
    assert!(skill_text("skills/documentation-style.md").starts_with("---\n"));
    assert!(skill_text("skills/documentation-style.md").contains("name: documentation-style"));
    // SLD self-compliance (DOC-38 §2.5): the entry SKILL.md carries
    // `spec_refs:` frontmatter pointing at the grammar section it defers to,
    // with `anchor` equal to `id` per DOC-38's self-check rule.
    assert!(skill_text("skills/documentation-style.md").contains("spec_refs:"));
    assert!(skill_text("skills/documentation-style.md").contains("id: SPEC-SLD-02~draft"));
    assert!(skill_text("skills/documentation-style.md").contains("anchor: SPEC-SLD-02~draft"));
}

#[test]
fn documentation_style_references_land_on_disk() {
    // Issue #2911: all 6 references/*.md files must be SEPARATE `ALL` entries
    // alongside the entry-point SKILL.md — mirrors
    // `skill_port_batch1_references_land_on_disk`. Real deploy-reachability
    // (landing under a deployed `.claude/skills/documentation-style/references/`)
    // is proven end-to-end in `tests_behavior_2911_documentation_style_tests.rs`.
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/documentation-style.md")
    );
    for rel_path in [
        "skills/documentation-style/references/spec.md",
        "skills/documentation-style/references/readme.md",
        "skills/documentation-style/references/file-level.md",
        "skills/documentation-style/references/class.md",
        "skills/documentation-style/references/method-function.md",
        "skills/documentation-style/references/block-inline.md",
    ] {
        assert!(
            deployed().into_iter().any(|a| a.rel_path == rel_path),
            "{rel_path} must be present in the ALL bundle table"
        );
    }
}

#[test]
fn documentation_style_unions_into_engineer_family_via_base_engineer() {
    // Issue #2911: BASE-ENGINEER.md now declares `skills: [documentation-style]`;
    // compose_agent's DOC-42 union-across-chain merge (builder.rs
    // `merge_frontmatter`) must propagate it into every concrete engineer-family
    // agent composed from the REAL bundled asset files, not a synthetic fixture.
    use crate::core::agent_builder::compose_agent;

    let assets_dir = agent_assets_dir();

    let composed = compose_agent("rust-engineer", assets_dir)
        .expect("compose_agent(rust-engineer) must succeed");
    assert!(
        composed.contains("documentation-style"),
        "composed rust-engineer is missing the BASE-ENGINEER-declared \
         documentation-style skill (union-across-chain, DOC-42): {composed}"
    );
}

#[test]
fn rust_build_performance_skill_is_in_bundle() {
    // Per Bob directive 2026-07-17: the rust-build-performance entry SKILL.md
    // must be present in `ALL` — mirrors `documentation_style_skill_is_in_bundle`:
    // a source file existing under `src/assets/skills/` is not sufficient,
    // only `ALL` registration makes `deploy_all_skill_tiers` ship it.
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/rust-build-performance.md"),
        "rust-build-performance.md must be present in the ALL bundle table"
    );
    assert!(skill_text("skills/rust-build-performance.md").starts_with("---\n"));
    assert!(
        skill_text("skills/rust-build-performance.md").contains("name: rust-build-performance")
    );
}

#[test]
fn rust_delivery_workflow_skill_is_in_bundle() {
    // Issue #8192: same reachability contract as the sibling above — the asset
    // file under `src/assets/skills/` only ships once `ALL` names it.
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/rust-delivery-workflow.md"),
        "rust-delivery-workflow.md must be present in the ALL bundle table"
    );
    assert!(skill_text("skills/rust-delivery-workflow.md").starts_with("---\n"));
    assert!(
        skill_text("skills/rust-delivery-workflow.md").contains("name: rust-delivery-workflow")
    );
}

#[test]
fn rust_delivery_workflow_states_each_delivery_rule() {
    // Issue #8192: the skill exists to carry rules that previously lived only
    // in incident prose. A body that dropped one would still pass the
    // reachability test above, so pin the rule set itself — one needle per
    // section that a rewrite must not silently lose.
    // #10058: one marker per rule, so a rewrite may reword any of them.
    for (fact, rule) in [
        (
            "it defers build speed to the sibling skill",
            "rust-delivery-workflow/defers-to-sibling-skills",
        ),
        (
            "the gate chain is preceded by a commit",
            "rust-delivery-workflow/commit-before-gates",
        ),
        (
            "CI's workflow file is the clippy pin's source",
            "rust-delivery-workflow/ci-clippy-pin",
        ),
        (
            "a crate-scoped local pass is not CI evidence",
            "rust-delivery-workflow/crate-scope-is-not-ci-evidence",
        ),
        (
            "concurrent builds contend for RAM, not just the build lock",
            "rust-delivery-workflow/builds-are-cpu-and-ram-bound",
        ),
        (
            "the job cap is read off the machine",
            "rust-delivery-workflow/job-cap-from-the-machine",
        ),
        (
            "env must be prefixed inline per command",
            "rust-delivery-workflow/env-inline-per-command",
        ),
        (
            "gate output is a verdict",
            "rust-delivery-workflow/gate-output-is-a-verdict",
        ),
        (
            "sccache is recommended, never silently enabled",
            "rust-delivery-workflow/sccache-never-silent",
        ),
        (
            "installs batch across a merge batch",
            "rust-delivery-workflow/batch-installs",
        ),
        (
            "test scope widens by stage",
            "rust-delivery-workflow/test-scope-by-stage",
        ),
    ] {
        assert!(
            has_rule(skill_text("skills/rust-delivery-workflow.md"), rule),
            "`rust-delivery-workflow.md` must state that {fact} (#8192)"
        );
    }
}

#[test]
fn self_improvement_loop_skill_is_in_bundle() {
    // Issue #7723: the self-improvement-loop entry SKILL.md must be present
    // in `ALL` — a source file existing under `src/assets/skills/` is not
    // sufficient, only `ALL` registration makes `deploy_all_skill_tiers`
    // ship it (the historical orphaned-tm-doctor.md bug).
    assert!(
        deployed()
            .into_iter()
            .any(|a| a.rel_path == "skills/self-improvement-loop.md"),
        "self-improvement-loop.md must be present in the ALL bundle table"
    );
    assert!(skill_text("skills/self-improvement-loop.md").starts_with("---\n"));
    assert!(skill_text("skills/self-improvement-loop.md").contains("name: self-improvement-loop"));
}

#[test]
fn self_improvement_loop_skill_carries_the_moved_anchors() {
    // Issue #7723: `self_analysis_reporting_ships_in_the_base_agent`
    // (`trusty-agents-common::agent_assets`) now asserts only the resident
    // trigger and the skill name in `BASE-AGENT.md`; this test asserts the
    // ten anchors that section used to carry directly, against the skill
    // they moved into.
    // #10058: one marker per anchor, so a rewrite may reword any of them.
    for (fact, rule) in [
        (
            "the section exists",
            "self-improvement-loop/reporting-section",
        ),
        (
            "the fast-loop section exists",
            "self-improvement-loop/fast-loop-section",
        ),
        (
            "the detection heuristics are headed and findable",
            "self-improvement-loop/detection-heuristics",
        ),
        (
            "the one memory tag is named",
            "self-improvement-loop/hypothesis-tag",
        ),
        (
            "the post-mortem's query is spelled out",
            "self-improvement-loop/post-mortem-query",
        ),
        (
            "trusty-tools is the fixed destination",
            "self-improvement-loop/fixed-destination",
        ),
        (
            "a subagent returns a block instead of filing",
            "self-improvement-loop/return-a-block-not-a-filing",
        ),
        (
            "dedup runs against the label",
            "self-improvement-loop/dedup-by-label",
        ),
        (
            "a clean run files nothing",
            "self-improvement-loop/clean-run-reports-nothing",
        ),
        (
            "the post-mortem tracker is named",
            "self-improvement-loop/post-mortem-tracker",
        ),
    ] {
        assert!(
            has_rule(skill_text("skills/self-improvement-loop.md"), rule),
            "`self-improvement-loop.md` must state that {fact} (#6935, #6937, #7723)"
        );
    }
}

#[test]
fn rust_build_performance_declared_by_rust_family_agents() {
    // Per Bob directive 2026-07-17: rust-engineer and tauri-engineer both
    // declare `rust-build-performance` directly in their own `skills:`
    // frontmatter (NOT via BASE-ENGINEER — not every engineer compiles
    // Rust), proven against the REAL bundled asset files via compose_agent,
    // mirroring `documentation_style_unions_into_engineer_family_via_base_engineer`.
    use crate::core::agent_builder::compose_agent;

    let assets_dir = agent_assets_dir();

    for agent in ["rust-engineer", "tauri-engineer"] {
        let composed = compose_agent(agent, assets_dir)
            .unwrap_or_else(|e| panic!("compose_agent({agent}) must succeed: {e}"));
        assert!(
            composed.contains("rust-build-performance"),
            "composed {agent} is missing its own declared \
             rust-build-performance skill: {composed}"
        );
    }
}

#[test]
fn rust_engineer_declares_delivery_workflow_and_names_no_absent_skill() {
    // Issue #8192, two halves of one contract. (1) `rust-engineer` declares
    // `rust-delivery-workflow` in its own `skills:` (not via BASE-ENGINEER —
    // no other engineer runs cargo gates). (2) The body's first-action
    // instruction no longer names `toolchains-rust-core`, which ships in no
    // bundle: #2904 claimed to port it and never did, so every dispatched
    // rust-engineer was told to load a skill that does not exist.
    use crate::core::agent_builder::compose_agent;

    let assets_dir = agent_assets_dir();
    let composed = compose_agent("rust-engineer", assets_dir)
        .expect("compose_agent(rust-engineer) must succeed");

    assert!(
        composed.contains("rust-delivery-workflow"),
        "composed rust-engineer is missing its own declared \
         rust-delivery-workflow skill: {composed}"
    );
    assert!(
        !composed.contains("toolchains-rust-core"),
        "composed rust-engineer still names `toolchains-rust-core`, a skill \
         present in no bundle (#8192)"
    );
}

#[test]
fn tm_init_surfaces_build_performance_pointer_for_rust_projects() {
    // The `rust-build-performance` skill (per-agent, resident in
    // `rust-engineer`/`tauri-engineer` — see
    // `rust_build_performance_declared_by_rust_family_agents` above) only
    // reaches a session that dispatches one of those agents. Nothing at
    // PROJECT-SETUP time surfaced it before a build already felt slow — this
    // test pins the `/tm-init` fix: a Rust project's scaffolded/refreshed
    // `CLAUDE.md` MUST get a durable Build Performance pointer, and that
    // pointer must not assert a benefit the in-flight sccache measurement
    // has not established.
    // #10058: the pointer (keyed off `Cargo.toml`, naming the skill), the
    // `--timings` baseline and the sccache caveat are pinned by markers.
    for (fact, rule) in [
        (
            "a Rust project's CLAUDE.md gets a Build Performance pointer",
            "tm-init/build-performance-section",
        ),
        (
            "a new Rust project measures a baseline with --timings first",
            "tm-init/timings-baseline",
        ),
        (
            "a scaffolded CLAUDE.md never asserts an unmeasured sccache speedup",
            "tm-init/no-unmeasured-sccache-claim",
        ),
    ] {
        assert!(
            has_rule(skill_text("skills/tm-init.md"), rule),
            "tm-init must state that {fact} (`{rule}`)"
        );
    }
}

#[test]
fn output_styles_keep_claude_code_coding_instructions() {
    // Claude Code strips its built-in software-engineering instructions (how to
    // scope changes, write comments, verify work) from any custom output style
    // unless `keep-coding-instructions` is true — the field defaults to FALSE.
    // All three PM styles are PM-orchestration styles layered ON TOP of
    // normal coding behaviour, so all three must opt back in.
    let frontmatter = |style: &BundledStyle| {
        style
            .content(rc())
            .split("---")
            .nth(1)
            .unwrap_or_else(|| panic!("{} must open with a YAML frontmatter block", style.id))
            .to_string()
    };
    for style in pm_output_styles() {
        assert!(
            frontmatter(style).contains("keep-coding-instructions: true"),
            "{} frontmatter must set `keep-coding-instructions: true`, or Claude \
             Code silently drops its built-in coding instructions",
            style.id
        );
    }
    // #8453: the supervisor is not a coder — its style drops them on purpose,
    // as the reference supervisor's style did.
    let architect = OUTPUT_STYLES
        .iter()
        .find(|s| s.id == crate::core::session_profile::ARCHITECT_WRITTEN_OUTPUT_STYLE_ID)
        .expect("the supervisor style is bundled");
    assert!(frontmatter(architect).contains("keep-coding-instructions: false"));
}

#[test]
fn output_styles_state_every_pm_prose_rule() {
    // #2647: the output style is the only channel that survives a manual
    // `claude` launch with no tm-appended system prompt, so every PM prose rule
    // is stated RESIDENT here rather than referenced. #7423 kept the rules and
    // moved their examples behind `Skill(skill="tm-prose-style")`.
    // #10058: a style is delivered verbatim on every PM turn and the floor is
    // cut from it, so it carries no rule-id markers. Each PM style must carry
    // the floor sections the production cut names (`FLOOR_SECTIONS`); the
    // rules inside are model behaviour, listed for the #10061 evals. The
    // supervisor's section is pinned by
    // `the_architect_style_carries_write_plainly_verbatim`.
    use crate::core::output_style::FLOOR_SECTIONS;

    for style in pm_output_styles() {
        for heading in FLOOR_SECTIONS {
            assert!(
                // The teacher and research styles qualify the directive's
                // heading ("(still absolute)"), so a prefix match.
                style
                    .content(rc())
                    .lines()
                    .any(|line| line.starts_with(heading)),
                "{} is missing the floor section `{heading}` (#2647, #8533)",
                style.id
            );
        }
        // The superseded lighter rule set must be FOLDED IN, not left alongside
        // the mirrored rules as a second overlapping statement of the same thing.
        assert!(
            !style
                .content(rc())
                .contains("**Tone**: Professional, neutral"),
            "{}: the old Communication block must be folded into the mirrored \
             prose rules, not duplicated beside them",
            style.id
        );
    }
}

#[test]
fn the_borrowed_metaphor_ban_reaches_both_prose_channels() {
    // The owner banned "load-bearing" on 2026-08-10 as an instance of a
    // category. It has to bind PM prose (the output styles, #2647) and agent
    // prose (BASE-AGENT.md composition) alike. #7423 split the two halves: the
    // RULE stays resident in both channels, and the category statement that
    // keeps it from degenerating into a phrase list moved to `tm-prose-style`.
    // #10058: both channels' sections are pinned by
    // `output_styles_state_every_pm_prose_rule` and
    // `base_agent_prose_rules_survive_composition`; the category statement
    // by its marker here.
    assert!(
        has_rule(
            skill_text("skills/tm-prose-style.md"),
            "tm-prose-style/borrowed-metaphor-category"
        ),
        "tm-prose-style must carry the borrowed-metaphor category statement"
    );
}

#[test]
fn the_honest_ban_reaches_both_prose_channels() {
    // The word ban used to live only in the manifest block the composed prompt
    // carries, so a manual `claude` launch never received it and the output
    // styles pointed at a rule they did not state. It now states the rule, in
    // every position the word can take — the owner's flagged instance was the
    // heading modifier "Distribution, stated honestly", not a bare "honest".
    // #10058: both channels' sections are pinned elsewhere (see the borrowed-
    // metaphor test); every position the word can take, moved to
    // `tm-prose-style` in #7423, by its marker here.
    assert!(
        has_rule(
            skill_text("skills/tm-prose-style.md"),
            "tm-prose-style/honest-ban-positions"
        ),
        "tm-prose-style must carry the \"honest\" ban's positions"
    );
}

#[test]
fn the_asd_ste_100_layer_reaches_both_prose_channels() {
    // The owner added ASD-STE-100 as a sentence-CONSTRUCTION layer on top of
    // the existing stance rules, in spirit only: the ~900-word approved
    // vocabulary is deliberately NOT adopted, because it forbids common verbs
    // and would make trade-off discussion stilted. Both halves are asserted —
    // the layer's presence, and the refusal that keeps a future reader from
    // "fixing" it into literal conformance.
    // #10058: both channels' sections are pinned elsewhere (see the borrowed-
    // metaphor test); the refusal and the construction rules, in
    // `tm-prose-style` since #7423, by their markers here.
    for rule in [
        "tm-prose-style/asd-ste-100-spirit-only",
        "tm-prose-style/asd-ste-100-rules",
    ] {
        assert!(
            has_rule(skill_text("skills/tm-prose-style.md"), rule),
            "tm-prose-style must carry the ASD-STE-100 detail `{rule}`"
        );
    }
    for style in OUTPUT_STYLES {
        // The superseded bullet was FOLDED IN, not left beside the layer as a
        // second statement of "one idea per sentence" in different words.
        assert!(
            !style
                .content(rc())
                .contains("Short sentences, one idea each"),
            "{}: the old short-sentence bullet must be folded into the \
             ASD-STE-100 layer, not duplicated beside it",
            style.id
        );
    }
}

#[test]
fn the_default_output_style_stays_within_its_resident_budget() {
    // #7423: the style is resident in every PM turn, so its size is a cost paid
    // continuously. #8999 lowered the budget from 8 KB to the size its trim
    // landed under — a rule added here must displace text, not extend the file.
    // Examples and inventories belong in `tm-prose-style`, which loads on demand.
    const BUDGET_BYTES: usize = 6_950;
    assert!(
        instruction_text("output-styles/trusty-mpm.md").len() <= BUDGET_BYTES,
        "output style `{DEFAULT_OUTPUT_STYLE_ID}` is {} bytes, over the \
         {BUDGET_BYTES}-byte resident budget — move examples into \
         `tm-prose-style` rather than growing it",
        instruction_text("output-styles/trusty-mpm.md").len()
    );
}

#[test]
fn output_styles_name_the_todowrite_fallback() {
    // #2799: every bundled style used to mandate `TodoWrite` unconditionally, so
    // a PM on a harness that does not expose it announced the gap and improvised
    // its own tracking. The section must state the CONDITION and the fallback;
    // pinning both halves keeps a future trim from dropping one of them.
    // #8453: a PM section; the supervisor style carries no TodoWrite mandate.
    // #10058: an output style carries no rule-id markers (it is delivered
    // verbatim every PM turn), so these two pins stay until the #10061 eval
    // that replaces them lands.
    for style in pm_output_styles() {
        assert!(
            style.content(rc()).contains("where the harness exposes it"),
            "{}: the TodoWrite section must make the tool conditional (#2799)",
            style.id
        );
        assert!(
            style.content(rc()).contains("prose task list"),
            "{}: the TodoWrite section must name the prose fallback (#2799)",
            style.id
        );
    }
}

/// The resident budget every deployed agent body is measured against.
///
/// Issue #7723 (epic #7681): composed rust-engineer (BASE-AGENT +
/// BASE-ENGINEER + rust-engineer body) measured 55,872 bytes on 2026-09-13,
/// 19,981 tokens per turn (32% of an engineer subagent's floor, the single
/// largest component — `docs/research/token-floor-measurement-2026-09-13.md`).
/// The #7723 trim target was ~40,000; 42,000 leaves headroom without licensing
/// the file to regrow back toward its pre-trim size.
const RESIDENT_BODY_BUDGET_BYTES: usize = 42_000;

/// Bodies already over [`RESIDENT_BODY_BUDGET_BYTES`] when #7825 widened the
/// gate from rust-engineer alone to every deployed agent, each pinned at a
/// ratchet ceiling with the issue tracking its trim.
///
/// Why: #7825 named four; widening the gate surfaced four more on
/// 2026-09-15 — java, nextjs, python and svelte engineer were over too, each
/// by under 600 bytes; #8047 trimmed all four back under the default budget,
/// so their entries are gone. Raising the budget for everyone would hide the
/// rest; excluding them would drop the coverage the issue asked for. A
/// per-stem ceiling keeps each one measured and lets it shrink but never grow.
/// What: `(stem, ceiling_bytes, tracking_issue)`. The ceiling is the measured
/// size rounded up to the next 500 bytes, so an unrelated BASE-AGENT edit does
/// not flip the gate while real regrowth still does. An entry whose body has
/// fallen back under the default budget fails the gate as stale — that is how
/// the allowlist empties itself.
/// Test: `every_deployed_agent_composed_body_stays_within_its_resident_budget`.
const OVER_BUDGET_BODY_BASELINES: &[(&str, usize, &str)] = &[
    ("dotnet-engineer", 43_500, "#7825"),
    ("elixir-engineer", 44_000, "#7825"),
    ("ticketing", 47_000, "#7727"),
    ("version-control", 47_500, "#7727"),
];

/// The bundled agent assets directory the composer reads.
fn agent_assets_dir() -> &'static std::path::Path {
    // #9011: the roster's checkout directory, `content/agents`.
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(crate::core::content_source::test_support::repo_agents_dir)
}

/// One roster file's body from the checkout roster (#9011), which replaced the
/// compiled-in `*_AGENT` consts.
fn agent(file_name: &str) -> &'static str {
    static ROSTER: std::sync::OnceLock<crate::core::content_source::AgentRoster> =
        std::sync::OnceLock::new();
    ROSTER
        .get_or_init(crate::core::content_source::test_support::repo_roster)
        .require(file_name)
        .unwrap_or_else(|e| panic!("{e}"))
}

/// Every deployed (non-foundation) bundled agent stem, sorted.
fn deployed_agent_stems() -> Vec<String> {
    let mut stems: Vec<String> = std::fs::read_dir(agent_assets_dir())
        .expect("bundled agent assets dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
        .filter_map(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .filter(|stem| !crate::core::delegation_authority::is_foundation_file(stem))
        .collect();
    stems.sort();
    stems
}

/// Per-asset byte ledger for one composed agent body, base-first.
///
/// Why: #7915 — the budget assertion below reported one total, so a one-line
/// addition anywhere in the `extends:` chain turned the gate red with no
/// pointer to which file grew. The ledger names the contributors.
/// What: walks the same `extends:` chain [`compose_agent`] resolves and renders
/// one `name.md = N bytes` entry per link, then the composed total and the
/// headroom (or overrun) against `budget`.
/// Test: `composed_body_ledger_names_every_chain_link_7915`.
fn composed_body_ledger(stem: &str, composed_len: usize, budget: usize) -> String {
    use crate::core::agent_builder::source_chain;

    let chain = source_chain(stem, agent_assets_dir()).unwrap_or_else(|_| vec![stem.to_string()]);
    let mut parts: Vec<String> = chain
        .iter()
        .map(|link| match agent_source_bytes(link) {
            Some(bytes) => format!("{link}.md = {bytes} bytes"),
            None => format!("{link}.md = <source not found>"),
        })
        .collect();
    parts.push(format!("composed = {composed_len} bytes"));
    parts.push(if composed_len > budget {
        format!("OVER budget {budget} by {}", composed_len - budget)
    } else {
        format!("budget {budget}, {} bytes headroom", budget - composed_len)
    });
    parts.join("; ")
}

/// Byte size of one chain link's source file, resolved case-insensitively.
///
/// The composer matches `extends: base-agent` to `BASE-AGENT.md` on
/// case-sensitive filesystems, so the ledger has to resolve the same way.
fn agent_source_bytes(link: &str) -> Option<u64> {
    let want = link.to_lowercase();
    for entry in std::fs::read_dir(agent_assets_dir()).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let matches = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| stem.to_lowercase() == want);
        if matches {
            return std::fs::metadata(&path).ok().map(|m| m.len());
        }
    }
    None
}

#[test]
fn rust_engineer_composed_body_stays_within_its_resident_budget() {
    use crate::core::agent_builder::compose_agent;

    let composed = compose_agent("rust-engineer", agent_assets_dir())
        .expect("compose_agent(rust-engineer) must succeed");
    assert!(
        composed.len() <= RESIDENT_BODY_BUDGET_BYTES,
        "composed rust-engineer is over the {RESIDENT_BODY_BUDGET_BYTES}-byte \
         resident budget (#7723) — move detail into a skill rather than growing \
         the chain. Ledger: {}",
        composed_body_ledger("rust-engineer", composed.len(), RESIDENT_BODY_BUDGET_BYTES)
    );
}

/// 🔴 #7915 REGRESSION: the budget assertion reported a single total with 14
/// bytes of headroom, so any addition to BASE-AGENT.md or a bundled skill
/// turned it red with no pointer to the file that grew.
#[test]
fn composed_body_ledger_names_every_chain_link_7915() {
    use crate::core::agent_builder::{compose_agent, source_chain};

    let composed = compose_agent("rust-engineer", agent_assets_dir())
        .expect("compose_agent(rust-engineer) must succeed");
    let ledger = composed_body_ledger("rust-engineer", composed.len(), RESIDENT_BODY_BUDGET_BYTES);

    let chain = source_chain("rust-engineer", agent_assets_dir())
        .expect("source_chain(rust-engineer) must succeed");
    assert!(
        chain.len() >= 3,
        "rust-engineer must inherit through BASE-AGENT and BASE-ENGINEER, got {chain:?}"
    );
    for link in &chain {
        assert!(
            ledger.contains(&format!("{link}.md = ")),
            "ledger must name chain link {link:?} and its byte count, got {ledger:?}"
        );
        assert!(
            !ledger.contains(&format!("{link}.md = <source not found>")),
            "ledger failed to resolve the source file for {link:?}: {ledger:?}"
        );
    }
    assert!(
        ledger.contains(&format!("composed = {} bytes", composed.len())),
        "ledger must report the composed total: {ledger:?}"
    );
    assert!(
        ledger.contains("headroom") || ledger.contains("OVER budget"),
        "ledger must report headroom or the overrun: {ledger:?}"
    );
}

/// The allowance for an agent's `skills:` preloads on top of its body budget.
///
/// Why: #10051 — Claude Code loads each preloaded skill's entry file with the
/// agent, so that file is resident on every turn; a skill's `references/*.md`
/// load only when read, so they are never resident and never budgeted (owner
/// ruling: "Progressive skills should not be size limited"). 45,000 sits about
/// 3,800 bytes above the largest preload set measured on 2026-10-19
/// (rust-engineer's five entry files, 41,218 bytes).
/// What: an agent's resident total — composed body plus every preload entry
/// file — must fit its body budget plus this allowance.
/// Test: `every_deployed_agent_composed_body_stays_within_its_resident_budget`,
/// `resident_size_counts_preload_entries_and_never_references_10051`.
const RESIDENT_PRELOAD_ALLOWANCE_BYTES: usize = 45_000;

/// One agent's always-loaded text, in bytes (#10051).
struct ResidentSize {
    /// The composed agent body.
    body: usize,
    /// `(skill, entry-file bytes)` for each `skills:` preload, chain order.
    preloads: Vec<(String, usize)>,
}

impl ResidentSize {
    /// Body plus every preload entry file.
    fn total(&self) -> usize {
        self.body + self.preloads.iter().map(|(_, n)| n).sum::<usize>()
    }

    /// The body fits `body_budget` and the total fits it plus the allowance.
    fn fits(&self, body_budget: usize) -> bool {
        self.body <= body_budget && self.total() <= body_budget + RESIDENT_PRELOAD_ALLOWANCE_BYTES
    }

    /// The preload half of the ledger, appended to [`composed_body_ledger`].
    fn preload_ledger(&self, body_budget: usize) -> String {
        let entries: Vec<String> = self
            .preloads
            .iter()
            .map(|(name, n)| format!("{name} entry = {n} bytes"))
            .collect();
        format!(
            "preloads [{}]; resident = {} bytes (budget {})",
            entries.join(", "),
            self.total(),
            body_budget + RESIDENT_PRELOAD_ALLOWANCE_BYTES
        )
    }
}

/// The checkout's skills directory, `content/skills`.
fn repo_skills_dir() -> std::path::PathBuf {
    crate::core::content_source::test_support::repo_root().join("content/skills")
}

/// The entry file of skill `name`: flat `<name>.md`, else `<name>/SKILL.md`.
fn skill_entry_path(skills_dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    [
        skills_dir.join(format!("{name}.md")),
        skills_dir.join(name).join("SKILL.md"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

/// Measures agent `stem`'s always-loaded text: the composed body plus the
/// entry file of each `skills:` preload. Nothing under a skill's
/// `references/` is read (#10051).
fn resident_size(
    stem: &str,
    agents_dir: &std::path::Path,
    skills_dir: &std::path::Path,
) -> ResidentSize {
    use crate::core::agent_builder::compose_agent;
    use trusty_agents_common::agents::metadata::agent_metadata_from_str;

    let composed =
        compose_agent(stem, agents_dir).unwrap_or_else(|e| panic!("compose_agent({stem}): {e}"));
    let preloads = agent_metadata_from_str(&composed)
        .skills
        .into_iter()
        .map(|name| {
            let path = skill_entry_path(skills_dir, &name).unwrap_or_else(|| {
                panic!(
                    "{stem} preloads `{name}`, which has no entry file under {}",
                    skills_dir.display()
                )
            });
            let bytes = std::fs::metadata(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
                .len();
            (
                name,
                usize::try_from(bytes).expect("entry file size fits usize"),
            )
        })
        .collect();
    ResidentSize {
        body: composed.len(),
        preloads,
    }
}

/// 🔴 #7825 REGRESSION: the resident-body budget covered rust-engineer alone,
/// so four other deployed bodies sat over 42,000 bytes undetected. #10051: the
/// resident total adds each `skills:` preload's entry file and never a
/// `references/` file.
#[test]
fn every_deployed_agent_composed_body_stays_within_its_resident_budget() {
    let stems = deployed_agent_stems();
    let skills_dir = repo_skills_dir();
    let mut over: Vec<String> = Vec::new();
    let mut stale: Vec<String> = Vec::new();

    for stem in &stems {
        let size = resident_size(stem, agent_assets_dir(), &skills_dir);
        let baseline = OVER_BUDGET_BODY_BASELINES
            .iter()
            .find(|(name, _, _)| name == stem);
        let (budget, tracker) = match baseline {
            Some((_, ceiling, issue)) => (*ceiling, Some(*issue)),
            None => (RESIDENT_BODY_BUDGET_BYTES, None),
        };
        if !size.fits(budget) {
            let ledger = format!(
                "{}; {}",
                composed_body_ledger(stem, size.body, budget),
                size.preload_ledger(budget)
            );
            over.push(match tracker {
                Some(issue) => format!("{stem} (ratchet, {issue}): {ledger}"),
                None => format!("{stem}: {ledger}"),
            });
        }
        if tracker.is_some() && size.body <= RESIDENT_BODY_BUDGET_BYTES {
            stale.push(stem.clone());
        }
    }

    assert!(
        over.is_empty(),
        "composed agent bodies exceed their resident budget (#7825) — move \
         detail into a skill rather than growing the chain: {over:#?}"
    );
    assert!(
        stale.is_empty(),
        "these bodies are back under the {RESIDENT_BODY_BUDGET_BYTES}-byte \
         default budget — drop their OVER_BUDGET_BODY_BASELINES entries (#7825): \
         {stale:?}"
    );
    for (name, _, _) in OVER_BUDGET_BODY_BASELINES {
        assert!(
            stems.iter().any(|stem| stem == name),
            "OVER_BUDGET_BODY_BASELINES names {name:?}, which is not a deployed \
             bundled agent — the entry is stale (#7825)"
        );
    }
}

/// Why: #10051 — the budget must count a preloaded skill's entry file and
/// never its `references/`, or a progressive skill is size-limited after all.
/// What: a fixture agent preloads a copy of the real `tm-ticketing` (entry
/// plus references). A large file under `references/` leaves the resident
/// size unchanged and within budget; the same text in the entry file, or in
/// the agent body, puts it over.
/// Test: this test.
#[test]
fn resident_size_counts_preload_entries_and_never_references_10051() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let agents = tmp.path().join("agents");
    let skills = tmp.path().join("skills");
    let refs = skills.join("tm-ticketing/references");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::create_dir_all(&refs).expect("references dir");

    let agent_path = agents.join("probe.md");
    let agent_body =
        "---\nname: probe\nrole: probe\nskills: [tm-ticketing]\n---\n\n# Probe\n\nBODY\n";
    std::fs::write(&agent_path, agent_body).expect("agent");
    let entry_path = skills.join("tm-ticketing.md");
    let entry = skill_text("skills/tm-ticketing.md");
    std::fs::write(&entry_path, entry).expect("entry");
    let mut large = String::new();
    for (rel, body) in rc().skills() {
        if let Some(file) = rel.strip_prefix("skills/tm-ticketing/references/") {
            std::fs::write(refs.join(file), body).expect("reference");
            large.push_str(body);
        }
    }
    while large.len() <= RESIDENT_BODY_BUDGET_BYTES + RESIDENT_PRELOAD_ALLOWANCE_BYTES {
        large = large.repeat(2);
    }

    let base = resident_size("probe", &agents, &skills);
    assert_eq!(
        base.preloads,
        vec![("tm-ticketing".to_string(), entry.len())],
        "the preload is counted as its entry file alone"
    );
    assert!(base.fits(RESIDENT_BODY_BUDGET_BYTES), "baseline must fit");

    std::fs::write(refs.join("large.md"), &large).expect("large reference");
    let with_reference = resident_size("probe", &agents, &skills);
    assert_eq!(
        with_reference.total(),
        base.total(),
        "a references/ file must never enter the resident count"
    );
    assert!(with_reference.fits(RESIDENT_BODY_BUDGET_BYTES));

    std::fs::write(&entry_path, format!("{entry}{large}")).expect("grown entry");
    let with_entry = resident_size("probe", &agents, &skills);
    assert_eq!(with_entry.total(), base.total() + large.len());
    assert!(
        !with_entry.fits(RESIDENT_BODY_BUDGET_BYTES),
        "the same text in the entry file must go over budget: {}",
        with_entry.preload_ledger(RESIDENT_BODY_BUDGET_BYTES)
    );

    std::fs::write(&entry_path, entry).expect("restored entry");
    std::fs::write(&agent_path, format!("{agent_body}{large}")).expect("grown body");
    let with_body = resident_size("probe", &agents, &skills);
    assert!(
        !with_body.fits(RESIDENT_BODY_BUDGET_BYTES),
        "the same text in the agent body must go over budget: body {} bytes",
        with_body.body
    );
}

/// Every skill with a `references/` directory as `(entry bundle path,
/// references prefix)`, derived from the loaded content (#10055): a skill
/// joins the link and leak checks by gaining the directory, with no Rust
/// change. Asserted equal to the `references/` directories on disk.
fn progressive_skills() -> Vec<(String, String)> {
    let stems: std::collections::BTreeSet<&str> = rc()
        .skills()
        .filter_map(|(rel, _)| rel.strip_prefix("skills/")?.split_once("/references/"))
        .map(|(stem, _)| stem)
        .collect();
    let mut on_disk: Vec<String> = Vec::new();
    let skills_dir = crate::core::content_source::test_support::repo_root().join("content/skills");
    for entry in std::fs::read_dir(&skills_dir).expect("content/skills") {
        let path = entry.expect("a directory entry").path();
        if path.join("references").is_dir() {
            on_disk.push(
                path.file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    on_disk.sort();
    assert_eq!(
        stems.iter().copied().collect::<Vec<_>>(),
        on_disk,
        "the loaded skills with references/ must match content/skills on disk"
    );
    stems
        .into_iter()
        .map(|stem| {
            (
                format!("skills/{stem}.md"),
                format!("skills/{stem}/references/"),
            )
        })
        .collect()
}

/// The `(file name, body)` of every loaded skill file under `prefix`.
fn references_under(prefix: &str) -> Vec<(&'static str, &'static str)> {
    rc().skills()
        .filter_map(|(rel, body)| rel.strip_prefix(prefix).map(|file| (file, body)))
        .collect()
}

/// The heading of the entry file's task-to-file table.
const TASK_TABLE_HEADING: &str = "## Where Each Task Lives";

/// Claude Code's guidance for a SKILL.md; references carry no limit.
const SKILL_ENTRY_LINE_LIMIT: usize = 500;

/// The `|` rows of `entry`'s [`TASK_TABLE_HEADING`] section.
fn task_table_rows(entry: &str) -> Vec<&str> {
    entry
        .lines()
        .skip_while(|line| *line != TASK_TABLE_HEADING)
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .filter(|line| line.starts_with('|'))
        .collect()
}

/// The `## ` heading texts of `body`, skipping fenced code blocks.
fn h2_headings(body: &str) -> Vec<&str> {
    let mut fenced = false;
    let mut headings = Vec::new();
    for line in body.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
        } else if !fenced && let Some(text) = line.strip_prefix("## ") {
            headings.push(text.trim());
        }
    }
    headings
}

/// Every way `entry`'s task table fails to reach `references`
/// (`(file name, body)`): a file no row links, a `## ` section the linking
/// row does not name, or a row link to a file that does not exist.
fn task_table_gaps(entry: &str, references: &[(&str, &str)]) -> Vec<String> {
    let rows = task_table_rows(entry);
    let mut gaps = Vec::new();
    for (file, body) in references {
        let link = format!("](references/{file})");
        let Some(row) = rows.iter().find(|row| row.contains(&link)) else {
            gaps.push(format!(
                "references/{file} is not linked from the task table"
            ));
            continue;
        };
        for heading in h2_headings(body) {
            if !row.contains(&format!("\"{heading}\"")) {
                gaps.push(format!(
                    "references/{file}: section \"{heading}\" is not named in its row"
                ));
            }
        }
    }
    for row in &rows {
        for target in row.split("](references/").skip(1) {
            let file = target.split(')').next().unwrap_or_default();
            if !references.iter().any(|(name, _)| *name == file) {
                gaps.push(format!(
                    "task table links references/{file}, which does not exist"
                ));
            }
        }
    }
    gaps
}

/// Gaps [`reference_reach_gaps`] found when #10055 widened the check from
/// tm-ticketing to every skill with `references/`, kept in a testdata file
/// read at test time (df-D77: closing a gap is a content-PR line deletion,
/// never a Rust change or a rebuild). Shrink-only: a gap not listed fails,
/// and a listed gap that no longer occurs fails as stale. Outside `src/`,
/// which `build.rs` watches, so editing it reruns no build.
const KNOWN_REFERENCE_REACH_GAPS_FILE: &str = "crates/trusty-mpm/testdata/reference-reach-gaps.tsv";

/// The `(entry bundle path, gap)` rows of the known-gaps data file `text`:
/// one `<entry>\t<gap>` per line; `#` lines and blank lines are skipped.
fn parse_known_reach_gaps(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (entry, gap) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("`{line}`: expected <entry><TAB><gap>"));
            (entry.to_owned(), gap.to_owned())
        })
        .collect()
}

/// `found` checked against `known`: `Err` names every new gap and every
/// stale row, the stale ones with the data file to drop them from.
fn reach_gap_verdict(found: &[(String, String)], known: &[(String, String)]) -> Result<(), String> {
    let new: Vec<_> = found.iter().filter(|gap| !known.contains(gap)).collect();
    let stale: Vec<_> = known.iter().filter(|gap| !found.contains(gap)).collect();
    let mut report = String::new();
    if !new.is_empty() {
        report.push_str(&format!(
            "an entry file no longer reaches its references — link the file from \
             the entry (and its task-table row, if it has one) (#10055): {new:#?}\n"
        ));
    }
    if !stale.is_empty() {
        report.push_str(&format!(
            "a known gap closed — drop its line from {KNOWN_REFERENCE_REACH_GAPS_FILE} \
             (#10055): {stale:#?}\n"
        ));
    }
    if report.is_empty() {
        Ok(())
    } else {
        Err(report)
    }
}

/// Every way `entry` fails to reach `references` (`(file name, body)`): the
/// entry is at or over [`SKILL_ENTRY_LINE_LIMIT`]; a file the entry never
/// names as `references/<file>`; an entry link into `references/` that names
/// no file; and, when the entry has a [`TASK_TABLE_HEADING`] table, every
/// [`task_table_gaps`] gap.
fn reference_reach_gaps(entry: &str, references: &[(&str, &str)]) -> Vec<String> {
    let mut gaps = Vec::new();
    // The count stays out of the text, so editing an over-limit entry is not
    // a Rust change.
    if entry.lines().count() >= SKILL_ENTRY_LINE_LIMIT {
        gaps.push(format!(
            "the entry is not under {SKILL_ENTRY_LINE_LIMIT} lines"
        ));
    }
    for (file, _) in references {
        if !entry.contains(&format!("references/{file}")) {
            gaps.push(format!("references/{file} is not reached from the entry"));
        }
    }
    for target in entry.split("](references/").skip(1) {
        let file = target.split([')', '#']).next().unwrap_or_default();
        if !file.is_empty() && !references.iter().any(|(name, _)| *name == file) {
            gaps.push(format!(
                "the entry links references/{file}, which does not exist"
            ));
        }
    }
    if entry.lines().any(|line| line == TASK_TABLE_HEADING) {
        gaps.extend(task_table_gaps(entry, references));
    }
    gaps
}

/// Why: #10051, #10055 — a skill's entry file loads on invoke and its
/// references only when read, so a reference file the entry never names is
/// text no reader reaches, and a pointer naming a moved section resolves
/// only through the task table.
/// What: for every skill with `references/` ([`progressive_skills`]),
/// [`reference_reach_gaps`] equals [`KNOWN_REFERENCE_REACH_GAPS_FILE`]. Mutation,
/// per skill: an entry that stops naming one reached file is reported, and
/// for a task-table skill so is dropping that file's row.
/// Test: this test.
#[test]
fn every_references_skill_entry_reaches_every_reference_file_10055() {
    let mut found: Vec<(String, String)> = Vec::new();
    for (entry_path, prefix) in progressive_skills() {
        let entry = skill_text(&entry_path);
        let references = references_under(&prefix);
        assert!(!references.is_empty(), "{prefix} references must load");
        found.extend(
            reference_reach_gaps(entry, &references)
                .into_iter()
                .map(|gap| (entry_path.clone(), gap)),
        );

        let Some((file, _)) = references
            .iter()
            .find(|(file, _)| entry.contains(&format!("references/{file}")))
        else {
            continue;
        };
        let renamed = entry.replace(&format!("references/{file}"), "references/MUTATED");
        assert!(
            reference_reach_gaps(&renamed, &references)
                .contains(&format!("references/{file} is not reached from the entry")),
            "`{entry_path}`: an entry that stops naming references/{file} must be reported"
        );
        let link = format!("](references/{file})");
        if let Some(row) = task_table_rows(entry)
            .into_iter()
            .find(|row| row.contains(&link))
        {
            assert!(
                task_table_gaps(&entry.replace(row, ""), &references)
                    .iter()
                    .any(|gap| gap.contains(&format!("references/{file} is not linked"))),
                "`{entry_path}`: dropping a task-table row must leave references/{file} unlinked"
            );
        }
    }

    let data = crate::core::content_source::test_support::repo_root()
        .join(KNOWN_REFERENCE_REACH_GAPS_FILE);
    let known = parse_known_reach_gaps(
        &std::fs::read_to_string(&data).unwrap_or_else(|e| panic!("{}: {e}", data.display())),
    );
    if let Err(report) = reach_gap_verdict(&found, &known) {
        panic!("{report}");
    }

    // Mutation: a row deleted while its gap remains is a new gap; a row kept
    // for a closed gap is stale and names the data file.
    if let Some(row) = known.first() {
        let unlisted = reach_gap_verdict(&found, &known[1..]).expect_err("a deleted row");
        assert!(unlisted.contains("no longer reaches"), "{unlisted}");
        let closed: Vec<_> = found.iter().filter(|gap| *gap != row).cloned().collect();
        let stale = reach_gap_verdict(&closed, &known).expect_err("a stale row");
        assert!(stale.contains(KNOWN_REFERENCE_REACH_GAPS_FILE), "{stale}");
    }
}

/// Words in the window that pre-filters [`reference_leaks`]; a references line
/// shorter than this is too generic to name its source.
const LEAK_WINDOW_WORDS: usize = 8;

/// Every references line (`(bundle path, body)`) whose text appears in one of
/// `dispatch` texts, whitespace-insensitively. Headings, fence lines and table
/// rules are skipped, as is any line under [`LEAK_WINDOW_WORDS`] words.
/// Returns `(bundle path, whitespace-collapsed line)`.
fn reference_leaks<'a>(
    dispatch: &[String],
    references: &[(&'a str, &str)],
) -> Vec<(&'a str, String)> {
    use std::collections::HashSet;

    let mut windows: HashSet<String> = HashSet::new();
    for text in dispatch {
        let words: Vec<&str> = text.split_whitespace().collect();
        windows.extend(words.windows(LEAK_WINDOW_WORDS).map(|w| w.join(" ")));
    }
    let flattened: Vec<String> = dispatch.iter().map(|text| one_line(text)).collect();
    let mut leaks = Vec::new();
    for (path, body) in references {
        for line in body.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.starts_with("```") || trimmed.starts_with("|---")
            {
                continue;
            }
            let words: Vec<&str> = trimmed.split_whitespace().collect();
            if words.len() < LEAK_WINDOW_WORDS
                || !words
                    .windows(LEAK_WINDOW_WORDS)
                    .all(|w| windows.contains(&w.join(" ")))
            {
                continue;
            }
            let needle = words.join(" ");
            if flattened.iter().any(|text| text.contains(&needle)) {
                leaks.push((*path, needle));
            }
        }
    }
    leaks
}

/// References files whose lines a composed agent body already carried when
/// #10051 added the leak gate, each pinned at a shrink-only ceiling.
///
/// Why: the `ticketing` agent body restates tm-ticketing's label, milestone
/// and lifecycle text (and BASE-AGENT shares one attribution sentence), so the
/// moved sections matched on day one. Rewriting the agent was out of #10051's
/// scope; excluding the files would drop the gate. A per-file ceiling keeps
/// every NEW leak red and lets the overlap only shrink — the
/// `OVER_BUDGET_BODY_BASELINES` pattern.
/// What: `(bundle path, leaked-line ceiling, tracking issue)`. A file over its
/// ceiling, or any other file with a leak, fails; a file under its ceiling
/// fails as stale, so the entry is lowered or dropped as the agent is trimmed.
/// Test: `references_text_never_reaches_dispatch_text_10051`.
const KNOWN_REFERENCE_OVERLAPS: &[(&str, usize, &str)] = &[
    ("skills/tm-ticketing/references/labels.md", 2, "#7681"),
    ("skills/tm-ticketing/references/lifecycle.md", 10, "#7681"),
    (
        "skills/tm-ticketing/references/milestones-and-projects.md",
        2,
        "#7681",
    ),
];

/// `leaks` checked against [`KNOWN_REFERENCE_OVERLAPS`]: `(over, stale)`.
fn leak_violations(leaks: &[(&str, String)]) -> (Vec<String>, Vec<String>) {
    let mut by_file: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
    for (path, line) in leaks {
        by_file.entry(path).or_default().push(line);
    }
    let ceiling = |path: &str| {
        KNOWN_REFERENCE_OVERLAPS
            .iter()
            .find(|(known, _, _)| *known == path)
            .map_or(0, |(_, n, _)| *n)
    };
    let over = by_file
        .iter()
        .filter(|(path, lines)| lines.len() > ceiling(path))
        .map(|(path, lines)| format!("{path} ({} > {}): {lines:#?}", lines.len(), ceiling(path)))
        .collect();
    let stale = KNOWN_REFERENCE_OVERLAPS
        .iter()
        .filter(|(path, n, _)| by_file.get(path).map_or(0, Vec::len) < *n)
        .map(|(path, n, issue)| {
            let now = by_file.get(path).map_or(0, Vec::len);
            format!("{path} ({issue}): {now} leaked line(s), ceiling {n}")
        })
        .collect();
    (over, stale)
}

/// Why: #10051 — a skill's references load only when read. A references line
/// in the dispatch text (the composed agent body Claude Code sends as the
/// subagent's system prompt) is resident after all, and paid on every turn.
/// What: no `references/*.md` line of any bundled skill appears in any
/// deployed agent's composed body, beyond the pinned
/// [`KNOWN_REFERENCE_OVERLAPS`]. Mutation (#10055): for every skill with
/// `references/` ([`progressive_skills`]), one more of its lines appended to
/// a composed body must be reported against its file.
/// Test: this test.
#[test]
fn references_text_never_reaches_dispatch_text_10051() {
    use crate::core::agent_builder::compose_agent;

    let dispatch: Vec<String> = deployed_agent_stems()
        .iter()
        .map(|stem| {
            compose_agent(stem, agent_assets_dir())
                .unwrap_or_else(|e| panic!("compose_agent({stem}): {e}"))
        })
        .collect();
    let references: Vec<(&str, &str)> = rc()
        .skills()
        .filter(|(rel, _)| rel.contains("/references/"))
        .collect();

    let leaks = reference_leaks(&dispatch, &references);
    let (over, stale) = leak_violations(&leaks);
    assert!(
        over.is_empty(),
        "references text reached dispatch text — keep it in references/ and \
         point at it instead (#10051): {over:#?}"
    );
    assert!(
        stale.is_empty(),
        "the overlap shrank — lower or drop these KNOWN_REFERENCE_OVERLAPS \
         entries (#10051): {stale:#?}"
    );

    let mut injected: Vec<(&str, &str)> = Vec::new();
    for (_, prefix) in progressive_skills() {
        let line = references
            .iter()
            .filter(|(rel, _)| rel.starts_with(&prefix))
            .find_map(|(rel, body)| {
                body.lines()
                    .find(|line| {
                        let text = one_line(line);
                        text.split(' ').count() >= LEAK_WINDOW_WORDS
                            && !text.starts_with(['#', '`', '|'])
                            && !leaks.iter().any(|(_, leaked)| *leaked == text)
                    })
                    .map(|line| (*rel, line))
            })
            .unwrap_or_else(|| panic!("a prose line under {prefix} no agent carries"));
        injected.push(line);
    }
    let mut mutated = dispatch.clone();
    for (_, line) in &injected {
        mutated[0] = format!("{}\n{line}\n", mutated[0]);
    }
    let (caught, _) = leak_violations(&reference_leaks(&mutated, &references));
    for (path, line) in injected {
        assert!(
            caught.iter().any(|v| v.starts_with(path)),
            "an injected {path} line must be reported: {line:?} not in {caught:#?}"
        );
    }
}

/// #8992 (ruling D07): the listing-description cap every bundled agent shares.
const AGENT_DESCRIPTION_CAP_CHARS: usize = 100;

/// Every bundled agent's `description:` is listed in every session's agent
/// listing, so its length is paid on every turn (#8992). Composed, because the
/// composed frontmatter is what deploys and what Claude Code lists.
#[test]
fn every_bundled_agent_description_fits_the_listing_cap_8992() {
    use crate::core::agent_builder::compose_agent;
    use trusty_agents_common::agents::metadata::agent_metadata_from_str;

    let stems = deployed_agent_stems();
    assert!(stems.len() > 20, "the roster must be read: {stems:?}");
    let mut over: Vec<String> = Vec::new();
    for stem in &stems {
        let composed = compose_agent(stem, agent_assets_dir())
            .unwrap_or_else(|e| panic!("compose_agent({stem}): {e}"));
        let description = agent_metadata_from_str(&composed)
            .description
            .unwrap_or_else(|| panic!("{stem} has no description"));
        let chars = description.chars().count();
        if chars > AGENT_DESCRIPTION_CAP_CHARS {
            over.push(format!("{stem} ({chars} chars): {description}"));
        }
    }
    assert!(
        over.is_empty(),
        "bundled agent descriptions over the {AGENT_DESCRIPTION_CAP_CHARS}-char listing cap \
         (#8992) — shorten by meaning, keep each specific enough to route on: {over:#?}"
    );
}

#[test]
fn base_agent_prose_rules_survive_composition() {
    // Output styles do NOT apply to subagents (they run their own system
    // prompt), so the prose rules have to reach agents through BASE-AGENT.md
    // composition instead. #7423 trimmed that section to one line per rule and
    // pointed it at the same skill the output style points at, so assert the
    // section, the sparse-on-success rule and the raw-output evidence floor it
    // must not weaken.
    // #7683: the pointer is PROSE, not a modelled `Skill(...)` call — most
    // agents' `tools:` allowlist no longer carries `Skill`, so an instruction
    // to invoke it would name a tool the agent does not have. The PM's own
    // output style still models the call (it keeps the tool); that pin lives in
    // `claude_md_sections_tests.rs`.
    // #10058: each rule is a marker in BASE-AGENT.md, and the composed body
    // carries BASE-AGENT's body verbatim; the rules' effect is a #10061 eval.
    use crate::core::agent_builder::compose_agent;

    for (fact, rule) in [
        ("the Write Plainly section", "base-agent/write-plainly"),
        ("a Write Plainly bullet", "base-agent/concrete-referent"),
        (
            "a Write Plainly bullet",
            "base-agent/cut-hedges-and-aphorisms",
        ),
        ("a Write Plainly bullet", "base-agent/do-not-embellish"),
        (
            "a Write Plainly bullet",
            "base-agent/dont-justify-restraint",
        ),
        ("a Write Plainly bullet", "base-agent/no-praise"),
        ("a Write Plainly bullet", "base-agent/worth-is-implied"),
        ("a Write Plainly bullet", "base-agent/honest-ban"),
        ("a Write Plainly bullet", "base-agent/borrowed-metaphor-ban"),
        ("a Write Plainly bullet", "base-agent/asd-ste-100-spirit"),
        (
            "a Write Plainly bullet",
            "base-agent/ticket-pr-bodies-sparse",
        ),
        (
            "the graduated-verbosity rule",
            "base-agent/verbosity-scales",
        ),
        (
            "the evidence-rule carve-out",
            "base-agent/sparse-on-success-keeps-evidence",
        ),
        (
            "the raw-output evidence floor",
            "base-agent/raw-output-evidence",
        ),
    ] {
        assert!(
            has_rule(agent("BASE-AGENT.md"), rule),
            "BASE-AGENT.md is missing {fact} `{rule}` (#7423)"
        );
    }
    assert_composed_carries("version-control", "BASE-AGENT.md");

    // #7683: BASE-AGENT is the `extends` root of every roster agent, and most
    // of their `tools:` allowlists omit `Skill`. Compose one of those and
    // assert the base template models no `Skill(...)` call at all — an agent
    // must never be told to invoke a tool it does not carry.
    let assets_dir = agent_assets_dir();
    let no_skill_tool =
        compose_agent("code-critic", assets_dir).expect("compose_agent must succeed");
    assert!(
        !no_skill_tool.contains("Skill(skill="),
        "an agent whose tools: omits Skill must inherit no modelled Skill(...) call (#7683)"
    );

    // "Caveman" is taken: hooks/optimizer.toml uses it for an unrelated
    // tool-output compression level. The prose rule must not collide with it.
    let composed =
        compose_agent("version-control", assets_dir).expect("compose_agent must succeed");
    assert!(
        !composed.contains("Caveman"),
        "graduated-verbosity rule must not reuse the optimizer's `Caveman` term"
    );
}

/// 🔴 #7612 REGRESSION: `local-ops` parked on a narrated wait twice in one
/// session — once on a reachability poll, once after starting a deploy script —
/// despite BASE-AGENT's inherited rule, and a 600s bounded loop overran the
/// Bash tool's 600000ms ceiling. #2501/#2610 closed the same class on
/// version-control; the persona text must name the blocking verb for ITS task
/// shapes and the margin a bounded loop owes.
#[test]
fn local_ops_names_tm_wait_for_deploys_and_a_loop_margin_7612() {
    // #10058: pinned by marker in the source; composition is covered by
    // `base_agent_guidance_sections_survive_composition`.
    for rule in ["local-ops/deploy-and-poll-block", "local-ops/loop-margin"] {
        assert!(
            has_rule(agent("local-ops.md"), rule),
            "local-ops.md is missing the #7612 blocking-wait guidance `{rule}`"
        );
    }
}

/// 🔴 #7558 REGRESSION: `git pull --ff-only` aborts naming only the colliding
/// path, so the recovery gets improvised — once by hand-diffing two copies of
/// an untracked research doc. The bundled `git-workflow` skill must state the
/// pre-pull check and BOTH of its outcomes, so the identical-content case
/// resolves itself and the differing-content case is reported rather than
/// deleted.
#[test]
fn git_workflow_skill_states_the_pre_pull_untracked_collision_check_7558() {
    for rule in [
        "git-workflow/pre-pull-collision-check",
        "git-workflow/identical-content-resolves",
        "git-workflow/differing-content-is-reported",
    ] {
        assert!(
            // #10055: entry plus references, so a split may move the section.
            has_rule(skill_with_references("skills/git-workflow.md"), rule),
            "git-workflow skill is missing the #7558 pre-pull untracked-collision \
             guidance `{rule}`"
        );
    }
}

/// #8630: adaptive-crm spent its org's Actions cap and every job failed after.
/// The deployed `git-workflow` entry must carry the four checks, the run-spend
/// rule and the billing-blocker phrase, and `version-control` must point at
/// that section.
#[test]
fn deployed_git_workflow_skill_carries_the_actions_spend_checks_8630() {
    // #10055: entry file only, on purpose — the spend checks are resident
    // text, and version-control points at the entry's section.
    let deployed = deployed()
        .into_iter()
        .find(|a| a.rel_path == "skills/git-workflow.md")
        .expect("git-workflow must be in the ALL bundle table")
        .contents;
    for rule in [
        "git-workflow/actions-spend-four-checks",
        "git-workflow/never-rerun-a-green-run",
        "git-workflow/billing-blocker",
    ] {
        assert!(
            has_rule(deployed, rule),
            "deployed git-workflow skill is missing the #8630 Actions-spend rule `{rule}`"
        );
    }
    // #10058: the pointer is a marker in the source; composition is covered by
    // `base_agent_guidance_sections_survive_composition`.
    assert!(
        has_rule(
            agent("version-control.md"),
            "version-control/actions-spend-pointer"
        ),
        "version-control must point at the #8630 section"
    );
}

/// 🔴 #7558 REGRESSION, second home: `tm-workflow` owns the main-checkout
/// `pull --ff-only` refresh and already names ONE cause of its failure
/// (another session's uncommitted work). The untracked-path collision is a
/// second cause of the same command failing, so the refresh instructions must
/// point at the check rather than leaving the reader to improvise — a
/// cross-reference, not a second copy of the block. #10055: read across the
/// entry and its references, so a split moves the text without a Rust change.
#[test]
fn tm_workflow_points_at_the_pre_pull_untracked_collision_check_7558() {
    let workflow = skill_with_references("skills/tm-workflow.md");
    assert!(
        has_rule(workflow, "tm-workflow/pre-pull-collision-pointer"),
        "tm-workflow is missing the #7558 cross-reference to the pre-pull \
         untracked-collision check"
    );
    // A pointer, not a duplicate: the procedure itself stays in one place.
    assert!(
        !workflow.contains("git show origin/<base>:<path> | diff - <path>"),
        "tm-workflow must cross-reference the check, not restate its commands (#7558)"
    );
}

/// 🔴 #7561 REGRESSION: a `kill <pid>` the agent issued itself surfaced four
/// times as "failed with exit code 143", because a `128 + signal` exit shares
/// the numeric range of a real failure. `verification-before-completion` owns
/// the "check exit code" step, so the signal reading must live beside it —
/// including the one `128 + N` value that IS a failure (137 nobody killed).
#[test]
fn verification_skill_reads_128_plus_signal_as_terminated_not_failed_7561() {
    for rule in [
        "verification-before-completion/exit-code-signal-reading",
        "verification-before-completion/exit-137-exception",
    ] {
        assert!(
            has_rule(
                skill_with_references("skills/verification-before-completion.md"),
                rule
            ),
            "verification-before-completion skill is missing the #7561 \
             signal-exit reading `{rule}`"
        );
    }
}

/// #7739: a local gate proves only the tree it ran against. `version-control`
/// reproduced this on an external repo (adaptive-crm PR #393) — the gate
/// passed against a working tree that held an uncommitted edit, the pushed
/// ref lacked it, and CI failed a `2 !== 3` assertion the gate never saw.
/// Both the agent and the workflow skill must state the clean-tree check.
#[test]
fn version_control_gate_requires_a_clean_tree_before_push_7739() {
    assert!(
        has_rule(
            agent("version-control.md"),
            "version-control/clean-tree-before-gate"
        ),
        "version-control.md is missing the #7739 clean-tree-before-gate rule"
    );
    assert!(
        has_rule(
            skill_with_references("skills/tm-workflow.md"),
            "tm-workflow/clean-tree-before-gate"
        ),
        "tm-workflow.md is missing the #7739 clean-tree-before-gate rule"
    );
}

/// #7871: the ticketing agent reported that a repository carried no `done`
/// lifecycle label without listing the repository's labels first —
/// adaptive-crm carried `unicorn:done` the whole time. The asset must send the
/// agent to the live list before any absence claim, and say what to do with
/// each answer the list can give.
#[test]
fn ticketing_reads_the_live_label_list_before_claiming_a_label_is_absent_7871() {
    assert!(
        has_rule(
            agent("ticketing.md"),
            "ticketing/live-label-list-before-absence"
        ),
        "ticketing.md is missing the #7871 list-before-asserting-absence rule"
    );
}

/// #8013: a 5xx or timeout from a mutating `gh` call is not proof the call
/// failed — `version-control` retried `gh pr merge` after a 502 without
/// checking whether the merge had already landed, and the retry's side
/// effects (a dropped `--delete-branch`, a missed webhook) went unnoticed.
#[test]
fn version_control_verifies_before_retrying_a_5xx_8013() {
    assert!(
        has_rule(
            agent("version-control.md"),
            "version-control/verify-before-retry-5xx"
        ),
        "version-control.md is missing the #8013 verify-before-retry rule"
    );
}

/// #8014: `version-control` took a PM brief's `e.g.` example commit subject
/// literally instead of deriving the real subject from the change, so the
/// squash-merge commit subject didn't match its own PR's title.
#[test]
fn version_control_treats_brief_examples_as_shapes_8014() {
    assert!(
        has_rule(
            agent("version-control.md"),
            "version-control/brief-example-is-a-shape"
        ),
        "version-control.md is missing the #8014 brief-example-is-a-shape rule"
    );
}

/// #8015: asked to store findings AND report them, the research agent's final
/// report was only a memory-drawer pointer, so the PM had to load
/// `memory_recall` to read the actual conclusion.
#[test]
fn research_report_carries_findings_not_just_a_memory_pointer_8015() {
    assert!(
        has_rule(agent("research.md"), "research/report-carries-findings"),
        "research.md is missing the #8015 report-carries-findings rule"
    );
}

/// #8022: closing a sibling tab dropped the web-qa agent's MCP tab group,
/// and the agent had no recovery step — a `computer` call on a still-open
/// tab in that group then failed with a stale cross-extension error.
#[test]
fn web_qa_recovers_a_dropped_tab_group_8022() {
    assert!(
        has_rule(agent("web-qa.md"), "web-qa/tab-group-recovery"),
        "web-qa.md is missing the #8022 tab-group-recovery rule"
    );
}

/// #8023: vercel-ops guidance named no source of truth for confirming which
/// commit a deployment serves, and no recovery path for a dropped webhook —
/// the agent lost a round-trip re-deriving both by hand.
#[test]
fn vercel_ops_names_commit_verification_and_webhook_recovery_8023() {
    assert!(
        has_rule(
            agent("vercel-ops.md"),
            "vercel-ops/deployment-commit-source"
        ),
        "vercel-ops.md is missing the #8023 commit-verification source and \
         dropped-webhook redeploy path"
    );
}

/// #10056: the security agent's credential scan flagged the required
/// `Claude-Session:` attribution trailer as a leaked session id.
/// The shipped rule must name the checker and quote the whole-line pattern
/// that checker enforces, so the agent's allowance and the code agree.
#[test]
fn security_allows_only_the_claude_session_attribution_trailer_10056() {
    use crate::core::commit_trailers::is_attribution_trailer;
    const ID: &str = "security/claude-session-trailer";
    const PATTERN: &str = r"^Claude-Session: https://claude\.ai/code/session_[A-Za-z0-9]+$";
    let body = agent("security.md");
    assert!(
        has_rule(body, ID),
        "security.md is missing the #10056 rule `{ID}`"
    );
    assert_eq!(rule_section(body, ID), Some("Commit-message trailer"));
    // The rule's text runs from its marker to the next `## ` heading.
    let after = body
        .split_once(&format!("<!-- rule:{ID} -->"))
        .map(|(_, rest)| rest)
        .expect("the rule's marker");
    let rule = after.split("\n## ").next().unwrap_or(after);
    for needle in ["is_attribution_trailer", PATTERN, "INFO"] {
        assert!(
            rule.contains(needle),
            "the `{ID}` rule must name `{needle}`"
        );
    }
    let trailer = "Claude-Session: https://claude.ai/code/session_01AbC";
    assert!(is_attribution_trailer(&format!("subject\n\n{trailer}")));
    assert!(!is_attribution_trailer(&format!(
        "subject\n\n{trailer}?from=cli"
    )));
}

/// #8027: local-ops ignored a diagnose-only brief's restrictions and cited a
/// stale log (last written before the incident window) as root cause.
#[test]
fn local_ops_honours_diagnose_only_briefs_8027() {
    for rule in [
        "local-ops/diagnose-only-overrides",
        "local-ops/log-mtime-check",
    ] {
        assert!(
            has_rule(agent("local-ops.md"), rule),
            "local-ops.md is missing the #8027 rule `{rule}`"
        );
    }
}

/// #8017: the resume skill's CLI fallback named `tm session catchup`, a verb
/// `main.rs` answers with `warning: 'session' is deprecated; use 'sessions'`,
/// and said nothing about the CLI having neither JSON nor paged output — so a
/// session that lost the MCP server fell back to an unbounded markdown blob
/// (163 KB for 23 sessions) without knowing a paged read existed.
///
/// Why: the skill is the only thing the PM reads before shelling out; a
/// deprecated verb there is a warning on every fallback, and an unnamed paging
/// knob is a paging knob nobody uses.
/// What: asserts the singular verb is gone from the skill and that the CLI
/// fallback and its paged full-digest form carry their markers (#10058).
/// Test: this test.
#[test]
fn session_resume_skill_cites_live_verbs_and_paged_forms_8017() {
    assert!(
        !skill_text("skills/tm-session-resume.md").contains("tm session catchup"),
        "tm-session-resume.md still cites the deprecated singular `tm session catchup`; \
         the live verb is `tm sessions catchup` (#2116)"
    );
    for rule in [
        "tm-session-resume/cli-fallback",
        "tm-session-resume/paged-full-digest",
    ] {
        assert!(
            has_rule(skill_text("skills/tm-session-resume.md"), rule),
            "tm-session-resume.md is missing the #8017 CLI-fallback correction `{rule}`"
        );
    }
}

/// Why (#9883 criterion 5): the resume default became the lean answer — the
/// resolved snapshot plus live `git status` — with the digest behind
/// `full: true`. A skill still teaching the watermarked digest as the default
/// would send the PM looking for a `sessions` array that is no longer there.
/// What: the skill carries the lean-default marker (#10058), and no longer
/// calls the default a watermarked digest or `full` a watermark switch.
/// Test: this test.
#[test]
fn session_resume_skill_matches_the_lean_default_9883() {
    let text = skill_text("skills/tm-session-resume.md");
    assert!(
        has_rule(text, "tm-session-resume/lean-default"),
        "missing the lean default"
    );
    for stale in [
        "watermarked markdown digest",
        "true ignores the watermark",
        "ignore the watermark, full history",
    ] {
        assert!(!text.contains(stale), "still says {stale:?}");
    }
}

/// Why (#9883): `--all-projects` scans the full digest, so `tm sessions
/// catchup` refuses it without `--full`. Shipped content that still teaches
/// the bare form hands the PM a parse error.
/// What: every `.md` under the checkout's `content/` — no line names
/// `catchup` followed by `--all-projects` without `--full` between them.
/// Test: this test.
#[test]
fn shipped_content_never_teaches_all_projects_without_full_9883() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("readable content dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    let root = crate::core::content_source::test_support::repo_root().join("content");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(
        files.len() > 100,
        "content walk found {} files",
        files.len()
    );
    let mut offenders = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("utf-8 content");
        for (n, line) in text.lines().enumerate() {
            let Some(at) = line.find("catchup") else {
                continue;
            };
            let rest = &line[at..];
            if let Some(flag) = rest.find("--all-projects")
                && !rest[..flag].contains("--full")
            {
                offenders.push(format!("{}:{}: {line}", file.display(), n + 1));
            }
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

/// Why (#8408): every relaunch recreates the tmux window, so the skill's old
/// "select the recorded `@id`" step pointed a resumed PM at a window that no
/// longer existed.
/// What: the skill gives no `tmux select-window` step and carries the
/// tmux-session route's marker (#10058).
/// Test: this test.
#[test]
fn session_resume_skill_does_not_realign_to_a_dead_window_8408() {
    assert!(
        !skill_text("skills/tm-session-resume.md").contains("tmux select-window"),
        "tm-session-resume.md must not tell a resumed PM to select the recorded window"
    );
    assert!(
        has_rule(
            skill_text("skills/tm-session-resume.md"),
            "tm-session-resume/tmux-session-route"
        ),
        "tm-session-resume.md must name the tmux-session resolution route"
    );
}
