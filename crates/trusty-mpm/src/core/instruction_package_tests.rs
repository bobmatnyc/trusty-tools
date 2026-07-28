//! Tests for the instruction-package schema (#4184).
//!
//! Split out of `instruction_package.rs` so the production file stays under the
//! 500-SLOC cap enforced by `scripts/check_line_cap.sh`.

use super::*;
use crate::core::instruction_pipeline::SECTION_SEPARATOR;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Build the canonical eight-section taxonomy with correct tiers.
fn sections() -> Vec<InstructionSection> {
    SectionId::CANONICAL
        .iter()
        .map(|id| InstructionSection {
            id: *id,
            title: format!("{id:?}"),
            customization_tier: if id.is_floor() {
                CustomizationTier::Fixed
            } else {
                CustomizationTier::Project
            },
            description: None,
        })
        .collect()
}

/// A text block owned by `section`.
fn text(section: SectionId, body: &str) -> InstructionBlock {
    InstructionBlock {
        section,
        body: BlockBody::Text {
            text: body.to_string(),
        },
        join_before: Join::Rule,
        optional: false,
    }
}

/// A generated block owned by `section`.
fn generated(section: SectionId, generator: Generator, optional: bool) -> InstructionBlock {
    InstructionBlock {
        section,
        body: BlockBody::Generated { generator },
        join_before: Join::Rule,
        optional,
    }
}

/// A minimal package that passes every structural check.
///
/// Block order deliberately interleaves `core` with `memory`/`search` and puts
/// the floor last, mirroring the shape a faithful lift of today's assets needs.
fn fixture() -> InstructionPackage {
    InstructionPackage {
        schema_version: SCHEMA_VERSION,
        package_id: "trusty-mpm/test".to_string(),
        description: None,
        trailing_newline: false,
        sections: sections(),
        blocks: vec![
            text(SectionId::Core, "CORE-A"),
            text(SectionId::Memory, "MEMORY"),
            text(SectionId::Core, "CORE-B"),
            text(SectionId::Search, "SEARCH"),
            generated(SectionId::Core, Generator::StackProfile, true),
            text(SectionId::Workflow, "WORKFLOW"),
            generated(SectionId::AgentDelegation, Generator::AgentRoster, false),
            generated(SectionId::Core, Generator::ProjectAddendum, true),
            text(SectionId::Identity, "IDENTITY"),
            text(SectionId::NonOverridableRules, "RULES"),
            text(SectionId::FrameworkGuaranteedConventions, "CONVENTIONS"),
        ],
    }
}

/// Composition inputs with every generator supplied.
fn inputs() -> CompositionInputs {
    CompositionInputs {
        agent_roster: "ROSTER".to_string(),
        stack_profile: Some("STACK".to_string()),
        project_addendum: Some("ADDENDUM".to_string()),
    }
}

/// The schema's own `examples[0]`, as raw JSON.
fn schema_example() -> String {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA_JSON).expect("schema is JSON");
    schema["examples"][0].to_string()
}

// ---------------------------------------------------------------------------
// Schema document <-> Rust type parity
// ---------------------------------------------------------------------------

/// Serialize an enum value to its schema string form.
fn wire(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .expect("serializes")
        .as_str()
        .expect("enum serializes to a string")
        .to_string()
}

/// Read a `$defs/<name>/enum` list out of the schema document.
fn schema_enum(name: &str) -> Vec<String> {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA_JSON).expect("schema is JSON");
    schema["$defs"][name]["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("$defs/{name}/enum is an array"))
        .iter()
        .map(|v| v.as_str().expect("enum member is a string").to_string())
        .collect()
}

#[test]
fn schema_document_is_valid_json_and_versioned() {
    // Why: the schema is the artifact external tooling consumes; a malformed or
    // mis-versioned document is a silent interop break.
    let schema: serde_json::Value = serde_json::from_str(SCHEMA_JSON).expect("schema is JSON");
    assert_eq!(
        schema["$schema"], "https://json-schema.org/draft/2020-12/schema",
        "schema must declare draft 2020-12"
    );
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        serde_json::json!(SCHEMA_VERSION),
        "schema's pinned version must match the Rust constant"
    );
}

#[test]
fn schema_enums_match_rust_enums() {
    // Why: the JSON Schema and the Rust types are two spellings of one
    // taxonomy. If they drift, a package validates in an editor and fails in
    // trusty-mpm (or worse, the reverse). This is the drift gate.
    let ids: Vec<String> = SectionId::CANONICAL.iter().map(wire).collect();
    assert_eq!(
        schema_enum("section-id"),
        ids,
        "schema section-id enum must match SectionId::CANONICAL, in order"
    );

    let tiers: Vec<String> = [
        CustomizationTier::Fixed,
        CustomizationTier::Project,
        CustomizationTier::User,
    ]
    .iter()
    .map(wire)
    .collect();
    assert_eq!(schema_enum("customization-tier"), tiers);

    let generators: Vec<String> = [
        Generator::AgentRoster,
        Generator::StackProfile,
        Generator::ProjectAddendum,
    ]
    .iter()
    .map(wire)
    .collect();
    assert_eq!(schema_enum("generator"), generators);
}

#[test]
fn schema_example_deserializes_validates_and_round_trips() {
    // Why (acceptance criterion of #4184): the Rust types must deserialize the
    // committed schema, and the round trip must be lossless — tooling that
    // rewrites a package must not churn unrelated bytes.
    let raw = schema_example();
    let pkg = InstructionPackage::from_json(&raw).expect("schema example deserializes");
    pkg.validate()
        .expect("schema example is structurally valid");

    let reserialized = pkg.to_json().expect("serializes");
    let again = InstructionPackage::from_json(&reserialized).expect("re-deserializes");
    assert_eq!(pkg, again, "round trip must be lossless");
    assert_eq!(
        reserialized,
        again.to_json().expect("serializes"),
        "serialization must be a fixed point"
    );
}

#[test]
fn rejects_unknown_fields() {
    // Why: a typo'd key must be a loud parse error. Silently ignoring it is how
    // a whole instruction block goes missing without anyone noticing (#4196's
    // failure shape).
    let mut value: serde_json::Value =
        serde_json::from_str(&schema_example()).expect("example is JSON");
    value["blocks"][0]["joinbefore"] = serde_json::json!("blank");
    let err = InstructionPackage::from_json(&value.to_string()).unwrap_err();
    assert!(
        err.to_string().contains("joinbefore"),
        "unknown field must be named in the error: {err}"
    );
}

// ---------------------------------------------------------------------------
// Taxonomy + tier axis
// ---------------------------------------------------------------------------

#[test]
fn canonical_order_is_sorted_and_complete() {
    // Why: `validate` compares declaration order against `CANONICAL` using
    // equality, and the enum's `Ord` is the documented tier-independent
    // ordering; both rely on CANONICAL being the enum's own order with no gaps
    // or repeats.
    let mut sorted = SectionId::CANONICAL;
    sorted.sort();
    assert_eq!(sorted, SectionId::CANONICAL, "CANONICAL must be sorted");
    let unique: std::collections::BTreeSet<_> = SectionId::CANONICAL.iter().collect();
    assert_eq!(unique.len(), 8, "eight distinct sections");
}

#[test]
fn floor_membership_is_exactly_the_absorbed_base_pm_blocks() {
    // Why: #4183 absorbs three BASE_PM blocks as the floor. Anything else being
    // floor would over-freeze content; anything less would let the floor be
    // overridden.
    let floor: Vec<SectionId> = SectionId::CANONICAL
        .into_iter()
        .filter(|s| s.is_floor())
        .collect();
    assert_eq!(
        floor,
        vec![
            SectionId::Identity,
            SectionId::NonOverridableRules,
            SectionId::FrameworkGuaranteedConventions,
        ]
    );
}

#[test]
fn tier_ordering_is_by_permissiveness() {
    // SPEC-PMINSTR-01 §4b: fixed < project < user.
    assert!(CustomizationTier::Fixed < CustomizationTier::Project);
    assert!(CustomizationTier::Project < CustomizationTier::User);
}

#[test]
fn tier_permits_matrix() {
    // Why: `project` deliberately excludes user-tier override so a personal
    // ~/.trusty-mpm config can never leak into a shared project.
    use CustomizationTier::*;
    use OverrideTier as O;
    assert!(!Fixed.permits(O::Project));
    assert!(!Fixed.permits(O::User));
    assert!(Project.permits(O::Project));
    assert!(!Project.permits(O::User));
    assert!(User.permits(O::Project));
    assert!(User.permits(O::User));
}

#[test]
fn section_lookup_reports_declared_tier() {
    let pkg = fixture();
    assert_eq!(
        pkg.section(SectionId::Identity)
            .map(|s| s.customization_tier),
        Some(CustomizationTier::Fixed)
    );
    assert_eq!(
        pkg.section(SectionId::Core).map(|s| s.customization_tier),
        Some(CustomizationTier::Project)
    );
}

// ---------------------------------------------------------------------------
// Join semantics
// ---------------------------------------------------------------------------

#[test]
fn join_literals_are_exact() {
    // Why: byte-identity is only provable if joins are literal, not inferred.
    // `Rule` must be the same separator the legacy pipeline uses, or the
    // re-sourced build (#4186) can never match today's bytes.
    assert_eq!(Join::Rule.as_str(), "\n\n---\n\n");
    assert_eq!(Join::Rule.as_str(), SECTION_SEPARATOR);
    assert_eq!(Join::Blank.as_str(), "\n\n");
    assert_eq!(Join::None.as_str(), "");
    assert_eq!(Join::Literal("\n\n\n".to_string()).as_str(), "\n\n\n");
    assert_eq!(Join::default(), Join::Rule);
}

#[test]
fn join_serializes_as_string_or_literal_object() {
    assert_eq!(serde_json::to_string(&Join::Rule).unwrap(), "\"rule\"");
    assert_eq!(serde_json::to_string(&Join::Blank).unwrap(), "\"blank\"");
    assert_eq!(serde_json::to_string(&Join::None).unwrap(), "\"none\"");
    assert_eq!(
        serde_json::to_string(&Join::Literal("x".to_string())).unwrap(),
        "{\"literal\":\"x\"}"
    );
}

// ---------------------------------------------------------------------------
// Validation — one test per defect class
// ---------------------------------------------------------------------------

#[test]
fn fixture_is_valid() {
    fixture().validate().expect("fixture must be valid");
}

#[test]
fn rejects_unsupported_schema_version() {
    let mut pkg = fixture();
    pkg.schema_version = SCHEMA_VERSION + 1;
    assert_eq!(
        pkg.validate().unwrap_err(),
        ValidationError::UnsupportedSchemaVersion {
            found: SCHEMA_VERSION + 1
        }
    );
}

#[test]
fn rejects_empty_package_id() {
    let mut pkg = fixture();
    pkg.package_id = "   ".to_string();
    assert_eq!(pkg.validate().unwrap_err(), ValidationError::EmptyPackageId);
}

#[test]
fn rejects_reordered_sections() {
    // Why: the manifest must read the same way in every project. Declaration
    // order has no effect on output, so freezing it costs nothing.
    let mut pkg = fixture();
    pkg.sections.swap(1, 2);
    assert!(matches!(
        pkg.validate().unwrap_err(),
        ValidationError::SectionsNotCanonical { .. }
    ));
}

#[test]
fn rejects_missing_section_declaration() {
    let mut pkg = fixture();
    pkg.sections.retain(|s| s.id != SectionId::Search);
    assert!(matches!(
        pkg.validate().unwrap_err(),
        ValidationError::SectionsNotCanonical { .. }
    ));
}

#[test]
fn rejects_floor_section_that_is_not_fixed() {
    // Why (#4194): the floor's non-overridable semantics must be representable
    // AND enforced, not merely conventional.
    let mut pkg = fixture();
    for section in &mut pkg.sections {
        if section.id == SectionId::NonOverridableRules {
            section.customization_tier = CustomizationTier::Project;
        }
    }
    assert_eq!(
        pkg.validate().unwrap_err(),
        ValidationError::FloorNotFixed {
            section: SectionId::NonOverridableRules,
            tier: CustomizationTier::Project,
        }
    );
}

#[test]
fn rejects_empty_block_stream() {
    let mut pkg = fixture();
    pkg.blocks.clear();
    assert_eq!(pkg.validate().unwrap_err(), ValidationError::NoBlocks);
}

#[test]
fn rejects_blank_text_block() {
    let mut pkg = fixture();
    pkg.blocks[0] = text(SectionId::Core, "   \n\t ");
    assert_eq!(
        pkg.validate().unwrap_err(),
        ValidationError::EmptyText {
            index: 0,
            section: SectionId::Core,
        }
    );
}

#[test]
fn rejects_optional_text_block() {
    // Why: `optional` exists only so a generator with nothing to supply can be
    // dropped deliberately. An optional *text* block would be authored content
    // that vanishes for no reason.
    let mut pkg = fixture();
    pkg.blocks[0].optional = true;
    assert_eq!(
        pkg.validate().unwrap_err(),
        ValidationError::OptionalTextBlock {
            index: 0,
            section: SectionId::Core,
        }
    );
}

#[test]
fn rejects_section_that_contributes_nothing() {
    // Why: a declared-but-silent section is exactly the #4196 shape — the
    // taxonomy claims the content is delivered, the output does not contain it.
    let mut pkg = fixture();
    pkg.blocks.retain(|b| b.section != SectionId::Search);
    assert_eq!(
        pkg.validate().unwrap_err(),
        ValidationError::SectionWithoutBlocks {
            section: SectionId::Search
        }
    );
}

#[test]
fn rejects_package_that_never_consumes_the_roster() {
    // Why (#4196): the delivered prompt named 8 agents because the computed
    // 42-agent roster was dropped at the final composition step. A package that
    // does not consume the roster is not a valid package.
    let mut pkg = fixture();
    for block in &mut pkg.blocks {
        if matches!(
            block.body,
            BlockBody::Generated {
                generator: Generator::AgentRoster
            }
        ) {
            block.body = BlockBody::Text {
                text: "STATIC ROSTER".to_string(),
            };
        }
    }
    assert_eq!(
        pkg.validate().unwrap_err(),
        ValidationError::RosterNotConsumed
    );
}

#[test]
fn rejects_optional_roster_block() {
    // Why (#4196): the roster must never be droppable, even deliberately.
    let mut pkg = fixture();
    let index = pkg
        .blocks
        .iter()
        .position(|b| {
            matches!(
                b.body,
                BlockBody::Generated {
                    generator: Generator::AgentRoster
                }
            )
        })
        .expect("roster block");
    pkg.blocks[index].optional = true;
    assert_eq!(
        pkg.validate().unwrap_err(),
        ValidationError::OptionalRoster { index }
    );
}

#[test]
fn rejects_overridable_block_after_the_floor() {
    // Why: the floor's guarantee is that it has the last word. Enforcing this
    // per-block preserves it without dictating one global order — which the
    // byte-identical lift in #4186 needs to stay free to choose.
    let mut pkg = fixture();
    pkg.blocks.push(text(SectionId::Core, "SNEAKY"));
    let err = pkg.validate().unwrap_err();
    assert!(
        matches!(
            err,
            ValidationError::OverridableAfterFloor {
                section: SectionId::Core,
                ..
            }
        ),
        "got {err}"
    );
}

#[test]
fn floor_blocks_may_follow_each_other() {
    // The floor may itself be several blocks; only non-floor content is barred.
    let mut pkg = fixture();
    pkg.blocks
        .push(text(SectionId::FrameworkGuaranteedConventions, "MORE"));
    pkg.validate().expect("floor-after-floor is fine");
}

// ---------------------------------------------------------------------------
// Composition — ordering, joins, determinism
// ---------------------------------------------------------------------------

#[test]
fn composes_blocks_in_array_order_with_declared_joins() {
    // Why: output order is block-array order and nothing else — no map
    // iteration can reach it. Interleaving `core` with `memory`/`search` proves
    // a section can contribute at several non-adjacent positions, which is what
    // makes a byte-identical lift out of the middle of a legacy asset possible.
    let mut pkg = fixture();
    pkg.blocks[1].join_before = Join::Blank;
    pkg.blocks[2].join_before = Join::Blank;
    let out = pkg.compose(&inputs()).expect("composes");
    assert_eq!(
        out,
        [
            "CORE-A\n\nMEMORY\n\nCORE-B",
            "SEARCH",
            "STACK",
            "WORKFLOW",
            "ROSTER",
            "ADDENDUM",
            "IDENTITY",
            "RULES",
            "CONVENTIONS",
        ]
        .join(SECTION_SEPARATOR)
    );
}

#[test]
fn first_emitted_block_never_contributes_a_join() {
    let pkg = fixture();
    let out = pkg.compose(&inputs()).expect("composes");
    assert!(out.starts_with("CORE-A"), "no leading separator: {out:?}");
    assert!(out.ends_with("CONVENTIONS"), "no trailing newline: {out:?}");
}

#[test]
fn optional_generated_block_is_dropped_with_its_join() {
    // Why: an optional generator with nothing to supply must leave no dangling
    // separator behind.
    let pkg = fixture();
    let out = pkg
        .compose(&CompositionInputs {
            agent_roster: "ROSTER".to_string(),
            stack_profile: None,
            project_addendum: None,
        })
        .expect("composes");
    assert!(!out.contains("STACK"));
    assert!(!out.contains("ADDENDUM"));
    assert!(
        !out.contains("---\n\n---"),
        "no dangling separator: {out:?}"
    );
    assert!(out.contains(&format!("WORKFLOW{SECTION_SEPARATOR}ROSTER")));
}

#[test]
fn whitespace_only_generator_output_counts_as_absent() {
    let pkg = fixture();
    let out = pkg
        .compose(&CompositionInputs {
            agent_roster: "ROSTER".to_string(),
            stack_profile: Some("   \n\n".to_string()),
            project_addendum: None,
        })
        .expect("composes");
    assert!(!out.contains("   \n\n"), "blank stack profile is dropped");
}

#[test]
fn generated_bodies_are_trimmed() {
    let pkg = fixture();
    let out = pkg
        .compose(&CompositionInputs {
            agent_roster: "\n\n  ROSTER  \n\n".to_string(),
            stack_profile: None,
            project_addendum: None,
        })
        .expect("composes");
    assert!(out.contains(&format!("WORKFLOW{SECTION_SEPARATOR}ROSTER")));
}

#[test]
fn missing_required_generator_input_is_a_hard_error() {
    // Why (#4196): a required generator that supplies nothing must fail the
    // composition loudly. Under the old concatenation it produced a prompt that
    // looked fine and named 8 of 42 agents.
    let pkg = fixture();
    let index = pkg
        .blocks
        .iter()
        .position(|b| {
            matches!(
                b.body,
                BlockBody::Generated {
                    generator: Generator::AgentRoster
                }
            )
        })
        .expect("roster block");
    let err = pkg
        .compose(&CompositionInputs {
            agent_roster: String::new(),
            ..inputs()
        })
        .unwrap_err();
    assert_eq!(
        err,
        CompositionError::MissingGeneratedInput {
            index,
            generator: Generator::AgentRoster,
        }
    );
}

#[test]
fn compose_rejects_an_invalid_package() {
    let mut pkg = fixture();
    pkg.schema_version = 99;
    assert!(matches!(
        pkg.compose(&inputs()).unwrap_err(),
        CompositionError::Invalid(ValidationError::UnsupportedSchemaVersion { found: 99 })
    ));
}

#[test]
fn compose_is_byte_identical_across_repeats_and_a_json_round_trip() {
    // Why: this is the property #4186's acceptance test depends on — the same
    // package and the same roster must produce the same bytes, including after
    // the package has been serialized and read back.
    let pkg = fixture();
    let inputs = inputs();
    let first = pkg.compose(&inputs).expect("composes");
    let second = pkg.compose(&inputs).expect("composes");
    assert_eq!(first, second, "compose must be a pure function");

    let round_tripped =
        InstructionPackage::from_json(&pkg.to_json().expect("serializes")).expect("deserializes");
    assert_eq!(
        first,
        round_tripped.compose(&inputs).expect("composes"),
        "a JSON round trip must not change a single byte of the output"
    );
}

#[test]
fn trailing_newline_appends_exactly_one_byte() {
    // Why: the tail byte is the difference between today's `resolve_pm_prompt`
    // output (none) and a file-shaped artifact (one). Both must be expressible.
    let mut pkg = fixture();
    let without = pkg.compose(&inputs()).expect("composes");
    pkg.trailing_newline = true;
    let with = pkg.compose(&inputs()).expect("composes");
    assert_eq!(with, format!("{without}\n"));
}

#[test]
fn schema_example_composes_deterministically() {
    // The committed example is not just parseable — it composes.
    let pkg = InstructionPackage::from_json(&schema_example()).expect("deserializes");
    let out = pkg.compose(&inputs()).expect("composes");
    assert!(out.contains("ROSTER"), "roster reaches the output: {out}");
    assert_eq!(out, pkg.compose(&inputs()).expect("composes"));
}
