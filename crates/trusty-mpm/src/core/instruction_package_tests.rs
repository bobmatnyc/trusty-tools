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

/// Index of the first block in the schema example whose body is `generated`.
fn generated_block_index(value: &serde_json::Value) -> usize {
    value["blocks"]
        .as_array()
        .expect("blocks is an array")
        .iter()
        .position(|b| b["body"]["kind"] == "generated")
        .expect("the example has a generated block")
}

#[test]
fn rejects_unknown_fields_at_every_level_and_names_the_key() {
    // Why: a typo'd key must be a loud parse error naming the offender. A
    // silently ignored key is how a whole instruction block changes meaning
    // without anyone noticing (#4196's failure shape: report success, compose
    // the wrong bytes) — and an error that merely says "invalid" leaves the
    // typo almost as hard to find as a silent drop.
    //
    // This covers EVERY object in the deserialization path, not just the one
    // struct that regressed. `BlockBody` (the `body` cases) is the #4223 HIGH:
    // before `deny_unknown_fields` was added to it, a key misplaced inside
    // `body` deserialized to `Ok(..)` with the key discarded, and the block
    // silently fell back to the `Join::Rule` default.
    let example: serde_json::Value =
        serde_json::from_str(&schema_example()).expect("example is JSON");
    let gen_index = generated_block_index(&example);

    // (label, JSON pointer to the object that should reject, injected key)
    let cases: [(&str, String, &str); 5] = [
        ("package root", String::new(), "bogus_root_key"),
        ("InstructionSection", "/sections/0".to_string(), "tier"),
        ("InstructionBlock", "/blocks/0".to_string(), "joinbefore"),
        (
            "BlockBody::Text",
            "/blocks/0/body".to_string(),
            "join_before",
        ),
        (
            "BlockBody::Generated",
            format!("/blocks/{gen_index}/body"),
            "optional",
        ),
    ];

    for (label, pointer, key) in cases {
        let mut value = example.clone();
        let target = if pointer.is_empty() {
            &mut value
        } else {
            value
                .pointer_mut(&pointer)
                .unwrap_or_else(|| panic!("{label}: pointer {pointer} resolves"))
        };
        target[key] = serde_json::json!("x");

        let err = match InstructionPackage::from_json(&value.to_string()) {
            Err(err) => err,
            Ok(_) => panic!(
                "{label}: unknown key `{key}` was SILENTLY ACCEPTED at {pointer:?}; \
                 it must be a parse error"
            ),
        };
        assert!(
            err.to_string().contains(key),
            "{label}: the error must NAME the offending key `{key}` so a typo is \
             findable, got: {err}"
        );
    }
}

#[test]
fn schema_document_closes_every_object() {
    // Why: the Rust loader and the shipped JSON Schema are two spellings of one
    // format. `rejects_unknown_fields_at_every_level_and_names_the_key` pins the
    // Rust half; this pins the schema half, so an object added to the schema
    // without `additionalProperties: false` cannot reintroduce the drift where a
    // package validates strictly in trusty-mpm but loosely in an editor (or the
    // reverse — which is how the #4223 HIGH shipped).
    let schema: serde_json::Value = serde_json::from_str(SCHEMA_JSON).expect("schema is JSON");

    /// Walk every subschema, collecting object-typed ones that stay open.
    fn walk(node: &serde_json::Value, path: String, open: &mut Vec<String>) {
        if let Some(map) = node.as_object() {
            if map.get("type") == Some(&serde_json::json!("object"))
                && map.get("additionalProperties") != Some(&serde_json::json!(false))
            {
                open.push(path.clone());
            }
            for (key, child) in map {
                // `examples` holds package instances, not subschemas.
                if path.is_empty() && key == "examples" {
                    continue;
                }
                walk(child, format!("{path}/{key}"), open);
            }
        } else if let Some(items) = node.as_array() {
            for (i, child) in items.iter().enumerate() {
                walk(child, format!("{path}/{i}"), open);
            }
        }
    }

    let mut open = Vec::new();
    walk(&schema, String::new(), &mut open);
    assert!(
        open.is_empty(),
        "every object in the schema must set `additionalProperties: false`; open: {open:?}"
    );
}

#[test]
fn join_rejects_an_unknown_key_alongside_literal() {
    // Why: `Join` is the one type in the graph that is NOT a `deny_unknown_fields`
    // struct — it is an externally tagged enum, so the attribute does not apply.
    // It is nonetheless closed: serde_json requires the variant object to hold
    // exactly one key. Pinned here because that is a property of serde's
    // externally-tagged representation rather than of anything written in this
    // module, so nothing else would catch it regressing.
    let err = serde_json::from_str::<Join>(r#"{"literal":"x","bogus":1}"#)
        .expect_err("an extra key alongside `literal` must be rejected");

    // DEFERRED LOW (#4223 re-review): unlike the struct errors, this one does
    // not NAME the offending key — serde's externally tagged representation
    // reports "expected value" plus a position. Deferred rather than fixed: it
    // is a REJECTION, not a silent drop, so the failure mode this PR exists to
    // close is already shut; naming the key would need a hand-written
    // Deserialize for `Join` purely to improve a message on the rarest variant
    // (`Literal` is an escape hatch). Pin the POSITION instead, so the
    // diagnostic can never degrade to something with no locator at all.
    let msg = err.to_string();
    assert!(
        msg.contains("line") && msg.contains("column"),
        "the error must at least locate the offending key: {msg}"
    );

    // An unrecognised variant is named, in both spellings.
    let err = serde_json::from_str::<Join>(r#""rulez""#).unwrap_err();
    assert!(err.to_string().contains("rulez"), "got {err}");
    let err = serde_json::from_str::<Join>(r#"{"litteral":"x"}"#).unwrap_err();
    assert!(err.to_string().contains("litteral"), "got {err}");
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
fn rejects_section_covered_only_by_optional_blocks() {
    // Why (#4223 re-review MEDIUM): owning blocks is not the same as emitting.
    // A section whose every block is `optional` passes the ownership check and
    // still composes to nothing — the taxonomy claims the content is delivered
    // and the output does not contain it, which is exactly the silent-missing-
    // section shape the ownership check exists to prevent.
    //
    // The complement — a section with BOTH an optional and a non-optional block
    // — must stay legal, and does: `fixture()`'s `core` owns two text blocks
    // plus two optional generated ones, and `fixture_is_valid` covers it.
    let mut pkg = fixture();
    pkg.blocks.retain(|b| b.section != SectionId::Search);
    pkg.blocks.insert(
        3,
        generated(SectionId::Search, Generator::ProjectAddendum, true),
    );
    assert_eq!(
        pkg.validate().unwrap_err(),
        ValidationError::SectionOnlyOptionalBlocks {
            section: SectionId::Search
        }
    );
}

#[test]
fn later_schema_version_is_reported_as_a_version_error_not_a_field_error() {
    // Why (#4223 re-review LOW): the module promises that a package from a later
    // schema is rejected with a loud "unsupported schema_version". That promise
    // was not kept for the case that actually matters — a v2 package almost
    // certainly carries a v2 field, and plain deserialization blamed THAT key
    // ("unknown field `...`"), pointing the author at something perfectly valid
    // in its own schema instead of telling them to upgrade trusty-mpm.
    let mut value: serde_json::Value =
        serde_json::from_str(&schema_example()).expect("example is JSON");
    value["schema_version"] = serde_json::json!(SCHEMA_VERSION + 1);
    value["blocks"][0]["field_added_in_v2"] = serde_json::json!("x");

    let err = InstructionPackage::from_json(&value.to_string()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("schema_version"),
        "the VERSION must be blamed, got: {msg}"
    );
    assert!(
        msg.contains(&(SCHEMA_VERSION + 1).to_string()),
        "the offending version must be named, got: {msg}"
    );
    assert!(
        !msg.contains("field_added_in_v2"),
        "must NOT blame a key that is valid in its own schema, got: {msg}"
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
fn valid_package_with_non_contiguous_blocks_still_round_trips() {
    // Why: the counterweight to the strictness tests. Closing unknown fields
    // must not degenerate into "reject everything" — in particular it must not
    // disturb the property this schema exists for: ONE SECTION OWNING SEVERAL
    // NON-ADJACENT BLOCK POSITIONS, which is what lets `memory`/`search` be
    // lifted out of the middle of PM_INSTRUCTIONS.md without moving a byte.
    let raw = schema_example();
    let pkg = InstructionPackage::from_json(&raw).expect("valid package must still deserialize");
    pkg.validate().expect("valid package must still validate");

    // The example genuinely exercises non-contiguity, so this test cannot pass
    // vacuously on a package whose sections happen to be contiguous.
    let core_positions: Vec<usize> = pkg
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.section == SectionId::Core)
        .map(|(i, _)| i)
        .collect();
    assert!(
        core_positions.len() >= 2,
        "fixture must own several blocks: {core_positions:?}"
    );
    assert!(
        core_positions.windows(2).any(|w| w[1] != w[0] + 1),
        "`core` must own NON-ADJACENT positions or this test proves nothing: {core_positions:?}"
    );

    // Round trip is lossless and byte-stable through compose.
    let composed = pkg.compose(&inputs()).expect("composes");
    let again = InstructionPackage::from_json(&pkg.to_json().expect("serializes"))
        .expect("re-deserializes");
    assert_eq!(pkg, again, "round trip must be lossless");
    assert_eq!(
        composed,
        again.compose(&inputs()).expect("composes"),
        "a JSON round trip must not change a single byte"
    );
}

#[test]
fn todays_delegation_section_shape_is_expressible() {
    // Why: this pins the CORRECTION of a wrong conclusion an earlier revision of
    // this PR drew and propagated to issue #4186 — that #4186 must choose
    // between a byte-identical lift and consuming the computed roster.
    //
    // It does not have to choose. Since #4196 (closing #4069),
    // `instruction_overrides::bundled_delegation` builds the section as
    //
    //     format!("{bundled}\n\n{ROSTER_PRECEDENCE_NOTE}\n\n{}", roster.trim())
    //
    // — bundled asset, blank line, precedence note, blank line, LIVE roster. So
    // the delivered prompt ALREADY carries the real roster, and this schema
    // expresses that exact shape with three blocks joined by `blank`. Byte
    // identity and roster consumption are therefore compatible, and #4186 should
    // keep strict byte-equality against today's output as its acceptance gate.
    //
    // (Stand-in strings, not the real assets: `ROSTER_PRECEDENCE_NOTE` is
    // private to `instruction_overrides`. What is pinned here is the SHAPE
    // `A\n\nB\n\n<generated roster>`, which is the load-bearing claim.)
    let mut pkg = fixture();
    let at = pkg
        .blocks
        .iter()
        .position(|b| b.section == SectionId::AgentDelegation)
        .expect("fixture has a delegation block");

    let asset = text(SectionId::AgentDelegation, "BUNDLED-ASSET");
    let mut note = text(SectionId::AgentDelegation, "PRECEDENCE-NOTE");
    note.join_before = Join::Blank;
    let mut roster = generated(SectionId::AgentDelegation, Generator::AgentRoster, false);
    roster.join_before = Join::Blank;
    pkg.blocks.splice(at..=at, [asset, note, roster]);

    pkg.validate()
        .expect("today's delegation shape must be a valid package");
    let out = pkg.compose(&inputs()).expect("composes");
    assert!(
        out.contains("BUNDLED-ASSET\n\nPRECEDENCE-NOTE\n\nROSTER"),
        "asset/note/roster must rejoin exactly as bundled_delegation writes them: {out:?}"
    );

    // And the roster is genuinely the generated block, not authored text --
    // swapping it for static text must fail validation (#4196's whole point).
    let mut static_roster = pkg.clone();
    for block in &mut static_roster.blocks {
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
        static_roster.validate().unwrap_err(),
        ValidationError::RosterNotConsumed
    );
}

#[test]
fn base_pm_floor_block_order_is_valid() {
    // Why: this pins the REFUTATION of a proposed constraint (#4223 review,
    // MEDIUM: "require floor blocks to be in canonical section order relative to
    // each other; it costs #4186 nothing"). It would in fact cost #4186 the
    // whole lift. `assets/instructions/BASE_PM.md` runs its headings:
    //
    //   ## Identity                                     -> Identity                       (0)
    //   ## Non-Overridable Rules                        -> NonOverridableRules             (6)
    //   ## Customizing PM Behavior                      -> NonOverridableRules             (6)
    //   ## Framework-Guaranteed Conventions             -> FrameworkGuaranteedConventions  (7)
    //   ## Trusty Tool Priority (Non-Overridable)       -> NonOverridableRules             (6)
    //
    // i.e. 0, 6, 6, 7, 6 — a canonical-order inversion in the real asset. A
    // non-decreasing-floor-order rule would reject a faithful lift of BASE_PM.md
    // and force #4186 to move bytes. If someone adds that constraint later, this
    // test fails and says why.
    let mut pkg = fixture();
    pkg.blocks.retain(|b| !b.section.is_floor());
    pkg.blocks.extend([
        text(SectionId::Identity, "IDENTITY"),
        text(SectionId::NonOverridableRules, "NON-OVERRIDABLE RULES"),
        text(SectionId::NonOverridableRules, "CUSTOMIZING PM BEHAVIOR"),
        text(
            SectionId::FrameworkGuaranteedConventions,
            "FRAMEWORK-GUARANTEED CONVENTIONS",
        ),
        text(SectionId::NonOverridableRules, "TRUSTY TOOL PRIORITY"),
    ]);

    pkg.validate()
        .expect("BASE_PM.md's real floor order must remain expressible");

    let out = pkg.compose(&inputs()).expect("composes");
    let parts: Vec<&str> = out.split(SECTION_SEPARATOR).collect();
    let tail: Vec<&str> = parts[parts.len() - 5..].to_vec();
    assert_eq!(
        tail,
        vec![
            "IDENTITY",
            "NON-OVERRIDABLE RULES",
            "CUSTOMIZING PM BEHAVIOR",
            "FRAMEWORK-GUARANTEED CONVENTIONS",
            "TRUSTY TOOL PRIORITY",
        ],
        "floor blocks compose in declared array order, inversion included"
    );
}

#[test]
fn schema_example_composes_deterministically() {
    // The committed example is not just parseable — it composes.
    let pkg = InstructionPackage::from_json(&schema_example()).expect("deserializes");
    let out = pkg.compose(&inputs()).expect("composes");
    assert!(out.contains("ROSTER"), "roster reaches the output: {out}");
    assert_eq!(out, pkg.compose(&inputs()).expect("composes"));
}
