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

/// The safety-core members `prompt` lacks, against the fixed test roster.
fn missing(prompt: &str) -> Vec<&'static str> {
    missing_core_members(prompt, Some(ROSTER))
}

/// The prompt a roster-absent launch in `dir` composes (no agent deployed).
fn roster_absent_prompt_for(dir: &TempDir) -> String {
    resolve_pm_prompt_with_roster(dir.path(), || None).0
}

/// A project override body's tail that leaves the comment fold open.
const UNCLOSED_TAIL: &str = "\n<!-- TODO";

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
    assert_eq!(missing(&prompt), Vec::<&str>::new());
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
    assert_eq!(missing(&prompt), Vec::<&str>::new());
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
    assert_eq!(missing(&prompt), Vec::<&str>::new());
    assert_eq!(prompt, clean);
}

#[test]
fn an_override_ending_in_an_unclosed_comment_hides_nothing_outside_its_section() {
    // #8533 finding 1: folded as one string, `<!-- TODO` at the end of any
    // override body swallowed every later block — agent selection, the roster
    // and the enforcement tables for AGENT-DELEGATION. Folded per block, the
    // unclosed comment costs only its own tail: the prompt equals the one the
    // same body composes without it.
    for id in SectionId::CANONICAL
        .into_iter()
        .filter(|id| !is_fixed_core_section(*id))
    {
        let token = section_token(id);
        let body = format!("Project text for {token}.");
        let well_formed = prompt_for(&project_with_claude_md(&marker(token, &body)));
        let unclosed = prompt_for(&project_with_claude_md(&marker(
            token,
            &format!("{body}{UNCLOSED_TAIL}"),
        )));
        assert_eq!(missing(&unclosed), Vec::<&str>::new(), "{token}");
        assert!(
            unclosed == well_formed,
            "{token}: an unclosed comment in the override hid base text after it \
             ({} bytes delivered, {} expected)",
            unclosed.len(),
            well_formed.len()
        );
    }
}

#[test]
fn an_override_ending_in_an_unclosed_comment_hides_nothing_on_the_roster_absent_path() {
    // The roster-absent string assembly places only these three overrides.
    for id in [
        SectionId::Memory,
        SectionId::Workflow,
        SectionId::AgentDelegation,
    ] {
        let token = section_token(id);
        let body = format!("Project text for {token}.");
        let well_formed = roster_absent_prompt_for(&project_with_claude_md(&marker(token, &body)));
        let unclosed = roster_absent_prompt_for(&project_with_claude_md(&marker(
            token,
            &format!("{body}{UNCLOSED_TAIL}"),
        )));
        assert_eq!(
            missing_core_members(&unclosed, None),
            Vec::<&str>::new(),
            "{token}"
        );
        assert!(
            unclosed == well_formed,
            "{token}: an unclosed comment in the override hid base text after it"
        );
    }
}

#[test]
fn an_override_ending_in_an_unclosed_fence_closes_it_in_its_own_block() {
    // #8533 critic LOW: a body that opens a fence and never closes it turned
    // the text after it into code. Each block now closes its own fence, so the
    // prompt equals the one the same body composes with the fence closed.
    let packaged: Vec<SectionId> = SectionId::CANONICAL
        .into_iter()
        .filter(|id| !is_fixed_core_section(*id))
        .collect();
    let roster_absent = [
        SectionId::Memory,
        SectionId::Workflow,
        SectionId::AgentDelegation,
    ];
    let paths: [(&[SectionId], fn(&TempDir) -> String); 2] = [
        (&packaged, prompt_for),
        (&roster_absent, roster_absent_prompt_for),
    ];
    for (ids, compose) in paths {
        for id in ids {
            let token = section_token(*id);
            for (open, close) in [
                ("```text\nunclosed", "```"),
                ("~~~\nunclosed", "~~~"),
                ("````md\n```\ninner", "````"),
            ] {
                let body = format!("Project text for {token}.\n{open}");
                let closed = compose(&project_with_claude_md(&marker(
                    token,
                    &format!("{body}\n{close}"),
                )));
                let unclosed = compose(&project_with_claude_md(&marker(token, &body)));
                assert_eq!(
                    crate::core::instruction_fold::open_fence_at_end(&unclosed),
                    None,
                    "{token} {open:?}"
                );
                assert!(
                    unclosed == closed,
                    "{token} {open:?}: the override's open fence reached the text after it"
                );
            }
        }
    }
}

#[test]
fn every_bundled_block_closes_its_own_comments_and_fences() {
    // Folding per block is byte-neutral for the shipped corpus only while no
    // block leaves a comment or fence open for the next one to depend on — the
    // `core.md`-opens-with-a-comment case. A block that did would now lose the
    // text after its opener instead of hiding a neighbour's.
    let package = bundled_fallback_package().expect("manifest");
    for (index, block) in package.blocks.iter().enumerate() {
        let Some(Ok(body)) = block.body.authored() else {
            continue;
        };
        let folded = crate::core::instruction_fold::fold_delivered_prompt(&format!(
            "{}\nSENTINEL-8533",
            body.trim()
        ));
        assert!(
            folded.ends_with("SENTINEL-8533"),
            "block {index} ({:?}) leaves a comment open",
            block.section
        );
        let fences = body
            .lines()
            .filter(|l| l.trim_start().starts_with("```"))
            .count();
        assert_eq!(
            fences % 2,
            0,
            "block {index} ({:?}) leaves a fence open",
            block.section
        );
    }
}

#[test]
fn the_roster_absent_path_keeps_every_core_member_but_the_roster() {
    // #8533 finding 2: with no agent deployed the prompt stated no
    // agent-selection rule, with or without an AGENT-DELEGATION override.
    for text in [
        "# Project\n".to_string(),
        marker("AGENT-DELEGATION", "Project routing."),
    ] {
        let prompt = roster_absent_prompt_for(&project_with_claude_md(&text));
        assert_eq!(
            missing_core_members(&prompt, None),
            Vec::<&str>::new(),
            "{text}"
        );
        assert!(!prompt.contains("## Delegation Authority"));
    }
    let pinned = crate::core::bundled_pm_package::pinned_run(SectionId::AgentDelegation);
    let selection = SAFETY_CORE
        .iter()
        .find(|m| m.name == "Agent selection")
        .expect("member");
    for text in [
        pinned.as_str(),
        crate::core::instruction_overrides::AGENT_SELECTION_WITHOUT_ROSTER,
    ] {
        assert!(text.contains(selection.marker), "{text}");
    }
}

#[test]
fn a_spoofed_heading_does_not_count_as_a_present_member() {
    // #8533 finding 3: the presence check reads body sentences, so an override
    // that repeats every core heading while the core text is gone is caught.
    let spoof = "## Memory & Instruction Sources\n\n## Customization Surface\n\n\
                 ## Detected Project Stack (auto-derived)\n\n\
                 ## Memory Protocol (Context-First)\n\n## Code Search Protocol (Context-First)\n\n\
                 > **Agent selection.**\n\n## Delegation Authority\n";
    let every: Vec<&str> = SAFETY_CORE.iter().map(|m| m.name).collect();
    assert_eq!(missing(spoof), every);
    assert_eq!(missing_core_members(spoof, None), every[..every.len() - 1]);
}
