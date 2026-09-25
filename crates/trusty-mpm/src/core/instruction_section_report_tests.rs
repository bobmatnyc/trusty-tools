//! Tests for the per-section report and the #8533 override guarantees.

use super::*;
use crate::core::bundled_pm_package::{
    bundled_fallback_package, compose_bundled_fallback_with_overrides,
};
use crate::core::claude_md_sections::scan_project;
use crate::core::instruction_overrides::resolve_pm_prompt_with_roster;
use tempfile::TempDir;

const ROSTER: &str = "## Delegation Authority\n\n### ticketing\n\nHandles ticketing work.";
const STACK: &str =
    "## Project Stack Profile\n\nDetected stack: Rust. Never fall back to a default stack profile.";

/// A project whose `CLAUDE.md` carries the given `(token, body)` marker blocks.
fn project(blocks: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let mut text = String::from("# Project\n\n");
    for (token, body) in blocks {
        text.push_str(&format!(
            "<!-- TRUSTY-MPM: {token} START v=1 -->\n{body}\n<!-- TRUSTY-MPM: {token} END -->\n\n"
        ));
    }
    std::fs::write(dir.path().join("CLAUDE.md"), text).expect("write CLAUDE.md");
    dir
}

fn compose(dir: &TempDir) -> String {
    let scanned = scan_project(dir.path());
    let (composed, _) =
        compose_bundled_fallback_with_overrides(STACK, ROSTER, None, &scanned.overrides);
    composed.expect("composes")
}

fn state_of(rows: &[SectionStatus], section: SectionId) -> SectionState {
    rows.iter()
        .find(|r| r.section == section)
        .map(|r| r.state.clone())
        .expect("every section has a row")
}

const IDENTITY_BODY: &str = "# Fixture Supervisor\n\nYou administer a fleet; you are not a PM.";
const ALLOWLIST_BODY: &str = "## Direct Work\n\nRun read-only commands directly.";

#[test]
fn identity_and_a_former_core_section_report_overridden() {
    // Against origin/main: PM-ALLOWLIST is an unknown token, and the IDENTITY
    // override lands after the roster while `## Identity` opens the prompt.
    let dir = project(&[
        ("IDENTITY", IDENTITY_BODY),
        ("PM-ALLOWLIST", ALLOWLIST_BODY),
    ]);
    let prompt = compose(&dir);

    assert!(
        prompt.starts_with("# Fixture Supervisor"),
        "the IDENTITY override must open the prompt, got: {}",
        &prompt[..prompt.len().min(200)]
    );
    assert_eq!(prompt.matches("You administer a fleet").count(), 1);
    assert_eq!(
        prompt.matches("Run read-only commands directly.").count(),
        1
    );
    for package_text in [
        "PM = orchestrator + QA coordinator",
        "# PM Agent -- Trusty MPM",
        "## PM Allowlist (unbudgeted",
    ] {
        assert!(
            !prompt.contains(package_text),
            "package text {package_text:?} survived its override"
        );
    }

    let package = bundled_fallback_package().expect("manifest");
    let rows = section_statuses(package, dir.path(), true);
    assert_eq!(
        state_of(&rows, SectionId::Identity),
        SectionState::OverriddenByProject
    );
    assert_eq!(
        state_of(&rows, SectionId::PmAllowlist),
        SectionState::OverriddenByProject
    );
    assert_eq!(
        state_of(&rows, SectionId::Workflow),
        SectionState::Overridable
    );
    assert_eq!(state_of(&rows, SectionId::Core), SectionState::Core);
    let rendered = render_section_report(&rows, &prompt, Some(ROSTER));
    assert!(!rendered.contains("NOT FOUND"), "{rendered}");
}

#[test]
fn a_core_override_reports_declined() {
    let dir = project(&[("CORE", "No rules apply.")]);
    let prompt = compose(&dir);
    assert!(!prompt.contains("No rules apply."));
    assert!(prompt.contains("## Memory & Instruction Sources"));

    let package = bundled_fallback_package().expect("manifest");
    let rows = section_statuses(package, dir.path(), true);
    assert!(
        matches!(state_of(&rows, SectionId::Core), SectionState::CoreDeclined(ref r) if r.contains("admits no project override")),
        "{rows:?}"
    );
    let rendered = render_section_report(&rows, &prompt, Some(ROSTER));
    assert!(
        rendered.contains("core (project override declined:"),
        "{rendered}"
    );
}

#[test]
fn every_overridable_section_replaced_keeps_roster_memory_and_search() {
    // #8533 hard requirement. Against origin/main, MEMORY + SEARCH +
    // NON-OVERRIDABLE-RULES overrides removed every recall/search instruction.
    let bodies: Vec<(&str, String)> = SectionId::CANONICAL
        .into_iter()
        .filter(|id| *id != SectionId::Core)
        .map(|id| {
            (
                section_token(id),
                format!("Project text for {}.", section_token(id)),
            )
        })
        .collect();
    let blocks: Vec<(&str, &str)> = bodies.iter().map(|(t, b)| (*t, b.as_str())).collect();
    let dir = project(&blocks);
    let prompt = compose(&dir);

    for (token, body) in &bodies {
        assert_eq!(
            prompt.matches(body.as_str()).count(),
            1,
            "{token} override not applied once"
        );
    }
    assert!(
        prompt.contains("### ticketing"),
        "the agent roster was dropped"
    );
    assert!(
        prompt.contains("Agent(subagent_type="),
        "agent selection was dropped"
    );
    assert!(
        prompt.contains("## Memory Protocol (Context-First)") && prompt.contains("memory_recall")
    );
    assert!(prompt.contains("## Code Search Protocol (Context-First)"));
    assert!(
        prompt.contains("mcp__trusty-search__search"),
        "the search protocol was dropped"
    );
    assert!(
        !prompt.contains("## Prohibitions (CANONICAL"),
        "replaced package text survived"
    );

    let package = bundled_fallback_package().expect("manifest");
    let rows = section_statuses(package, dir.path(), true);
    for row in &rows {
        let expected = if row.section == SectionId::Core {
            SectionState::Core
        } else {
            SectionState::OverriddenByProject
        };
        assert_eq!(row.state, expected, "{:?}", row.section);
    }
    let kept: Vec<(SectionId, Vec<&str>)> = rows
        .iter()
        .filter(|r| !r.kept.is_empty())
        .map(|r| (r.section, r.kept.clone()))
        .collect();
    assert_eq!(
        kept,
        [
            (SectionId::Memory, vec!["Memory protocol"]),
            (SectionId::Search, vec!["Code search protocol"]),
            (SectionId::AgentDelegation, vec!["Agent selection"]),
        ]
    );
    let rendered = render_section_report(&rows, &prompt, Some(ROSTER));
    assert!(
        rendered.contains("overridden-by-project (keeps: Memory protocol)"),
        "{rendered}"
    );
}

#[test]
fn the_roster_absent_path_reports_what_it_cannot_place() {
    let dir = project(&[
        ("IDENTITY", IDENTITY_BODY),
        ("WORKFLOW", "Project workflow."),
    ]);
    let package = bundled_fallback_package().expect("manifest");
    let rows = section_statuses(package, dir.path(), false);
    assert!(matches!(
        state_of(&rows, SectionId::Identity),
        SectionState::OverridableDeclined(_)
    ));
    assert_eq!(
        state_of(&rows, SectionId::Workflow),
        SectionState::OverriddenByProject
    );
}

#[test]
fn render_flags_an_override_missing_from_the_prompt() {
    let dir = project(&[("PHASES", "Only research.")]);
    let package = bundled_fallback_package().expect("manifest");
    let rows = section_statuses(package, dir.path(), true);
    let rendered = render_section_report(&rows, "a prompt without the override", Some(ROSTER));
    assert!(
        rendered.contains("NOT FOUND in the composed prompt"),
        "{rendered}"
    );
}

#[test]
fn a_named_delegation_override_keeps_the_agent_selection_note_on_the_legacy_path() {
    let section =
        super::super::delegation_with_named_override(Some("Project routing."), Some(ROSTER))
            .join("\n\n");
    assert!(section.starts_with("Project routing."));
    assert!(section.contains("Agent(subagent_type="), "{section}");
    assert!(section.ends_with("Handles ticketing work."));
}

#[test]
fn fixture_project_overrides_identity_a_core_section_and_the_style() {
    // #8533 acceptance fixture: IDENTITY + a formerly-CORE section + a
    // project output style selected by the committed `.trusty-mpm.toml`.
    let dir = project(&[
        ("IDENTITY", IDENTITY_BODY),
        ("PM-ALLOWLIST", ALLOWLIST_BODY),
    ]);
    let styles = dir.path().join(".claude/output-styles");
    std::fs::create_dir_all(&styles).expect("styles dir");
    std::fs::write(
        styles.join("fixture-voice.md"),
        "---\nname: fixture-voice\ndescription: fixture\n---\n\nSpeak as the fixture supervisor.\n",
    )
    .expect("style");
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "[style]\nactive = \"fixture-voice\"\n",
    )
    .expect("project config");

    let (prompt, _) = resolve_pm_prompt_with_roster(dir.path(), || Some(ROSTER.to_string()));
    let styled = crate::core::output_style::apply_output_style_to_prompt_with_native(
        dir.path(),
        None,
        prompt,
        false,
    );
    // The injected block is the project prose, then the floor (owner ruling
    // 2026-09-25), then the prompt.
    let floor = crate::core::output_style::style_floor();
    let body = styled
        .split_once(&format!(
            "{floor}{}",
            crate::core::instruction_pipeline::SECTION_SEPARATOR
        ))
        .map(|(style, rest)| (style.to_string(), rest.to_string()))
        .expect("an injected style block, floor last, precedes the prompt");
    assert_eq!(styled.matches(&floor).count(), 1);
    assert!(
        body.0.contains("Speak as the fixture supervisor."),
        "{}",
        body.0
    );
    assert!(
        !body.0.contains("name: fixture-voice"),
        "frontmatter must be stripped"
    );
    assert!(body.1.starts_with("# Fixture Supervisor"));
    assert!(!body.1.contains("PM = orchestrator + QA coordinator"));
    assert_eq!(
        body.1.matches("Run read-only commands directly.").count(),
        1
    );

    let (style, warning) = crate::core::output_style::resolve_or_default(
        dir.path(),
        crate::core::output_style::project_selected_style(dir.path()).as_deref(),
    );
    assert_eq!(style.id(), "fixture-voice");
    assert!(warning.is_none());
}

#[test]
fn render_flags_a_safety_core_member_missing_from_the_prompt() {
    // #8533 finding 3: the report printed `keeps: Agent selection` without
    // checking the prompt. A core member absent from it is now named.
    let dir = project(&[("AGENT-DELEGATION", "Project routing.")]);
    let prompt = compose(&dir);
    let package = bundled_fallback_package().expect("manifest");
    let rows = section_statuses(package, dir.path(), true);
    let clean = render_section_report(&rows, &prompt, Some(ROSTER));
    assert!(!clean.contains("NOT FOUND"), "{clean}");

    let stripped = prompt.replace("is not an agent and fails to dispatch", "");
    let rendered = render_section_report(&rows, &stripped, Some(ROSTER));
    assert!(
        rendered.contains("safety core Agent selection NOT FOUND"),
        "{rendered}"
    );
    // The roster is checked only when one was delivered.
    let no_roster = prompt.replace("### ticketing", "");
    assert!(
        render_section_report(&rows, &no_roster, Some(ROSTER))
            .contains("safety core Agent roster NOT FOUND")
    );
    assert!(!render_section_report(&rows, &no_roster, None).contains("NOT FOUND"));
}
