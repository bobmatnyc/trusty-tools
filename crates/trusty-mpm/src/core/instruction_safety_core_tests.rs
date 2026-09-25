//! Tests for the safety core (#8533): its one enumeration agrees with the
//! manifest and the docs, and no project override — well-formed or not — can
//! remove a member from the composed prompt.

use super::*;
use crate::core::bundled_pm_package::bundled_fallback_package;
use crate::core::claude_md_sections::{Rejection, section_token};
use crate::core::instruction_overrides::resolve_pm_prompt_with_roster;
use crate::core::instruction_overrides::{SectionState, section_statuses};
use crate::core::instruction_package::{BlockBody, CustomizationTier};
use tempfile::TempDir;

const ROSTER: &str = "## Delegation Authority\n\n### ticketing\n\nHandles ticketing work.";

/// The two docs that name the safety core, as shipped.
const DOCS: [(&str, &str); 2] = [
    (
        "tm-workflow.md",
        include_str!("../assets/skills/tm-workflow.md"),
    ),
    (
        "sections/README.md",
        include_str!("../assets/instructions/sections/README.md"),
    ),
];

/// A project whose root `CLAUDE.md` is exactly `text`.
fn project_with_claude_md(text: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("CLAUDE.md"), text).expect("write CLAUDE.md");
    dir
}

/// A well-formed marker block.
fn marker(token: &str, body: &str) -> String {
    format!("<!-- TRUSTY-MPM: {token} START v=1 -->\n{body}\n<!-- TRUSTY-MPM: {token} END -->\n\n")
}

/// The prompt a launch in `dir` composes, with a fixed roster.
fn prompt_for(dir: &TempDir) -> String {
    resolve_pm_prompt_with_roster(dir.path(), || Some(ROSTER.to_string())).0
}

/// Every safety-core marker is in `prompt`, or the missing member names.
fn missing_core_members(prompt: &str) -> Vec<&'static str> {
    SAFETY_CORE
        .iter()
        .filter(|m| !prompt.contains(m.marker))
        .map(|m| m.name)
        .collect()
}

#[test]
fn the_manifest_pins_exactly_the_safety_core() {
    let package = bundled_fallback_package().expect("manifest");

    let fixed: Vec<SectionId> = package
        .sections
        .iter()
        .filter(|s| s.customization_tier == CustomizationTier::Fixed)
        .map(|s| s.id)
        .collect();
    let mut expected_fixed: Vec<SectionId> = SAFETY_CORE
        .iter()
        .filter(|m| m.kind == SafetyCoreKind::FixedSection)
        .map(|m| m.section)
        .collect();
    expected_fixed.dedup();
    assert_eq!(
        fixed, expected_fixed,
        "tier `fixed` must be the core's sections"
    );

    // Every pinned block carries exactly one pinned member, and every pinned
    // member is carried by a pinned block of its own section.
    let pinned: Vec<(SectionId, &str)> = package
        .blocks
        .iter()
        .filter(|b| b.pinned)
        .map(|b| match b.body.authored() {
            Some(Ok(text)) => (b.section, text),
            other => panic!("a pinned block must be authored, got {other:?}"),
        })
        .collect();
    let members: Vec<&SafetyCoreMember> = SAFETY_CORE
        .iter()
        .filter(|m| m.kind == SafetyCoreKind::PinnedBlock)
        .collect();
    assert_eq!(
        pinned.len(),
        members.len(),
        "one pinned block per pinned member"
    );
    for member in members {
        assert!(
            pinned
                .iter()
                .any(|(section, text)| *section == member.section && text.contains(member.marker)),
            "{} is not a pinned block of {:?}",
            member.name,
            member.section
        );
    }

    for member in SAFETY_CORE.iter() {
        match member.kind {
            SafetyCoreKind::GeneratedBlock(generator) => {
                assert!(
                    package.blocks.iter().any(|b| b.section == member.section
                        && b.body == BlockBody::Generated { generator }),
                    "{} has no {generator:?} block in {:?}",
                    member.name,
                    member.section
                )
            }
            SafetyCoreKind::FixedSection => assert!(
                package
                    .authored_run(&[member.section])
                    .contains(member.marker),
                "{} is not in the {:?} section",
                member.name,
                member.section
            ),
            SafetyCoreKind::PinnedBlock => {}
        }
    }
}

/// The `| name | token | kind |` rows between the safety-core doc markers.
fn doc_rows(doc: &str) -> Vec<(String, String, String)> {
    let start = doc
        .find("<!-- safety-core:start -->")
        .expect("doc carries the safety-core table start marker");
    let end = doc
        .find("<!-- safety-core:end -->")
        .expect("doc carries the safety-core table end marker");
    doc[start..end]
        .lines()
        .filter(|l| l.starts_with('|') && !l.starts_with("|---") && !l.contains("| Member |"))
        .map(|l| {
            let cells: Vec<String> = l
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().trim_matches('`').to_string())
                .collect();
            (cells[0].clone(), cells[1].clone(), cells[2].clone())
        })
        .collect()
}

#[test]
fn the_docs_name_every_safety_core_member() {
    let expected: Vec<(String, String, String)> = SAFETY_CORE
        .iter()
        .map(|m| {
            (
                m.name.to_string(),
                section_token(m.section).to_string(),
                m.kind.label().to_string(),
            )
        })
        .collect();
    // The spec of record lives outside the crate, so `include_str!` would break
    // `cargo package`; read it from the workspace at test time instead.
    let spec_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/specs/SPEC-PMINSTR-01-p1-p2-instruction-restructure.md");
    let spec = std::fs::read_to_string(&spec_path).expect("the workspace spec of record");
    let docs = DOCS
        .into_iter()
        .chain([("SPEC-PMINSTR-01 §11.5", spec.as_str())]);
    for (name, doc) in docs {
        assert_eq!(
            doc_rows(doc),
            expected,
            "{name}: the safety-core table diverges from `SAFETY_CORE`"
        );
    }
}

#[test]
fn a_safety_core_override_is_declined_for_each_core_section() {
    let package = bundled_fallback_package().expect("manifest");
    for member in SAFETY_CORE
        .iter()
        .filter(|m| m.kind == SafetyCoreKind::FixedSection)
    {
        let token = section_token(member.section);
        let body = format!("PROJECT-REPLACES-{token}");
        let dir = project_with_claude_md(&marker(token, &body));

        let prompt = prompt_for(&dir);
        assert!(
            !prompt.contains(&body),
            "{}: the override reached the prompt",
            member.name
        );
        assert!(
            prompt.contains(member.marker),
            "{}: the core text was dropped",
            member.name
        );

        let scanned = crate::core::claude_md_sections::scan_project(dir.path());
        let (_, rejected) = package.with_overrides(&scanned.overrides);
        assert!(
            rejected.iter().any(|r| matches!(
                r,
                Rejection::NotOverridable { section, .. } if *section == member.section
            )),
            "{}: the decline was not reported: {rejected:?}",
            member.name
        );
        let rows = section_statuses(package, dir.path(), true);
        let row = rows
            .iter()
            .find(|r| r.section == member.section)
            .expect("row");
        assert!(
            matches!(row.state, SectionState::CoreDeclined(_)),
            "{}: {:?}",
            member.name,
            row.state
        );
    }
}

#[test]
fn overriding_every_overridable_section_keeps_the_safety_core() {
    // The "override everything" test. Fails with `apply_one`'s pinned-block
    // exemption removed: the memory, search and agent-selection markers go.
    let mut text = String::from("# Project\n\n");
    let bodies: Vec<String> = SectionId::CANONICAL
        .into_iter()
        .filter(|id| !is_fixed_core_section(*id))
        .map(|id| {
            let body = format!("Project text for {}.", section_token(id));
            text.push_str(&marker(section_token(id), &body));
            body
        })
        .collect();
    let dir = project_with_claude_md(&text);
    let prompt = prompt_for(&dir);

    for body in &bodies {
        assert_eq!(
            prompt.matches(body.as_str()).count(),
            1,
            "{body} not applied once"
        );
    }
    assert_eq!(missing_core_members(&prompt), Vec::<&str>::new());
    assert!(prompt.contains("memory_recall") && prompt.contains("mcp__trusty-search__search"));
    assert!(prompt.contains("Agent(subagent_type="));
}

#[test]
fn a_malformed_marker_block_keeps_the_base_prompt_and_the_safety_core() {
    // Fail-open: an unclosed block, an unsupported version and a nested START
    // each cost only themselves; the prompt is the clean project's, byte for byte.
    let clean = prompt_for(&project_with_claude_md("# Project\n"));
    let malformed = project_with_claude_md(
        "# Project\n\n\
         <!-- TRUSTY-MPM: IDENTITY START v=1 -->\nUnclosed identity.\n\n\
         <!-- TRUSTY-MPM: MEMORY START v=9 -->\nFuture memory.\n<!-- TRUSTY-MPM: MEMORY END -->\n\n\
         <!-- TRUSTY-MPM: SEARCH START v=1 -->\n<!-- TRUSTY-MPM: CORE START v=1 -->\n\
         Nested core.\n<!-- TRUSTY-MPM: CORE END -->\n",
    );
    let prompt = prompt_for(&malformed);
    assert_eq!(missing_core_members(&prompt), Vec::<&str>::new());
    assert!(!prompt.contains("Nested core.") && !prompt.contains("Future memory."));
    assert_eq!(
        prompt, clean,
        "a malformed block must leave the base prompt whole"
    );
}

#[test]
fn an_unreadable_claude_md_keeps_the_base_prompt_and_the_safety_core() {
    // Fail-open: a `CLAUDE.md` that exists but cannot be read (here a
    // directory, which fails `read_to_string` with a non-NotFound error even as
    // root) means "no overrides", never an empty or partial prompt.
    let clean = prompt_for(&project_with_claude_md("# Project\n"));
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir(dir.path().join("CLAUDE.md")).expect("CLAUDE.md as a directory");
    let prompt = prompt_for(&dir);
    assert_eq!(missing_core_members(&prompt), Vec::<&str>::new());
    assert_eq!(prompt, clean);
}
