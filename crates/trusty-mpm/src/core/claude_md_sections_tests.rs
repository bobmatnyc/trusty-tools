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
    FILE_INSTRUCTIONS, FILE_WORKFLOW, OVERRIDE_DIR_NAME, PromptSource,
    resolve_pm_prompt_with_roster,
};
use crate::core::instruction_package::Generator;
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

// `claude_md_wins_a_same_section_collision_with_instructions_md` and
// `instructions_md_supplies_sections_claude_md_does_not_claim` were deleted with
// the second marker host (#4286). Both asserted cross-host precedence between
// `CLAUDE.md` and `.trusty-mpm/INSTRUCTIONS.md`; with one host there is no
// cross-host case to settle. `a_marker_in_the_retired_instructions_file_is_not_
// read` and `claude_md_is_the_only_marker_host` cover the replacement contract.
//
// `scan_project` keeps its `REASON_SHADOWED` arm — see `HOST_FILES` — so adding
// a host back stays a data change. `duplicate_section_in_one_host_keeps_the_first`
// below keeps that arm's sibling (`REASON_DUPLICATE`) exercised.

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

// `strip_is_a_no_op_without_markers` and
// `strip_removes_marked_blocks_from_the_addendum` were deleted with
// `strip_marker_blocks` itself (#4286). Both covered de-duplicating a marked
// block inside `.trusty-mpm/INSTRUCTIONS.md`, which was simultaneously a marker
// host and the `project-addendum` source. Retiring that file removed both roles,
// so the function had no caller and the tests had no behaviour to pin. The
// replacement coverage — that the retired file is no longer a marker host at
// all — is `a_marker_in_the_retired_instructions_file_is_not_read` below.

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
    let package = bundled_fallback_package().expect("manifest parses");
    let (result, rejected) = package.with_overrides(&[]);
    assert_eq!(&result, package);
    assert_eq!(rejected, vec![]);
}

#[test]
fn floor_sections_refuse_every_named_section_override() {
    // The structural guarantee: the package's `customization_tier` is the only
    // authority, and it says `fixed` for every floor section. No list in the
    // reader is consulted, so none can drift out of sync with this.
    let package = bundled_fallback_package().expect("manifest parses");
    // The loop set is derived from `is_floor()`, never hand-listed: #4573 added
    // a fourth floor section, and a hand-listed set would have kept passing
    // while the new one went untested — the exact drift shape that let the
    // Prohibitions table ship at tier `project`.
    for section in SectionId::CANONICAL
        .into_iter()
        .filter(|id| *id == SectionId::Core)
    {
        let (result, rejected) = package.with_overrides(&[over(section, "SUBVERTED")]);
        assert_eq!(&result, package, "{section:?} must be untouched");
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
    // #4286 flipped which sections these are: `core` became the one protected
    // section and the four former floor sections joined the overridable set.
    let package = bundled_fallback_package().expect("manifest parses");
    for section in [
        SectionId::Identity,
        SectionId::Memory,
        SectionId::Search,
        SectionId::Workflow,
        SectionId::AgentDelegation,
        SectionId::Enforcement,
        SectionId::NonOverridableRules,
        SectionId::FrameworkGuaranteedConventions,
    ] {
        let (result, rejected) = package.with_overrides(&[over(section, "REPLACED_BODY")]);
        assert_eq!(rejected, vec![], "{section:?} is tier project");
        assert_ne!(&result, package, "{section:?} must actually change");
        // Every bundled section is a schema-v2 `file` body, so this also pins the
        // property #4318 could have silently broken: an override must replace an
        // authored block whether that block is inline `text` or a `file`
        // reference, and must collapse the section to exactly one authored block.
        let texts: Vec<&str> = result
            .blocks
            .iter()
            .filter(|b| b.section == section)
            .filter_map(|b| match &b.body {
                BlockBody::Text { text } => Some(text.as_str()),
                BlockBody::File { .. } | BlockBody::Generated { .. } => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["REPLACED_BODY"],
            "{section:?} keeps exactly one authored block, rewritten as inline text"
        );
        assert!(
            !result
                .blocks
                .iter()
                .any(|b| b.section == section && matches!(b.body, BlockBody::File { .. })),
            "{section:?} must not keep a file body alongside the override"
        );
        result.validate().expect("overridden package validates");
    }
}

#[test]
fn agent_delegation_override_keeps_the_generated_roster() {
    // The #4196 shape in override form: a project must be able to rewrite the
    // routing doctrine WITHOUT being able to suppress the live agent roster.
    let package = bundled_fallback_package().expect("manifest parses");
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
    let package = bundled_fallback_package().expect("manifest parses");
    let (result, rejected) = package.with_overrides(&[
        over(SectionId::Core, "SUBVERTED"),
        over(SectionId::Workflow, "CUSTOM_WORKFLOW"),
    ]);
    assert_eq!(
        rejected,
        vec![Rejection::NotOverridable {
            section: SectionId::Core,
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
    let package = bundled_fallback_package().expect("manifest parses");
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
    let package = bundled_fallback_package().expect("manifest parses");
    let (result, rejected) = package.with_overrides(&[over(SectionId::Workflow, "  \n ")]);
    assert_eq!(&result, package);
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
    let mut package = bundled_fallback_package().expect("manifest parses").clone();
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
    let mut package = bundled_fallback_package().expect("manifest parses").clone();
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
    let mut package = bundled_fallback_package().expect("manifest parses").clone();
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
    // Contrived package that cannot validate for a reason no override can fix:
    // a second `fixed` section, which #4286's core-only tier invariant rejects.
    // Overriding Workflow does not repair it, so the override is discarded on
    // its own diagnosis, the final validation refuses the whole, and the package
    // as supplied comes back. Applying an override must never make things worse
    // than not applying it.
    //
    // Before #4286 this used a Memory block placed after the floor, tripping
    // `OverridableAfterFloor`; that rule died with the floor.
    let mut package = bundled_fallback_package().expect("manifest parses").clone();
    for section in &mut package.sections {
        if section.id == SectionId::NonOverridableRules {
            section.customization_tier = CustomizationTier::Fixed;
        }
    }

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
                    error: ValidationError::TierNotCoreOnly { .. },
                },
                Rejection::PackageInvalid(ValidationError::TierNotCoreOnly { .. }),
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

    for section in SectionId::CANONICAL
        .into_iter()
        .filter(|id| *id == SectionId::Core)
    {
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

// ---------------------------------------------------------------------------
// #4399 REGRESSION GATES — RETIRED AS UNREACHABLE (#4286)
// ---------------------------------------------------------------------------
//
// Six tests lived here: `an_unrelated_legacy_file_does_not_shadow_a_named_
// section_override` and its memory/delegation variants, `a_same_section_legacy_
// file_still_wins_over_a_named_override_and_is_reported`, `identity_core_and_
// search_stay_reported_unapplied_on_the_legacy_path`, plus the two
// `.trusty-mpm/INSTRUCTIONS.md` addendum tests.
//
// Every one of them constructed the same precondition: a legacy `.trusty-mpm/`
// file forcing the sectionless string assembly, with a CLAUDE.md named override
// competing against it. #4286 removed the read path, so that precondition can no
// longer be built — a legacy file cannot force any branch, because it is not
// read. The tests were not deleted to make a red suite green; they were deleted
// because the state they set up is now unconstructible.
//
// The guarantee they protected is stronger now and is proven below by
// `a_legacy_file_cannot_shadow_a_named_override_because_it_is_not_read`, which
// asserts the property directly rather than through the collision that used to
// threaten it.

#[test]
fn a_legacy_file_cannot_shadow_a_named_override_because_it_is_not_read() {
    // The #4399 bug class, closed by construction. Previously a legacy file for
    // section A silently dropped a CLAUDE.md override for unrelated section B.
    // Now: a project with ALL FIVE legacy files present still receives every one
    // of its CLAUDE.md named overrides, and the composed prompt is byte-identical
    // to the same project with no legacy files at all.
    let with_legacy = TempDir::new().unwrap();
    let clean = TempDir::new().unwrap();
    let claude_md = format!(
        "{}\n\n{}\n",
        block(SectionId::Workflow, "NAMED_WORKFLOW"),
        block(SectionId::AgentDelegation, "NAMED_ROUTING")
    );
    for project in [with_legacy.path(), clean.path()] {
        write_claude_md(project, &claude_md);
    }
    for name in crate::core::instruction_overrides::LEGACY_OVERRIDE_FILES {
        write_trusty_file(with_legacy.path(), name, "LEGACY_CONTENT\n");
    }

    let (with_prompt, _) = resolve(with_legacy.path());
    let (clean_prompt, _) = resolve(clean.path());

    assert!(with_prompt.contains("NAMED_WORKFLOW"));
    assert!(with_prompt.contains("NAMED_ROUTING"));
    assert!(!with_prompt.contains("LEGACY_CONTENT"));
    assert_eq!(
        with_prompt, clean_prompt,
        "legacy files must not perturb the composed prompt in any way"
    );
}

#[test]
fn a_marker_in_the_retired_instructions_file_is_not_read() {
    // `.trusty-mpm/INSTRUCTIONS.md` was the second marker host until #4286.
    // A marked block there must now be ignored entirely — the file is not a
    // customization surface in any form.
    let tmp = TempDir::new().unwrap();
    write_trusty_file(
        tmp.path(),
        FILE_INSTRUCTIONS,
        &block(SectionId::Workflow, "FROM_RETIRED_HOST"),
    );

    let scanned = scan_project(tmp.path());
    assert!(
        scanned.overrides.is_empty(),
        "the retired file must yield no overrides"
    );
    assert!(!resolve(tmp.path()).0.contains("FROM_RETIRED_HOST"));
}

#[test]
fn claude_md_is_the_only_marker_host() {
    assert_eq!(HOST_FILES, ["CLAUDE.md"]);
}

// #4573 — the authority tables are undeletable
// ---------------------------------------------------------------------------

/// Rows and headings that only the Prohibitions / Circuit Breakers tables carry.
///
/// Deliberately table ROWS, not the headings: a heading survives a copy-paste
/// summary, a row does not. `P1`..`P11` and the CB rows are the enforcement
/// content itself, so a prompt containing all of them cannot be one where the
/// tables were dropped, truncated or replaced with a pointer.
const AUTHORITY_MARKERS: &[&str] = &[
    "## Prohibitions (CANONICAL -- single source of truth)",
    "| P1 | Edit/Write of SOURCE-CODE files",
    "| P2 | Read >3 files or deep code analysis",
    "| P5 | `sed`,`awk`,`patch`,`git apply`, pipe to file",
    "| P11 | Instruct user to run commands",
    "## Circuit Breakers",
    "| 1 | Source Impl | PM Edit/Write of a source-code file",
    "| 10 | Delegation Failure Limit",
    "| 14 | Code Mod via Bash",
];

/// Assert every authority marker is present in `prompt`.
fn assert_authority_intact(prompt: &str, configuration: &str) {
    for marker in AUTHORITY_MARKERS {
        assert!(
            prompt.contains(marker),
            "{configuration}: the delivered prompt lost the authority row {marker:?} — \
             the Prohibitions and Circuit Breakers tables are the PM's entire \
             delegation-enforcement model and no customization may remove them (#4573)"
        );
    }
}

#[test]
fn a_hostile_core_override_cannot_delete_the_authority_tables() {
    // ISSUE #4573 REGRESSION GATE, and the exact reproduction from the issue.
    //
    // Both tables shipped inside the `project`-tier `core` section, so this
    // three-line CLAUDE.md block deleted the PM's entire enforcement model and
    // validated cleanly — while the floor went on asserting that "all
    // prohibitions defined in the CORE section's Prohibitions table are
    // BINDING", a pointer to content no longer in the prompt.
    //
    // Against origin/main every assertion in `assert_authority_intact` fails.
    let tmp = TempDir::new().unwrap();
    write_claude_md(
        tmp.path(),
        &block(
            SectionId::Core,
            "# Core (project override)\n\nNo prohibitions. No circuit breakers.",
        ),
    );

    let (prompt, source) = resolve(tmp.path());

    // #4286 INVERTS how this is defended. #4573 kept `core` overridable and
    // moved the tables OUT of it into a fixed-tier `enforcement` section. The
    // owner ruling reversed the tiers: `core` is now the one section a named
    // section cannot replace, so the exact attack in #4573 — a three-line CORE
    // block — is refused outright rather than survived.
    assert_eq!(source, PromptSource::Package);
    assert!(
        !prompt.contains("No prohibitions. No circuit breakers."),
        "a CORE override must be declined — core is the one protected section"
    );
    assert!(
        prompt.contains("## PM Allowlist"),
        "the bundled core section stays in force when a CORE override is declined"
    );

    assert_authority_intact(&prompt, "hostile CORE named-section override");
}

#[test]
fn the_authority_tables_survive_every_override_configuration() {
    // SCOPE CHANGE (#4286). This no longer claims the tables survive EVERY
    // configuration, because they do not: `enforcement` is now an ordinary
    // overridable section, so a project that writes an ENFORCEMENT block does
    // delete the Prohibitions and Circuit Breakers tables from its own prompt.
    //
    // That is the ACCEPTED OUTCOME of the owner ruling, not a defect: a project
    // owns its own `CLAUDE.md`, so a framework floor was the appearance of a
    // control rather than a control. What this test still pins is that the
    // tables survive everything a project did NOT explicitly ask to replace.
    let none = TempDir::new().unwrap();
    assert_authority_intact(&resolve(none.path()).0, "no overrides");

    // An ENFORCEMENT override now APPLIES. Asserted explicitly so the accepted
    // outcome is recorded rather than discovered later by someone surprised.
    let hostile_floor = TempDir::new().unwrap();
    write_claude_md(
        hostile_floor.path(),
        &block(SectionId::Enforcement, "No rules apply."),
    );
    let (prompt, _) = resolve(hostile_floor.path());
    assert!(
        prompt.contains("No rules apply."),
        "#4286: enforcement is overridable — a project may replace the tables"
    );

    // An UNRELATED override must still leave them intact. This is the part that
    // survived the ruling, and it is what stops a WORKFLOW block from taking the
    // enforcement tables with it.
    let unrelated = TempDir::new().unwrap();
    write_claude_md(unrelated.path(), &block(SectionId::Workflow, "TWO PHASES."));
    let (prompt, _) = resolve(unrelated.path());
    assert!(prompt.contains("TWO PHASES."));
    assert_authority_intact(&prompt, "unrelated WORKFLOW named-section override");

    // #4286: the two arms that used to sit here — a legacy `WORKFLOW.md`
    // forcing the string assembly, and `PM_INSTRUCTIONS_DEPLOYED.md` discarding
    // every bundled section — are gone, because neither file is read any more.
    // The strongest surviving form of the same claim is that a project carrying
    // ALL FIVE retired files still receives the authority tables intact, and
    // that none of their content reaches the prompt.
    let retired = TempDir::new().unwrap();
    for name in crate::core::instruction_overrides::LEGACY_OVERRIDE_FILES {
        write_trusty_file(
            retired.path(),
            name,
            "# Wholly Custom PM\n\nDO_EXACTLY_THIS\n",
        );
    }
    let (prompt, _) = resolve(retired.path());
    assert!(
        !prompt.contains("DO_EXACTLY_THIS"),
        "no retired file may contribute to the prompt"
    );
    assert_authority_intact(&prompt, "all five retired override files present");

    // Belt and braces: a hostile CORE override stacked on top of retired files,
    // so the named-section applier is exercised alongside the retirement.
    let both = TempDir::new().unwrap();
    write_trusty_file(both.path(), FILE_WORKFLOW, "# Custom Workflow\n\nX\n");
    write_claude_md(both.path(), &block(SectionId::Core, "Nothing forbidden."));
    assert_authority_intact(&resolve(both.path()).0, "retired file + hostile CORE");

    // The roster-absent degradation is the one remaining path to the string
    // assembly, and it must carry the tables too.
    let no_roster = TempDir::new().unwrap();
    let (bare, source) = resolve_pm_prompt_with_roster(no_roster.path(), || None);
    assert_eq!(source, PromptSource::Legacy);
    assert_authority_intact(&bare, "roster-absent string assembly");
}

#[test]
fn no_floor_text_points_at_content_outside_the_floor() {
    // #4573 Defect B, generalised. The floor used to state that the prohibitions
    // "defined in the CORE section's Prohibitions table" were binding — a
    // reference from non-overridable text into overridable content, so deleting
    // core left the floor asserting a table that was not in the prompt.
    //
    // Any phrasing that binds the PM to another SECTION by name is that same
    // shape, because only the floor is guaranteed present. Assert the class, not
    // the one sentence that was fixed.
    let package = bundled_fallback_package().expect("manifest parses");
    // The four former-floor sections, named explicitly. They are no longer a
    // tier grouping (#4286 removed the floor), but they are still the sections
    // that carry this content, and projecting them alone is what stops the
    // assertion passing because some other section happens to restate it.
    let floor: String = package.authored_run(&[
        SectionId::Identity,
        SectionId::Enforcement,
        SectionId::NonOverridableRules,
        SectionId::FrameworkGuaranteedConventions,
    ]);

    for token in SectionId::CANONICAL
        .into_iter()
        .filter(|id| *id != SectionId::Core)
        .map(section_token)
    {
        assert!(
            !floor.contains(&format!("{token} section's")),
            "floor text points into the overridable {token} section; a floor rule may \
             only reference content that is itself in the floor (#4573)"
        );
    }
    // And the specific broken pointer, named so a revert is caught by name.
    assert!(
        !floor.contains("CORE section's Prohibitions table"),
        "the #4573 dangling reference must not come back"
    );
    assert!(
        !floor.contains("PM_INSTRUCTIONS.md"),
        "the #4183-deleted PM_INSTRUCTIONS.md must not be referenced by the floor"
    );
    // The pointee is where the pointer now says it is.
    assert_authority_intact(&floor, "the floor projected on its own");

    // …and literally where it says: the replacement sentence reads "the
    // Prohibitions table above". `validate_floor_is_last` guarantees the floor
    // is a contiguous TAIL but says nothing about order WITHIN it, so "above"
    // is a manifest-order fact that needs its own assertion or the pointer
    // degrades from broken to merely wrong.
    let tables = floor
        .find("## Prohibitions (CANONICAL -- single source of truth)")
        .expect("the Prohibitions table is in the floor");
    let breakers = floor
        .find("## Circuit Breakers")
        .expect("the Circuit Breakers table is in the floor");
    let reference = floor
        .find("Every prohibition in the Prohibitions table above")
        .expect("the floor's binding statement");
    assert!(
        tables < reference && breakers < reference,
        "both tables must precede the sentence that says they are `above`"
    );
}

// ---------------------------------------------------------------------------
// #4594 — delegation is a default with an action BUDGET, and the budget has
// two halves
// ---------------------------------------------------------------------------

/// Collapse every whitespace run to one space.
///
/// Why: the floor is hard-wrapped prose, so a literal needle spanning a line
/// break would fail on a pure re-wrap that changed no rule — a false alarm that
/// gets tests deleted. Normalising pins the SENTENCE while leaving the wrapping
/// free.
fn unwrapped(text: &str) -> String {
    // Strip the blockquote continuation marker before flattening. The canonical
    // budget statement in `enforcement.md` is a `>` quote, so a `>` lands
    // mid-sentence at every line break; without this, a needle would silently
    // depend on where the source happens to wrap — the exact fragility this
    // helper exists to remove.
    text.lines()
        .map(|l| l.trim_start().trim_start_matches('>'))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Assert both halves of the direct-action budget survive into `prompt`.
///
/// The mid-flight half is the clause most likely to be silently dropped by a
/// future edit: an implementation that keeps only the pre-task estimate reads
/// complete and leaves the real failure mode — the PM that started in good faith
/// and keeps going on action 4 — unaddressed.
///
/// Every needle below is drawn from `sections/enforcement.md`'s canonical
/// "The direct-action budget (P1 and P5 only)" subsection, deliberately. The
/// rule used to be restated verbatim in `identity.md`, `core.md` and
/// `non-overridable-rules.md`, and two of these needles matched only
/// `identity.md`'s wording — so the helper was gated on a RESTATEMENT that
/// `the_budget_survives_every_override_configuration` itself proves is
/// deletable by an `IDENTITY` block. Keying every needle to the canonical
/// subsection asserts the same two halves against the copy that survives every
/// override configuration.
fn assert_budget_intact(prompt: &str, configuration: &str) {
    let flat = unwrapped(prompt);
    let needles: &[(&str, &str)] = &[
        (
            "The PM delegates when it believes a task will take more than 3 direct actions",
            "the up-front estimate half of the direct-action budget",
        ),
        (
            "or when it is unable to complete the task in 3",
            "the second clause of the owner's governing sentence",
        ),
        (
            "Anything you believe needs more than 3 direct actions is delegated, never begun",
            "the up-front estimate stated as a rule, not only as the owner's sentence",
        ),
        (
            "The estimate is not a licence to finish",
            "the MID-FLIGHT HANDOFF half — a PM whose 3-action estimate stops \
             holding must hand off the remainder",
        ),
        (
            "Do not take a fourth direct action to finish work you misjudged",
            "the action the mid-flight rule forbids",
        ),
        (
            "The user can always override",
            "the user override, which the ruling keeps",
        ),
    ];
    for (needle, what) in needles {
        assert!(
            flat.contains(needle),
            "{configuration}: the delivered prompt lost {what} (#4594); \
             missing {needle:?}"
        );
    }
}

#[test]
fn the_direct_action_budget_states_both_halves_in_the_floor() {
    // The owner's ruling (#4594) is a DEFAULT WITH A BUDGET, expressed in
    // ACTIONS, with three parts that only work together: the absolute phrasing
    // is gone, the user override stays, and the budget generalizes from file
    // changes to actions. Assert it on the floor projected alone, so this
    // cannot pass because a project-tier section happens to restate it.
    let package = bundled_fallback_package().expect("manifest parses");
    // The four former-floor sections, named explicitly. They are no longer a
    // tier grouping (#4286 removed the floor), but they are still the sections
    // that carry this content, and projecting them alone is what stops the
    // assertion passing because some other section happens to restate it.
    let floor: String = package.authored_run(&[
        SectionId::Identity,
        SectionId::Enforcement,
        SectionId::NonOverridableRules,
        SectionId::FrameworkGuaranteedConventions,
    ]);

    assert_budget_intact(&floor, "the floor projected on its own");
    let flat = unwrapped(&floor);

    // Consequence 1: the absolute phrasing the ruling retires must not come
    // back. These are the exact assertions #4594 names at identity.md:7 and
    // non-overridable-rules.md:3-5.
    assert!(
        !flat.contains("never direct impl"),
        "the retired absolute phrasing must not come back (#4594)"
    );
    assert!(
        !flat.contains(
            "remove them. No cost-saving, \"trivial change\", or \"documented command\" exceptions."
        ),
        "the blanket no-exceptions sentence must stay scoped to P2-P4/P6-P11 (#4594)"
    );

    // Consequence 3: expressed in ACTIONS, not only file changes. The hook's
    // file-change limit is still described, but as the mechanical floor of a
    // broader budget rather than as the budget itself.
    assert!(
        flat.contains("One direct action = one PM-executed step of implementation work"),
        "the budget must be denominated in direct ACTIONS, not files (#4594)"
    );
    assert!(
        flat.contains("the hook sees files, not actions"),
        "the actions/files gap must be stated, or `pm_guard` compliance reads \
         as budget compliance (#4594)"
    );

    // The prohibitions the ruling explicitly does NOT relax stay absolute.
    assert!(
        flat.contains("All OTHER prohibitions (P2–P4, P6–P11) are routing rules"),
        "P2-P4 and P6-P11 must remain absolute, no budget (#4594)"
    );
}

#[test]
fn the_pm_allowlist_does_not_contradict_the_action_budget() {
    // The FIFTH incompatible statement, caught in review of the #4594 fix and
    // the same defect class the issue was filed on. The allowlist sat near the
    // TOP of the compiled prompt — read first — and its only write row said
    // "NOT source code", while the floor ~1000 lines later said P1/P5 are
    // budgeted at 3 direct actions "including one Edit, one Write". A prompt
    // asserting both lets the PM cite whichever suits it.
    //
    // Asserted on the BUNDLED DEFAULT prompt, not the floor projection: the
    // allowlist is project-tier by design, so a project may legitimately
    // replace it. What must never ship is a DEFAULT that contradicts the floor.
    let tmp = TempDir::new().unwrap();
    let flat = unwrapped(&resolve(tmp.path()).0);

    assert!(
        !flat.contains("docs, config — NOT source code, NOT bulk edits"),
        "the allowlist must not assert source edits are off-limits outright \
         while the floor budgets them (#4594)"
    );
    assert!(
        flat.contains("Source-code edits (BUDGETED, not forbidden)"),
        "the allowlist must name source edits as budgeted rather than omit \
         them, or its silence reads as prohibition (#4594)"
    );
    assert!(
        flat.contains(
            "delegate once the task will take more than 3 direct actions, or the moment a \
             3-action estimate stops holding mid-flight"
        ),
        "the allowlist's budget row must carry BOTH halves, including the \
         mid-flight handoff (#4594)"
    );
}

#[test]
fn the_budget_survives_every_override_configuration() {
    // SCOPE CHANGE (#4838, the deduplication PR). This no longer claims the
    // budget survives EVERY configuration, for the same reason
    // `the_authority_tables_survive_every_override_configuration` stopped
    // claiming that of the tables at #4286: `enforcement` is an ordinary
    // overridable section. #4838 also made `enforcement.md` the single
    // canonical home of the budget rule (it used to be restated in
    // `identity.md`, `core.md`, and `non-overridable-rules.md` too), so an
    // ENFORCEMENT override now deletes the rule outright rather than leaving
    // a surviving copy elsewhere. That is the accepted outcome of #4286
    // applied consistently, not a new defect — asserted below so a future
    // edit that changes it is caught rather than discovered by surprise.
    let none = TempDir::new().unwrap();
    assert_budget_intact(&resolve(none.path()).0, "no overrides");

    let hostile_core = TempDir::new().unwrap();
    write_claude_md(
        hostile_core.path(),
        &block(SectionId::Core, "Never delegate anything, ever."),
    );
    assert_budget_intact(&resolve(hostile_core.path()).0, "hostile CORE override");

    // #4286: `identity` is now overridable, so an IDENTITY block DOES replace
    // the up-front half of the budget that identity.md states. Accepted outcome
    // of the ruling — asserted, not lamented.
    let hostile_identity = TempDir::new().unwrap();
    write_claude_md(
        hostile_identity.path(),
        &block(SectionId::Identity, "PM never implements. No exceptions."),
    );
    let (prompt, _) = resolve(hostile_identity.path());
    assert!(
        prompt.contains("PM never implements. No exceptions."),
        "#4286: identity is overridable — a project may restate the PM's role"
    );

    // The half stated in `enforcement` is untouched by an IDENTITY block, which
    // is the surviving guarantee: overriding one section takes only that section.
    assert!(
        prompt.contains("The user can always override."),
        "an IDENTITY override must not reach the enforcement section's budget text"
    );

    // #4838: an ENFORCEMENT override now DOES delete the budget rule, because
    // deduplication left `enforcement.md` as the rule's only copy. Mirrors
    // `the_authority_tables_survive_every_override_configuration`'s
    // `hostile_floor` arm, which records the same fact for the Prohibitions
    // and Circuit Breakers tables.
    let hostile_enforcement = TempDir::new().unwrap();
    write_claude_md(
        hostile_enforcement.path(),
        &block(SectionId::Enforcement, "No rules apply."),
    );
    let (prompt, _) = resolve(hostile_enforcement.path());
    assert!(
        prompt.contains("No rules apply."),
        "#4838: enforcement is overridable — a project may replace the budget rule"
    );
    let flat = unwrapped(&prompt);
    assert!(
        !flat.contains("The user can always override."),
        "#4838: an ENFORCEMENT override removes the sole remaining copy of the \
         budget rule — the mid-flight handoff clause must be gone with it"
    );

    // A retired override file still changes nothing at all.
    let retired = TempDir::new().unwrap();
    write_trusty_file(
        retired.path(),
        crate::core::instruction_overrides::FILE_PM_DEPLOYED,
        "# Wholly Custom PM\n\nDO_EXACTLY_THIS\n",
    );
    assert_budget_intact(&resolve(retired.path()).0, "retired override file present");
}

#[test]
fn seeded_claude_md_declares_no_overrides() {
    // REGRESSION GATE, found by running a real `tm` instance during #4286
    // acceptance and not by any unit test or golden.
    //
    // The seeded `CLAUDE.md` stub documents the marker grammar. A first draft
    // showed a worked example as a fenced code block:
    //
    //     <!-- TRUSTY-MPM: WORKFLOW START v=1 -->
    //     …your workflow, replacing the bundled one…
    //     <!-- TRUSTY-MPM: WORKFLOW END -->
    //
    // `parse_marker` matches whole lines and knows nothing about code fences, so
    // that example WAS a live override: every newly seeded project silently lost
    // its entire bundled workflow section and received the placeholder prose
    // instead. Observed in a real launch — bundled workflow heading count 0,
    // placeholder text present in the delivered prompt.
    //
    // The stub therefore may never contain a parseable marker pair. This asserts
    // the property directly against the shipped bytes.
    let tmp = TempDir::new().unwrap();
    let stub = crate::core::instruction_pipeline::CLAUDE_MD_STUB;
    std::fs::write(tmp.path().join("CLAUDE.md"), stub).unwrap();

    let scanned = scan_project(tmp.path());
    assert_eq!(
        scanned.overrides,
        vec![],
        "the seeded CLAUDE.md must declare NO overrides; it documents the \
         mechanism and must not exercise it"
    );
    assert_eq!(
        scanned.diagnostics,
        vec![],
        "and it must not emit marker diagnostics either — a warning on every \
         launch of every new project is not an acceptable way to document this"
    );

    // The stub still teaches the grammar (inline, so it cannot parse) and still
    // points at the compiled instructions.
    assert!(stub.contains("TRUSTY-MPM: WORKFLOW START"));
    assert!(stub.contains(".trusty-mpm/last-instructions.md"));

    // And the delivered prompt keeps its bundled workflow section.
    let (prompt, _) = resolve(tmp.path());
    assert!(
        prompt.contains("# PM Workflow Configuration"),
        "the bundled workflow section must survive a freshly seeded CLAUDE.md"
    );
}

// --- LOCATOR: the projection the writer splices against (#4754) -------------

/// One span per well-formed block, in file order, with the inclusive line
/// indices the writer needs.
#[test]
fn locate_blocks_finds_one_span_per_block() {
    let token = section_token(SectionId::Workflow);
    let text = format!(
        "intro\n\
         <!-- TRUSTY-MPM: {token} START v=1 -->\nfirst\n<!-- TRUSTY-MPM: {token} END -->\n\
         middle\n\
         <!-- TRUSTY-MPM: {token} START v=1 -->\nsecond\n<!-- TRUSTY-MPM: {token} END -->\n"
    );

    let located = locate_blocks(&text, SectionId::Workflow);

    assert_eq!(located.spans, vec![(1, 3), (5, 7)]);
    assert!(!located.unclosed);
    // The spans must really delimit the blocks: the named lines are the markers.
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[1].contains("START") && lines[3].contains("END"));
}

/// An unpaired `START` is surfaced, because it means the file's block extents
/// are unknown and the writer must decline to splice.
#[test]
fn locate_blocks_flags_an_unclosed_marker() {
    let token = section_token(SectionId::Memory);
    let text = format!("<!-- TRUSTY-MPM: {token} START v=1 -->\nno end marker\n");

    let located = locate_blocks(&text, SectionId::Memory);

    assert!(located.unclosed, "an unpaired START must be reported");
    assert!(located.spans.is_empty(), "and yield no span to splice over");
}

/// Blocks belonging to other sections are not returned — a writer targeting one
/// section must never splice over another's block.
#[test]
fn locate_blocks_ignores_other_sections() {
    let text = format!(
        "{}{}",
        block(SectionId::Memory, "memory body"),
        block(SectionId::Workflow, "workflow body")
    );

    let located = locate_blocks(&text, SectionId::Workflow);

    assert_eq!(located.spans.len(), 1, "only the WORKFLOW block");
    let lines: Vec<&str> = text.lines().collect();
    let (start, end) = located.spans[0];
    assert!(lines[start].contains(section_token(SectionId::Workflow)));
    assert!(lines[end].contains(section_token(SectionId::Workflow)));
}

// ── routing-surface consolidation (instruction-prompt dedup) ─────────────

/// Every routing mapping the collapsed tables used to carry, as
/// (needle, what it routes) pairs.
///
/// Six surfaces answered "which agent handles what" for the same ~11 agents:
/// `Agent Routing` (core), `When to Delegate to Each Agent`, `Ops Agent
/// Routing`, `Make / Mise Command Routing`, `Common User Request Routing`, and
/// the generated roster. Five were folded into one Routing Table. The mappings
/// below existed ONLY in a deleted table, so a future edit that trims the
/// surviving table would silently drop a real routing rule with nothing else
/// asserting it — which is the failure this test exists to catch, not the
/// wording of any one row.
const FOLDED_ROUTING_MAPPINGS: &[(&str, &str)] = &[
    // Make / Mise Command Routing — the whole table lived nowhere else. `mise`
    // in particular is absent from the Prohibitions table, so this section is
    // the ONLY statement that `mise run` targets are delegated at all.
    (
        "`make` and `mise run`",
        "make and mise targets are delegated",
    ),
    (
        "the PM never runs one directly",
        "the PM-never-runs-them rule",
    ),
    (
        "every `make` and `mise run` target",
        "make/mise → local-ops",
    ),
    (
        "build, dist, clean, install, setup",
        "build/dist/clean/install/setup → local-ops",
    ),
    (
        "version, bump, release, publish, deploy",
        "version/release/publish → local-ops",
    ),
    (
        "`pyproject.toml`, `package.json`",
        "the manifest-file release triggers",
    ),
    (
        "`test`, `lint`, `check` (or `engineer`)",
        "make/mise test/lint/check → qa or engineer",
    ),
    // Ops Agent Routing.
    (
        "localhost, PM2, npm, docker",
        "the local-ops trigger keywords",
    ),
    (
        "including anything unknown or ambiguous",
        "unknown/ambiguous → local-ops",
    ),
    (
        "generic `ops` agent is DEPRECATED",
        "the deprecated generic ops agent",
    ),
    (
        "EXAMPLES of routing, not an exhaustive list",
        "the not-exhaustive caveat",
    ),
    // Common User Request Routing.
    (
        "screenshot, click, navigate, DOM, console errors",
        "browser triggers → web-qa",
    ),
    (
        "never chrome-devtools, claude-in-chrome, or playwright",
        "the browser-tool ban",
    ),
    (
        "`research` → `engineer` → `local-ops` → `qa` → `documentation`",
        "the \"just do it\" full pipeline",
    ),
    // When to Delegate to Each Agent — the per-agent facts.
    (
        "Prefer the language-specific engineer",
        "the language-specific engineer preference",
    ),
    (
        "APPROVED / NEEDS_IMPROVEMENT / BLOCKED",
        "the code-analyzer verdict vocabulary",
    ),
    (
        "not interchangeable",
        "code-analyzer vs code-critic are distinct",
    ),
    (
        "never goes to `version-control`",
        "P6 — ticket bookkeeping stays with ticketing",
    ),
    (
        "Check git user for main-branch access",
        "the version-control git-user check",
    ),
    (
        "Triggers: \"skill\", \"stack\", \"framework\"",
        "the mpm-skills-manager triggers",
    ),
    (
        "Secret scanning, attack-vector detection",
        "the security capabilities",
    ),
    // core.md's Agent Routing — the per-agent default model column.
    ("| sonnet |", "the per-agent default-model column"),
    ("| opus |", "the per-agent default-model column"),
    ("| haiku |", "the per-agent default-model column"),
    // The verbatim-name rule, which gated both deleted tables.
    (
        "fails to dispatch (issue #4594)",
        "the pass-the-name-verbatim rule",
    ),
];

#[test]
fn the_surviving_routing_table_covers_every_folded_mapping() {
    let package = bundled_fallback_package().expect("manifest parses");
    let delegation = package.authored_run(&[SectionId::AgentDelegation]);

    for (needle, what) in FOLDED_ROUTING_MAPPINGS {
        assert!(
            delegation.contains(needle),
            "the collapsed routing tables lost {what}; missing {needle:?}"
        );
    }
}

#[test]
fn routing_lives_on_exactly_one_surface() {
    // The point of the collapse: a reader asking "which agent handles what"
    // must find one table, not six. `core.md` keeps a pointer, never a table.
    let package = bundled_fallback_package().expect("manifest parses");
    let core = package.authored_run(&[SectionId::Core]);

    for retired in [
        "## Ops Agent Routing",
        "## Make / Mise Command Routing",
        "## Common User Request Routing",
        "## When to Delegate to Each Agent",
    ] {
        assert!(
            !package
                .authored_run(&[SectionId::AgentDelegation])
                .contains(retired),
            "{retired} was folded into the Routing Table and must not return"
        );
    }

    assert!(
        core.contains("## Agent Routing"),
        "core keeps the heading as a pointer, so a reader is not left guessing"
    );
    // The retired table's exact header. Deliberately not a bare
    // "| `subagent_type` |": core legitimately keys its 5-phase workflow table
    // and its QA-target table on the same column, and neither is a routing
    // surface — they say which agent gates a phase, not which triggers route
    // to which agent.
    assert!(
        !core.contains("| `subagent_type` | Triggers | Default Model |"),
        "core must point at the Routing Table, never carry a second copy of it"
    );
}

#[test]
fn the_direct_action_budget_is_stated_once_and_pointed_at_elsewhere() {
    // The rule was stated four times. Only `enforcement.md` states it; the
    // other three sections point at it by its exact subsection title, so the
    // pointer cannot rot into a dangling reference unnoticed.
    let package = bundled_fallback_package().expect("manifest parses");
    const TITLE: &str = "The direct-action budget (P1 and P5 only)";

    let enforcement = package.authored_run(&[SectionId::Enforcement]);
    assert!(
        enforcement.contains(&format!("### {TITLE}")),
        "enforcement.md holds the canonical statement"
    );

    for section in [SectionId::Core, SectionId::NonOverridableRules] {
        let body = package.authored_run(&[section]);
        assert!(
            body.contains(TITLE),
            "{section:?} must point at the canonical budget by title"
        );
        assert!(
            !body.contains("One direct action = one PM-executed step"),
            "{section:?} must point at the budget, not restate its mechanics"
        );
    }

    // #4969: `identity` no longer references the budget AT ALL. It used to restate the
    // whole PM identity — orchestrator role, delegation default, budget — that
    // `core` already states, so the two sections said the same thing twice in
    // one prompt. `identity` now defers to core's `## Identity` and keeps only
    // the tm-session context that is genuinely its own.
    //
    // The anti-rot guarantee this test exists for still holds, via a two-hop
    // chain that is asserted end to end: identity -> core's `## Identity` ->
    // the budget title -> enforcement's canonical statement.
    let identity = package.authored_run(&[SectionId::Identity]);
    assert!(
        !identity.contains(TITLE) && !identity.contains("One direct action = one PM-executed step"),
        "identity must not restate the budget — core states it once"
    );
    assert!(
        identity.contains(r#"CORE section's "Identity""#),
        "identity must point at the section that does state it, by exact title"
    );
    assert!(
        package
            .authored_run(&[SectionId::Core])
            .contains("## Identity"),
        "core must carry the `## Identity` heading identity points at, or that \
         pointer is dangling"
    );
}

#[test]
fn every_skill_pointer_names_the_call_rather_than_decorating() {
    // #4969: `[SKILL: name]` is DOCUMENTATION STYLE, not a tool call. Nothing about the
    // bracket notation makes the model invoke `Skill(skill="name")`, so a
    // pointer written that way reads as a label and the content it points at
    // gets re-inlined beside it by the next editor. That is exactly what had
    // happened to the QA gate: a `[SKILL: tm-verification-protocols]` line sat
    // directly above a full inlined copy of the skill's evidence table, routing
    // table and forbidden-phrase list, with a second copy in `workflow`.
    //
    // Every pointer must therefore read as an instruction naming the call.
    let package = bundled_fallback_package().expect("manifest parses");
    let prompt = package.authored_run(&SectionId::CANONICAL);

    assert!(
        !prompt.contains("[SKILL:"),
        "bare `[SKILL: …]` notation is decoration, not an instruction — write \
         `call Skill(skill=\"name\")` so the pointer states the action"
    );

    // Every skill the prompt defers to must be named in a real call. Guards the
    // inverse failure: deleting the inlined content AND the pointer with it.
    for skill in [
        "tm-verification-protocols",
        "tm-git-file-tracking",
        "tm-pr-workflow",
        "tm-session-management",
        "tm-circuit-breaker",
        "tm-delegation-patterns",
    ] {
        assert!(
            prompt.contains(&format!(r#"Skill(skill="{skill}")"#)),
            "{skill} is deferred to but never invoked — the prompt must name \
             the call, or the content is simply gone"
        );
    }
}

#[test]
fn the_qa_evidence_contract_is_stated_once_in_the_skill() {
    // #4969: it was stated three times. `core` inlined the evidence table, the QA
    // routing table and the forbidden phrases beside a pointer to the skill
    // that already held all three, and `workflow` restated nearly all of it
    // again with no pointer at all.
    let package = bundled_fallback_package().expect("manifest parses");

    for (section, id) in [("core", SectionId::Core), ("workflow", SectionId::Workflow)] {
        let body = package.authored_run(&[id]);
        assert!(
            !body.contains("Forbidden Phrases") && !body.contains("| Required Evidence |"),
            "{section} must not restate the evidence table or forbidden phrases \
             — they are needed at completion time, not on every prompt"
        );
    }

    // The trigger stays resident: the PM has to know a completion claim is
    // gated before it can know to load anything.
    let core = package.authored_run(&[SectionId::Core]);
    assert!(
        core.contains("## QA Verification Gate (BLOCKING unless phase 4 is skipped)")
            && core.contains(r#"Skill(skill="tm-verification-protocols")"#),
        "core keeps the gate trigger and names the call that carries the detail"
    );
}

#[test]
fn the_prose_rules_ban_categories_not_phrase_lists() {
    // Both new prose rules were added because a literal example list failed to
    // generalize. Each must state the ban as a category or template AND mark
    // its examples non-exhaustive; asserting only the examples would rebuild
    // the exact failure they were written for.
    let core = bundled_fallback_package()
        .expect("manifest parses")
        .authored_run(&[SectionId::Core]);

    assert!(core.contains("**No praise for the user.**"));
    assert!(
        core.contains("bans the CATEGORY"),
        "the sycophancy rule must ban the category, not four strings"
    );
    assert!(core.contains("Non-exhaustive examples:"));

    assert!(core.contains("**Delete the framing opener; lead with the fact.**"));
    assert!(
        core.contains("`One <noun> that <its significance, or your relation to it>:`"),
        "the framing-opener rule must state the TEMPLATE"
    );
    assert!(
        core.contains("the rule is the template\nabove, never this list"),
        "the observed instances must be marked as illustration only"
    );
}
