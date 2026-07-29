//! Tests for the `CLAUDE.md` named-section override reader (#4183 / #4286).
//!
//! Three groups, deliberately separated:
//!
//! * GRAMMAR — what `parse_marker` / `parse_blocks` recognise, and what they
//!   refuse to recognise. The refusals matter more than the acceptances: prose
//!   must never be swallowed into an override block.
//! * APPLICATION — `InstructionPackage::with_overrides`, the single decision
//!   point for whether a parsed override may land. The floor tests here are the
//!   structural guarantee that `CLAUDE.md` cannot reach the framework floor.
//! * WIRING — end-to-end through `resolve_pm_prompt_with_roster`. These are the
//!   tests that fail if the reader is built but never called, which is the exact
//!   failure #381 was.

use super::*;
use crate::core::bundled_pm_package::bundled_fallback_package;
use crate::core::instruction_overrides::{
    FILE_AGENT_DELEGATION, FILE_INSTRUCTIONS, OVERRIDE_DIR_NAME, PromptSource,
    resolve_pm_prompt_with_roster,
};
use crate::core::instruction_package::{Generator, InstructionBlock, Join};
use tempfile::TempDir;

/// A fixed roster, so composed output never depends on the machine's agents.
const ROSTER: &str = "## Delegation Authority\n\n### ticketing\n\nHandles ticketing work.";

/// Write `<project>/CLAUDE.md`.
fn write_claude_md(project: &Path, body: &str) {
    std::fs::write(project.join("CLAUDE.md"), body).expect("write CLAUDE.md");
}

/// Write `<project>/.trusty-mpm/<name>`, creating the directory.
fn write_trusty_file(project: &Path, name: &str, body: &str) {
    let dir = project.join(OVERRIDE_DIR_NAME);
    std::fs::create_dir_all(&dir).expect("create .trusty-mpm");
    std::fs::write(dir.join(name), body).expect("write override");
}

/// Render a marker-delimited block for `section`.
fn block(section: SectionId, body: &str) -> String {
    let token = section_token(section);
    format!("<!-- TRUSTY-MPM: {token} START v=1 -->\n{body}\n<!-- TRUSTY-MPM: {token} END -->\n")
}

/// Resolve the PM prompt for `project` against the fixed roster.
fn resolve(project: &Path) -> (String, PromptSource) {
    resolve_pm_prompt_with_roster(project, || Some(ROSTER.to_string()))
}

/// The reasons reported by a scan, in order.
fn reasons(scanned: &ProjectOverrides) -> Vec<&'static str> {
    scanned.diagnostics.iter().map(|d| d.reason).collect()
}

// ---------------------------------------------------------------------------
// GRAMMAR
// ---------------------------------------------------------------------------

#[test]
fn every_section_token_is_the_kebab_case_id_uppercased() {
    // The token is the contract a project types by hand. Pinning it to the
    // serde name means renaming a section cannot silently orphan every
    // project's markers — it breaks here first.
    for id in SectionId::CANONICAL {
        let kebab = serde_json::to_string(&id).expect("serialize id");
        let expected = kebab.trim_matches('"').to_ascii_uppercase();
        assert_eq!(section_token(id), expected, "token for {id:?}");
    }
}

#[test]
fn parses_the_documented_marker_grammar() {
    // The documented form, plus the tolerated variations: free interior
    // whitespace, whitespace before `-->`, leading indentation, and
    // case-insensitive keywords and tokens.
    for line in [
        "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->",
        "<!--   TRUSTY-MPM:   WORKFLOW   START   v=1   -->",
        "   <!-- trusty-mpm: workflow start v=1 -->   ",
        "<!-- TRUSTY-MPM: Agent-Delegation START v=1 -->",
    ] {
        let marker = parse_marker(line).unwrap_or_else(|| panic!("must parse: {line:?}"));
        assert!(matches!(
            marker.kind,
            MarkerKind::Start(MarkerVersion::Supported)
        ));
    }

    let end = parse_marker("<!-- TRUSTY-MPM: WORKFLOW END -->").expect("END parses");
    assert_eq!(end.kind, MarkerKind::End);
    assert!(end.token.eq_ignore_ascii_case("WORKFLOW"));
}

#[test]
fn prose_mentioning_the_marker_is_not_a_marker() {
    // Recognition must be conservative: anything that is not a whole-line
    // `TRUSTY-MPM:` comment is ordinary text. Otherwise documentation ABOUT the
    // mechanism would start eating the prose around it.
    for line in [
        "Use `<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` to open a block.",
        "<!-- TRUSTY-MPM: WORKFLOW START v=1 --> trailing prose",
        "<!-- an ordinary HTML comment -->",
        "<!-- TRUSTY-MPM: WORKFLOW -->",
        "<!-- TRUSTY-MPM: WORKFLOW START v=1 extra -->",
        "<!-- TRUSTY-MPM: WORKFLOW SIDEWAYS v=1 -->",
        "# TRUSTY-MPM: WORKFLOW START v=1",
        "",
    ] {
        assert!(
            parse_marker(line).is_none(),
            "must NOT be a marker: {line:?}"
        );
    }
}

#[test]
fn scans_claude_md_for_named_sections() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &format!(
            "# Project Instructions\n\nsome prose\n\n{}\n{}",
            block(SectionId::Workflow, "TWO_PHASE_ONLY"),
            block(SectionId::Memory, "Recall from the `team` palace first."),
        ),
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(scanned.diagnostics, vec![]);
    // Sorted into canonical order: Memory (2) precedes Workflow (4), whatever
    // order they were authored in.
    let got: Vec<(SectionId, &str)> = scanned
        .overrides
        .iter()
        .map(|o| (o.section, o.body.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (SectionId::Memory, "Recall from the `team` palace first."),
            (SectionId::Workflow, "TWO_PHASE_ONLY"),
        ]
    );
    assert_eq!(scanned.overrides[0].host, tmp.path().join("CLAUDE.md"));
}

#[test]
fn text_outside_markers_is_ignored() {
    // Only what lies strictly between the marker lines is instruction content.
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &format!(
            "BEFORE_TEXT\n\n{}\n\nAFTER_TEXT\n",
            block(SectionId::Workflow, "INSIDE_TEXT")
        ),
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(scanned.overrides.len(), 1);
    assert_eq!(scanned.overrides[0].body, "INSIDE_TEXT");
}

#[test]
fn claude_md_wins_a_same_section_collision_with_instructions_md() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(tmp.path(), &block(SectionId::Workflow, "FROM_CLAUDE_MD"));
    write_trusty_file(
        tmp.path(),
        FILE_INSTRUCTIONS,
        &block(SectionId::Workflow, "FROM_INSTRUCTIONS_MD"),
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(scanned.overrides.len(), 1);
    assert_eq!(scanned.overrides[0].body, "FROM_CLAUDE_MD");
    assert_eq!(reasons(&scanned), vec![REASON_SHADOWED]);
}

#[test]
fn instructions_md_supplies_sections_claude_md_does_not_claim() {
    // Both hosts are scanned; precedence only settles collisions.
    let tmp = TempDir::new().unwrap();
    write_claude_md(tmp.path(), &block(SectionId::Workflow, "FROM_CLAUDE_MD"));
    write_trusty_file(
        tmp.path(),
        FILE_INSTRUCTIONS,
        &block(SectionId::Memory, "FROM_INSTRUCTIONS_MD"),
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(scanned.diagnostics, vec![]);
    assert_eq!(scanned.overrides.len(), 2);
    assert_eq!(scanned.overrides[0].section, SectionId::Memory);
    assert_eq!(scanned.overrides[1].section, SectionId::Workflow);
}

#[test]
fn duplicate_section_in_one_host_keeps_the_first() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &format!(
            "{}\n{}",
            block(SectionId::Workflow, "FIRST"),
            block(SectionId::Workflow, "SECOND")
        ),
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(scanned.overrides.len(), 1);
    assert_eq!(scanned.overrides[0].body, "FIRST");
    assert_eq!(reasons(&scanned), vec![REASON_DUPLICATE]);
}

#[test]
fn unknown_section_token_is_skipped_with_a_diagnostic() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &format!(
            "<!-- TRUSTY-MPM: TELEPATHY START v=1 -->\nX\n<!-- TRUSTY-MPM: TELEPATHY END -->\n{}",
            block(SectionId::Workflow, "STILL_APPLIES")
        ),
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(reasons(&scanned), vec![REASON_UNKNOWN_SECTION]);
    assert_eq!(scanned.overrides.len(), 1, "other blocks still apply");
    assert_eq!(scanned.overrides[0].body, "STILL_APPLIES");
}

#[test]
fn unsupported_version_is_skipped() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        "<!-- TRUSTY-MPM: WORKFLOW START v=99 -->\nFROM_THE_FUTURE\n\
         <!-- TRUSTY-MPM: WORKFLOW END -->\n",
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(reasons(&scanned), vec![REASON_UNSUPPORTED_VERSION]);
    assert!(scanned.overrides.is_empty());
}

#[test]
fn missing_version_is_accepted_as_v1_with_a_diagnostic() {
    // `v=` is required by the design, but a missing one must not blank a
    // section for this release — it degrades to v=1 and says so.
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        "<!-- TRUSTY-MPM: WORKFLOW START -->\nNO_VERSION\n<!-- TRUSTY-MPM: WORKFLOW END -->\n",
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(reasons(&scanned), vec![REASON_MISSING_VERSION]);
    assert_eq!(scanned.overrides.len(), 1);
    assert_eq!(scanned.overrides[0].body, "NO_VERSION");
}

#[test]
fn empty_body_never_blanks_a_section() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(tmp.path(), &block(SectionId::Workflow, "   \n\t\n   "));

    let scanned = scan_project(tmp.path());
    assert_eq!(reasons(&scanned), vec![REASON_EMPTY_BODY]);
    assert!(scanned.overrides.is_empty(), "absent, never blank");
}

#[test]
fn unterminated_block_is_skipped_and_later_blocks_still_apply() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &format!(
            "{}<!-- TRUSTY-MPM: MEMORY START v=1 -->\nnever closed\n",
            block(SectionId::Workflow, "APPLIES")
        ),
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(reasons(&scanned), vec![REASON_UNCLOSED]);
    assert_eq!(scanned.overrides.len(), 1);
    assert_eq!(scanned.overrides[0].section, SectionId::Workflow);
}

#[test]
fn nested_start_discards_the_outer_block() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\nouter\n\
         <!-- TRUSTY-MPM: MEMORY START v=1 -->\ninner\n\
         <!-- TRUSTY-MPM: MEMORY END -->\n",
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(reasons(&scanned), vec![REASON_UNCLOSED]);
    assert_eq!(scanned.overrides.len(), 1);
    assert_eq!(scanned.overrides[0].section, SectionId::Memory);
    assert_eq!(scanned.overrides[0].body, "inner");
}

#[test]
fn unmatched_end_is_reported_and_ignored() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &format!(
            "<!-- TRUSTY-MPM: MEMORY END -->\n{}",
            block(SectionId::Workflow, "APPLIES")
        ),
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(reasons(&scanned), vec![REASON_UNMATCHED_END]);
    assert_eq!(scanned.overrides.len(), 1);
}

#[test]
fn mismatched_end_discards_the_block() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\nbody\n<!-- TRUSTY-MPM: MEMORY END -->\n",
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(reasons(&scanned), vec![REASON_MISMATCHED_END]);
    assert!(scanned.overrides.is_empty());
}

#[test]
fn missing_claude_md_yields_no_overrides() {
    let tmp = TempDir::new().unwrap();
    let scanned = scan_project(tmp.path());
    assert_eq!(scanned, ProjectOverrides::default());
}

#[test]
fn unreadable_host_is_not_fatal() {
    // A directory where the file should be: `read_to_string` fails with
    // something other than NotFound and the launch still resolves.
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("CLAUDE.md")).unwrap();
    let scanned = scan_project(tmp.path());
    assert!(scanned.overrides.is_empty());
}

#[test]
fn diagnostics_carry_the_host_and_a_one_based_line() {
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        "line one\n<!-- TRUSTY-MPM: TELEPATHY START v=1 -->\nX\n\
         <!-- TRUSTY-MPM: TELEPATHY END -->\n",
    );

    let scanned = scan_project(tmp.path());
    assert_eq!(
        scanned.diagnostics,
        vec![ScanDiagnostic {
            host: tmp.path().join("CLAUDE.md"),
            line: 2,
            reason: REASON_UNKNOWN_SECTION,
        }]
    );
}

#[test]
fn strip_is_a_no_op_without_markers() {
    // The universal case today. Byte-for-byte identity here is what proves the
    // existing `project-addendum` generator cannot regress.
    for text in [
        "",
        "# Project Rules\n\nALWAYS_RUN_MAKE_CHECK\n",
        "trailing whitespace kept   \n\n\n",
        "<!-- an ordinary comment -->\n",
    ] {
        assert_eq!(strip_marker_blocks(text), text);
    }
}

#[test]
fn strip_removes_marked_blocks_from_the_addendum() {
    let text = format!(
        "KEEP_BEFORE\n\n{}\nKEEP_AFTER\n",
        block(SectionId::Workflow, "MOVED_TO_A_SECTION")
    );
    let stripped = strip_marker_blocks(&text);
    assert!(stripped.contains("KEEP_BEFORE"));
    assert!(stripped.contains("KEEP_AFTER"));
    assert!(!stripped.contains("MOVED_TO_A_SECTION"));
    assert!(!stripped.contains("TRUSTY-MPM:"), "markers go too");
}

// ---------------------------------------------------------------------------
// APPLICATION — `InstructionPackage::with_overrides`
// ---------------------------------------------------------------------------

/// An override of `section` with `body`, as the scanner would produce.
fn over(section: SectionId, body: &str) -> SectionOverride {
    SectionOverride {
        section,
        body: body.to_string(),
        host: PathBuf::from("CLAUDE.md"),
        line: 1,
    }
}

#[test]
fn with_overrides_of_nothing_is_the_identity() {
    // The property the two existing goldens rest on: a project with no
    // `CLAUDE.md` composes exactly the package it always did.
    let package = bundled_fallback_package();
    let (result, rejected) = package.with_overrides(&[]);
    assert_eq!(result, package);
    assert_eq!(rejected, vec![]);
}

#[test]
fn floor_sections_refuse_every_named_section_override() {
    // The structural guarantee: the package's `customization_tier` is the only
    // authority, and it says `fixed` for all three floor sections. No list in
    // the reader is consulted, so none can drift out of sync with this.
    let package = bundled_fallback_package();
    for section in [
        SectionId::Identity,
        SectionId::NonOverridableRules,
        SectionId::FrameworkGuaranteedConventions,
    ] {
        let (result, rejected) = package.with_overrides(&[over(section, "SUBVERTED")]);
        assert_eq!(result, package, "{section:?} must be untouched");
        assert_eq!(
            rejected,
            vec![Rejection::NotOverridable {
                section,
                tier: CustomizationTier::Fixed,
            }]
        );
    }
}

#[test]
fn content_sections_accept_a_project_override() {
    let package = bundled_fallback_package();
    for section in [
        SectionId::Core,
        SectionId::Memory,
        SectionId::Search,
        SectionId::Workflow,
        SectionId::AgentDelegation,
    ] {
        let (result, rejected) = package.with_overrides(&[over(section, "REPLACED_BODY")]);
        assert_eq!(rejected, vec![], "{section:?} is tier project");
        assert_ne!(result, package, "{section:?} must actually change");
        let texts: Vec<&str> = result
            .blocks
            .iter()
            .filter(|b| b.section == section)
            .filter_map(|b| match &b.body {
                BlockBody::Text { text } => Some(text.as_str()),
                BlockBody::Generated { .. } => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["REPLACED_BODY"],
            "{section:?} keeps exactly one text block"
        );
        result.validate().expect("overridden package validates");
    }
}

#[test]
fn agent_delegation_override_keeps_the_generated_roster() {
    // The #4196 shape in override form: a project must be able to rewrite the
    // routing doctrine WITHOUT being able to suppress the live agent roster.
    let package = bundled_fallback_package();
    let (result, rejected) = package.with_overrides(&[over(
        SectionId::AgentDelegation,
        "# Custom Routing\n\nROUTE_ALL_TO_ENGINEER",
    )]);
    assert_eq!(rejected, vec![]);
    assert!(
        result.blocks.iter().any(|b| matches!(
            b.body,
            BlockBody::Generated {
                generator: Generator::AgentRoster
            }
        )),
        "the roster block must survive the override"
    );
    result.validate().expect("roster is still consumed");
}

#[test]
fn one_bad_override_does_not_discard_a_good_one() {
    let package = bundled_fallback_package();
    let (result, rejected) = package.with_overrides(&[
        over(SectionId::Identity, "SUBVERTED"),
        over(SectionId::Workflow, "CUSTOM_WORKFLOW"),
    ]);
    assert_eq!(
        rejected,
        vec![Rejection::NotOverridable {
            section: SectionId::Identity,
            tier: CustomizationTier::Fixed,
        }]
    );
    assert!(
        result.blocks.iter().any(|b| matches!(
            &b.body,
            BlockBody::Text { text } if text == "CUSTOM_WORKFLOW"
        )),
        "the permitted override still lands"
    );
}

#[test]
fn application_order_is_canonical_not_authoring_order() {
    let package = bundled_fallback_package();
    let forwards = package.with_overrides(&[
        over(SectionId::Core, "C"),
        over(SectionId::Workflow, "W"),
        over(SectionId::Memory, "M"),
    ]);
    let backwards = package.with_overrides(&[
        over(SectionId::Memory, "M"),
        over(SectionId::Workflow, "W"),
        over(SectionId::Core, "C"),
    ]);
    assert_eq!(forwards, backwards);
}

#[test]
fn an_empty_override_body_is_rejected_by_the_applier_too() {
    // Belt to the scanner's braces: even if an empty body reached this far it
    // would keep the bundled section rather than blank it.
    let package = bundled_fallback_package();
    let (result, rejected) = package.with_overrides(&[over(SectionId::Workflow, "  \n ")]);
    assert_eq!(result, package);
    assert_eq!(
        rejected,
        vec![Rejection::EmptyBody {
            section: SectionId::Workflow
        }]
    );
}

#[test]
fn a_section_with_no_text_block_has_nothing_to_override() {
    // Contrived package: Workflow's only block is generated, so there is no
    // authored text for an override to replace. Rejected rather than injected
    // at some arbitrary position.
    let mut package = bundled_fallback_package();
    for b in &mut package.blocks {
        if b.section == SectionId::Workflow {
            b.body = BlockBody::Generated {
                generator: Generator::StackProfile,
            };
        }
    }
    let (result, rejected) = package.with_overrides(&[over(SectionId::Workflow, "X")]);
    assert_eq!(result, package);
    assert_eq!(
        rejected,
        vec![Rejection::NoTextBlock {
            section: SectionId::Workflow
        }]
    );
}

#[test]
fn an_override_that_would_leave_a_section_silent_is_rejected() {
    // Contrived package: Workflow's only text block is `optional`, so applying
    // the override would produce a section that composes to nothing whenever it
    // is dropped. Rejected — the bundled section is kept instead.
    let mut package = bundled_fallback_package();
    for b in &mut package.blocks {
        if b.section == SectionId::Workflow {
            b.optional = true;
        }
    }
    let (result, rejected) = package.with_overrides(&[over(SectionId::Workflow, "X")]);
    assert_eq!(result, package, "the bundled section is kept");
    assert_eq!(
        rejected.first(),
        Some(&Rejection::WouldEmitNothing {
            section: SectionId::Workflow
        })
    );
}

#[test]
fn an_undeclared_section_is_rejected_and_the_package_is_reverted() {
    // Contrived package whose taxonomy is missing Workflow: the override has
    // nowhere declared to land, and the final validation then refuses the whole
    // thing — degrading to the package as supplied, never to a broken one.
    let mut package = bundled_fallback_package();
    package.sections.retain(|s| s.id != SectionId::Workflow);

    let (result, rejected) = package.with_overrides(&[over(SectionId::Workflow, "X")]);
    assert_eq!(result, package, "the unoverridden package is returned");
    assert_eq!(
        rejected,
        vec![
            Rejection::UnknownSection {
                section: SectionId::Workflow
            },
            Rejection::PackageInvalid(ValidationError::SectionsNotCanonical {
                found: result.sections.iter().map(|s| s.id).collect(),
            }),
        ]
    );
}

#[test]
fn an_override_that_cannot_validate_is_discarded_and_the_package_reverts() {
    // Contrived package carrying an overridable Memory block AFTER the floor,
    // so anything derived from it fails `OverridableAfterFloor`. Overriding
    // Workflow cannot fix that, so the override is discarded on its own
    // diagnosis, the final validation then refuses the whole, and the package as
    // supplied comes back. Applying an override must never make things worse
    // than not applying it.
    let mut package = bundled_fallback_package();
    package.blocks.push(InstructionBlock {
        section: SectionId::Memory,
        body: BlockBody::Text {
            text: "TRAILING".to_string(),
        },
        join_before: Join::Rule,
        optional: false,
    });

    let (result, rejected) = package.with_overrides(&[over(SectionId::Workflow, "X")]);
    assert_eq!(
        result, package,
        "the supplied package is returned unchanged"
    );
    assert!(
        matches!(
            rejected.as_slice(),
            [
                Rejection::Invalidates {
                    section: SectionId::Workflow,
                    error: ValidationError::OverridableAfterFloor { .. },
                },
                Rejection::PackageInvalid(ValidationError::OverridableAfterFloor { .. }),
            ]
        ),
        "unexpected rejections: {rejected:?}"
    );
}

// ---------------------------------------------------------------------------
// WIRING — end to end through `resolve_pm_prompt`
// ---------------------------------------------------------------------------

#[test]
fn claude_md_workflow_override_reaches_the_delivered_prompt() {
    // THE wiring gate. Revert the `resolve_pm_prompt_with_roster` call into
    // `compose_bundled_fallback_with_overrides` and this fails: the bundled
    // workflow heading comes back and the override text never appears. A reader
    // nothing calls is issue #381 with a new file name.
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &block(SectionId::Workflow, "# Custom Workflow\n\nTWO_PHASE_ONLY"),
    );

    let (prompt, source) = resolve(tmp.path());
    assert_eq!(source, PromptSource::Package);
    assert!(
        prompt.contains("TWO_PHASE_ONLY"),
        "override must be delivered"
    );
    assert!(
        !prompt.contains("# PM Workflow Configuration"),
        "the bundled workflow section must be replaced"
    );
    // Every other section is untouched.
    assert!(prompt.contains("# PM Agent -- Trusty MPM"));
    assert!(prompt.contains("# Agent Delegation Routing"));
    assert!(prompt.contains("Generated with trusty-mpm"));
}

#[test]
fn a_floor_marker_composes_byte_identically_to_no_claude_md() {
    // Per-floor-section acceptance gate: a `CLAUDE.md` that tries to replace
    // any floor section delivers EXACTLY the bytes a project with no
    // `CLAUDE.md` receives. Not "mostly the same" — the same.
    let baseline = TempDir::new().unwrap();
    let (expected, _) = resolve(baseline.path());

    for section in [
        SectionId::Identity,
        SectionId::NonOverridableRules,
        SectionId::FrameworkGuaranteedConventions,
    ] {
        let tmp = TempDir::new().unwrap();
        write_claude_md(tmp.path(), &block(section, "SUBVERTED_FLOOR"));
        let (prompt, source) = resolve(tmp.path());
        assert_eq!(source, PromptSource::Package);
        assert!(
            !prompt.contains("SUBVERTED_FLOOR"),
            "{section:?} override must not reach the prompt"
        );
        assert_eq!(
            prompt, expected,
            "{section:?} override must not move a single byte"
        );
    }
}

#[test]
fn the_live_roster_survives_an_agent_delegation_override() {
    // #4196 in override form, end to end.
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &block(SectionId::AgentDelegation, "# Custom Routing\n\nROUTE_ALL"),
    );

    let (prompt, source) = resolve(tmp.path());
    assert_eq!(source, PromptSource::Package);
    assert!(prompt.contains("ROUTE_ALL"), "the override lands");
    assert!(
        !prompt.contains("## Make / Mise Command Routing"),
        "the bundled doctrine is replaced"
    );
    assert!(
        prompt.contains("## Delegation Authority") && prompt.contains("### ticketing"),
        "the LIVE roster must still be emitted: {prompt}"
    );
}

#[test]
fn tm_never_writes_claude_md() {
    // Standing constraint (#2170): resolving a prompt READS the project's
    // `CLAUDE.md` and must never create, rewrite or touch it.
    let authored = format!(
        "# Mine\n\ndo not touch   \n\n{}",
        block(SectionId::Workflow, "CUSTOM")
    );
    let tmp = TempDir::new().unwrap();
    write_claude_md(tmp.path(), &authored);
    let (prompt, _) = resolve(tmp.path());
    assert!(prompt.contains("CUSTOM"));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap(),
        authored,
        "the project's CLAUDE.md must be byte-identical after resolution"
    );

    // And an absent one is not seeded by this path either.
    let empty = TempDir::new().unwrap();
    let _ = resolve(empty.path());
    assert!(
        !empty.path().join("CLAUDE.md").exists(),
        "resolving must not create a CLAUDE.md"
    );
}

#[test]
fn named_sections_are_reported_unapplied_on_the_legacy_path() {
    // A project still carrying a legacy `.trusty-mpm/` override file composes
    // through the sectionless legacy assembly, so its named sections cannot
    // land. That must be loud (`warn_unapplied`) and must not corrupt the
    // prompt — never silent, which is the #381 failure.
    let tmp = TempDir::new().unwrap();
    write_claude_md(tmp.path(), &block(SectionId::Workflow, "NAMED_WORKFLOW"));
    write_trusty_file(tmp.path(), FILE_AGENT_DELEGATION, "# Custom Routing\n\nX\n");

    let (prompt, source) = resolve(tmp.path());
    assert_eq!(source, PromptSource::Legacy);
    assert!(!prompt.contains("NAMED_WORKFLOW"));
    assert!(prompt.contains("# PM Workflow Configuration"));
    assert!(prompt.contains("# BASE_PM Framework Floor"));
    assert_eq!(
        scan_project(tmp.path()).overrides.len(),
        1,
        "and it is reported"
    );
}

#[test]
fn an_unmarked_instructions_md_still_feeds_the_project_addendum() {
    // No regression to the existing additive-addendum behaviour: a marker-free
    // `.trusty-mpm/INSTRUCTIONS.md` is delivered exactly as it is today.
    let tmp = TempDir::new().unwrap();
    write_trusty_file(
        tmp.path(),
        FILE_INSTRUCTIONS,
        "# Project Rules\n\nALWAYS_RUN_MAKE_CHECK\n",
    );

    let (prompt, source) = resolve(tmp.path());
    assert_eq!(source, PromptSource::Package);
    assert!(prompt.contains("ALWAYS_RUN_MAKE_CHECK"));
    let addendum = prompt.find("ALWAYS_RUN_MAKE_CHECK").expect("addendum");
    let base = prompt.find("# BASE_PM Framework Floor").expect("floor");
    assert!(addendum < base, "the addendum still precedes the floor");
}

#[test]
fn a_marked_block_in_instructions_md_is_delivered_once() {
    // `INSTRUCTIONS.md` is both a marker host and the addendum source. The
    // marked block must arrive as the override it is, NOT also as raw prose.
    let tmp = TempDir::new().unwrap();
    write_trusty_file(
        tmp.path(),
        FILE_INSTRUCTIONS,
        &format!(
            "# Project Rules\n\nSTILL_ADDITIVE\n\n{}",
            block(SectionId::Workflow, "SECTIONED_WORKFLOW")
        ),
    );

    let (prompt, source) = resolve(tmp.path());
    assert_eq!(source, PromptSource::Package);
    assert!(
        prompt.contains("STILL_ADDITIVE"),
        "unmarked prose still added"
    );
    assert!(
        !prompt.contains("TRUSTY-MPM:"),
        "markers never reach the prompt"
    );
    assert_eq!(
        prompt.matches("SECTIONED_WORKFLOW").count(),
        1,
        "the marked body is delivered exactly once, as the workflow section"
    );
    assert!(!prompt.contains("# PM Workflow Configuration"));
}
