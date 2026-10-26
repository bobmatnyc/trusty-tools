//! Tests for the board spec and the `project:` block (#10275, AC1–AC5).
//!
//! Why: each acceptance criterion of #10275 names a refusal; each test here
//! builds the offending spec or block and asserts the refusal and the name it
//! carries.
//! What: mutated specs are re-serialized and re-loaded through
//! [`load_spec`], the same path the embedded spec takes; `project:` blocks go
//! through the real `issue-state.yaml` loader.
//! Test: this IS the test module (classified test file: 3000-SLOC cap).

use super::project::{ProjectBlock, validate_project};
use super::spec::{
    BOARD_SPEC_YAML, BoardSpec, FieldKind, NoLabel, OptionColor, ViewLayout, embedded_spec,
    load_spec,
};
use super::validate::BoardSpecError;
use crate::commands::issue::config::{STATUS_LIFECYCLE_YAML, StateModel, load_model_in};

/// The built-in `status:*` lifecycle the Status options map onto.
fn lifecycle() -> StateModel {
    serde_yaml::from_str(STATUS_LIFECYCLE_YAML).expect("lifecycle parses")
}

/// The embedded spec as a mutable value.
fn spec() -> BoardSpec {
    embedded_spec().expect("the embedded spec loads")
}

/// Serialize `spec` and load it again, the way the embedded spec is loaded.
fn reload(spec: &BoardSpec) -> Result<BoardSpec, BoardSpecError> {
    let yaml = serde_yaml::to_string(spec).expect("serialize");
    load_spec(&yaml, &lifecycle())
}

/// The Status option named `name`.
fn status_option<'a>(spec: &'a mut BoardSpec, name: &str) -> &'a mut super::spec::OptionSpec {
    spec.fields
        .iter_mut()
        .find(|f| f.name == "Status")
        .expect("Status field")
        .options
        .iter_mut()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("Status option {name}"))
}

/// AC1: the embedded spec parses with its version and exactly the six
/// standard fields; a non-standard field or a Status set other than the nine
/// names is refused.
#[test]
fn embedded_spec_has_the_version_and_the_six_standard_fields_10275() {
    let spec = spec();
    assert_eq!(spec.version, 1);
    let names: Vec<&str> = spec.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Status",
            "Start date",
            "Target date",
            "Deployed on",
            "Published on",
            "Hold"
        ]
    );

    // Status: Project #45's nine options, colors and descriptions, in order.
    let status: Vec<(&str, OptionColor, &str)> = spec
        .field("Status")
        .expect("Status")
        .options
        .iter()
        .map(|o| (o.name.as_str(), o.color, o.description.as_str()))
        .collect();
    use OptionColor::*;
    assert_eq!(
        status,
        [
            ("Backlog", Gray, "Issue created"),
            ("Ready for Development", Blue, "Selected for work"),
            ("In Development", Yellow, "Engineer session claimed it"),
            ("In PR", Orange, "PR open, `Refs #N`"),
            (
                "On Preview",
                Yellow,
                "Per-PR preview deploy is up and checked"
            ),
            ("Merged", Green, "PR squash-merged"),
            (
                "On Prod",
                Pink,
                "Runs in production or as the installed binary, with live evidence"
            ),
            ("Published", Purple, "Version on crates.io"),
            (
                "Won't Do",
                Gray,
                "Not planned, duplicate or superseded, with a link"
            ),
        ]
    );
    for date in ["Start date", "Target date", "Deployed on", "Published on"] {
        assert_eq!(
            spec.field(date).expect(date).kind,
            FieldKind::Date,
            "{date}"
        );
    }

    // A field outside the standard set is refused, naming it.
    for extra in ["Crate", "Agents"] {
        let mut with_extra = spec.clone();
        let mut field = with_extra.field("Start date").expect("date").clone();
        field.name = extra.to_string();
        with_extra.fields.push(field);
        assert_eq!(
            reload(&with_extra),
            Err(BoardSpecError::NonStandardField(extra.to_string()))
        );
    }

    // A Status set that is not exactly the nine names is refused: one fewer,
    // one renamed, and one reordered.
    let mut eight = spec.clone();
    eight.fields[0].options.retain(|o| o.name != "Won't Do");
    let mut renamed = spec.clone();
    status_option(&mut renamed, "Won't Do").name = "Wont Do".to_string();
    let mut reordered = spec.clone();
    reordered.fields[0].options.swap(0, 1);
    for (case, bad) in [
        ("eight", eight),
        ("renamed", renamed),
        ("reordered", reordered),
    ] {
        let err = reload(&bad).expect_err(case);
        assert!(
            matches!(err, BoardSpecError::StatusOptions { .. }),
            "{case}: {err}"
        );
    }
}

/// AC2: every Status option maps to a state or declares `label: none`; an
/// option with the mapping dropped is refused, naming the option.
#[test]
fn a_status_option_without_a_mapping_is_refused_naming_it_10275() {
    let spec = spec();
    let mapped: Vec<(&str, Option<&str>)> = spec
        .field("Status")
        .expect("Status")
        .options
        .iter()
        .map(|o| (o.name.as_str(), o.state.as_deref()))
        .collect();
    // TICKETING.md's Lifecycle table: four options carry a `status:*` label.
    assert_eq!(
        mapped,
        [
            ("Backlog", None),
            ("Ready for Development", None),
            ("In Development", Some("status:in-progress")),
            ("In PR", Some("status:coded")),
            ("On Preview", None),
            ("Merged", Some("status:merged")),
            ("On Prod", Some("status:tested")),
            ("Published", None),
            ("Won't Do", None),
        ]
    );
    for option in &spec.field("Status").expect("Status").options {
        assert_eq!(
            option.state.is_some(),
            option.label.is_none(),
            "{}",
            option.name
        );
    }

    for name in spec
        .field("Status")
        .expect("Status")
        .options
        .iter()
        .map(|o| o.name.clone())
    {
        let mut unmapped = spec.clone();
        let option = status_option(&mut unmapped, &name);
        option.state = None;
        option.label = None;
        let err = reload(&unmapped).expect_err(&name);
        assert_eq!(err, BoardSpecError::UnmappedOption(name.clone()));
        assert!(err.to_string().contains(&format!("`{name}`")), "{err}");
    }
}

/// AC3: each `status:*` state is reached by exactly one option; two options
/// mapped to one state are refused naming both.
#[test]
fn two_status_options_mapped_to_one_state_are_refused_naming_both_10275() {
    let mut twice = spec();
    status_option(&mut twice, "Merged").state = Some("status:coded".to_string());
    let err = reload(&twice).expect_err("two options on status:coded");
    assert_eq!(
        err,
        BoardSpecError::StateMappedTwice {
            state: "status:coded".to_string(),
            first: "In PR".to_string(),
            second: "Merged".to_string(),
        }
    );
    let text = err.to_string();
    assert!(
        text.contains("`In PR`") && text.contains("`Merged`"),
        "{text}"
    );

    // The other half of "exactly one": a state no option reaches.
    let mut unreached = spec();
    let option = status_option(&mut unreached, "On Prod");
    option.state = None;
    option.label = Some(NoLabel::None);
    assert_eq!(
        reload(&unreached),
        Err(BoardSpecError::StateNotMapped("status:tested".to_string()))
    );

    // A label-less state is not a mapping target.
    let mut labelless = spec();
    let option = status_option(&mut labelless, "Backlog");
    option.label = None;
    option.state = Some("open".to_string());
    assert!(matches!(
        reload(&labelless),
        Err(BoardSpecError::UnknownState { option, state, .. })
            if option == "Backlog" && state == "open"
    ));
}

/// AC4: Hold is declared off; a spec without Hold is refused and a spec with
/// Hold enabled is accepted.
#[test]
fn a_spec_without_hold_is_refused_and_hold_enabled_is_accepted_10275() {
    let spec = spec();
    let hold = spec.field("Hold").expect("Hold");
    assert!(!hold.enabled, "Hold stays off until #9401");
    assert_eq!(hold.kind, FieldKind::SingleSelect);
    let options: Vec<(&str, OptionColor, &str)> = hold
        .options
        .iter()
        .map(|o| (o.name.as_str(), o.color, o.description.as_str()))
        .collect();
    assert_eq!(
        options,
        [
            ("Blocked", OptionColor::Red, "External wait; blocker named"),
            (
                "Needs decision",
                OptionColor::Orange,
                "Waits on a named decision-maker"
            ),
        ]
    );

    let mut without = spec.clone();
    without.fields.retain(|f| f.name != "Hold");
    assert_eq!(
        reload(&without),
        Err(BoardSpecError::MissingField("Hold".to_string()))
    );

    let mut enabled = spec.clone();
    if let Some(hold) = enabled.fields.iter_mut().find(|f| f.name == "Hold") {
        hold.enabled = true;
    }
    let loaded = reload(&enabled).expect("Hold enabled is accepted");
    assert!(loaded.field("Hold").expect("Hold").enabled);
}

/// AC5: the `project:` block parses through the real loader with owner,
/// number, options to drop and extra fields; an unknown key is refused naming
/// the key.
#[test]
fn the_project_block_parses_and_refuses_an_unknown_key_10275() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("issue-state.yaml");
    let block = "\nproject:\n  owner: duettoresearch\n  number: 7\n  drop_options:\n    - \
                 Published\n    - On Preview\n  extra_fields:\n    - Area\n";
    std::fs::write(&path, format!("{STATUS_LIFECYCLE_YAML}{block}")).expect("write");
    let (model, _) = load_model_in(tmp.path(), Some(path.as_path()), None).expect("block loads");
    assert_eq!(
        model.project,
        Some(ProjectBlock {
            owner: "duettoresearch".to_string(),
            number: 7,
            drop_options: vec!["Published".to_string(), "On Preview".to_string()],
            extra_fields: vec!["Area".to_string()],
        })
    );

    let bad = "\nproject:\n  owner: duettoresearch\n  number: 7\n  colour: red\n";
    std::fs::write(&path, format!("{STATUS_LIFECYCLE_YAML}{bad}")).expect("write");
    let err = load_model_in(tmp.path(), Some(path.as_path()), None)
        .expect_err("unknown key refused")
        .to_string();
    assert!(err.contains("unknown field `colour`"), "{err}");
}

/// AC5: trusty-tools' own `issue-state.yaml` names Project #45, owner
/// `bobmatnyc`, with Crate and Agents as extras, and the block conforms to the
/// spec.
#[test]
fn the_repo_block_names_project_45_with_crate_and_agents_10275() {
    let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    let path = root.join("issue-state.yaml");
    let (model, _) = load_model_in(&root, Some(path.as_path()), None).expect("repo model loads");
    let block = model.project.expect("the repo model names its board");
    assert_eq!(
        block,
        ProjectBlock {
            owner: "bobmatnyc".to_string(),
            number: 45,
            drop_options: Vec::new(),
            extra_fields: vec!["Crate".to_string(), "Agents".to_string()],
        }
    );
    validate_project(&block, &spec()).expect("the repo block conforms");
}

/// The `project:` block is checked against the spec: unknown dropped options
/// and standard fields declared as extras are refused, naming them.
#[test]
fn the_project_block_is_checked_against_the_spec_10275() {
    let spec = spec();
    let base = ProjectBlock {
        owner: "duettoresearch".to_string(),
        number: 7,
        drop_options: vec!["Published".to_string(), "On Preview".to_string()],
        extra_fields: vec!["Crate".to_string()],
    };
    validate_project(&base, &spec).expect("a conformant block");

    let cases = [
        (
            ProjectBlock {
                owner: " ".to_string(),
                ..base.clone()
            },
            BoardSpecError::EmptyOwner,
        ),
        (
            ProjectBlock {
                number: 0,
                ..base.clone()
            },
            BoardSpecError::ZeroProjectNumber,
        ),
        (
            ProjectBlock {
                drop_options: vec!["Shipped".to_string()],
                ..base.clone()
            },
            BoardSpecError::UnknownDropOption("Shipped".to_string()),
        ),
        (
            ProjectBlock {
                extra_fields: vec!["Hold".to_string()],
                ..base.clone()
            },
            BoardSpecError::ExtraIsStandard("Hold".to_string()),
        ),
    ];
    for (block, want) in cases {
        assert_eq!(validate_project(&block, &spec), Err(want));
    }
}

/// Views, workflows and the README come from the Project #45 capture; a view
/// that shows a repo extra such as Crate is refused.
#[test]
fn a_view_naming_a_repo_extra_is_refused_10275() {
    let spec = spec();
    let views: Vec<(&str, ViewLayout, Option<&str>)> = spec
        .views
        .iter()
        .map(|v| (v.name.as_str(), v.layout, v.vertical_group_by.as_deref()))
        .collect();
    assert_eq!(
        views,
        [
            ("List", ViewLayout::Table, None),
            ("Swimlanes", ViewLayout::Board, Some("Status")),
            ("Roadmap", ViewLayout::Roadmap, None),
        ]
    );
    let list = &spec.views[0].fields;
    assert!(!list.iter().any(|f| f == "Crate"), "{list:?}");
    assert_eq!(list.len(), 11, "#45's twelve List columns less Crate");

    let on: Vec<&str> = spec
        .workflows
        .iter()
        .filter(|w| w.enabled)
        .map(|w| w.name.as_str())
        .collect();
    assert_eq!(
        on,
        ["Auto-add to project", "Auto-add sub-issues to project"]
    );
    assert_eq!(spec.workflows.len(), 7);
    assert!(spec.readme.contains("{{status_table}}"));

    let mut with_crate = spec.clone();
    with_crate.views[0].fields.push("Crate".to_string());
    assert_eq!(
        reload(&with_crate),
        Err(BoardSpecError::ViewUnknownField {
            view: "List".to_string(),
            field: "Crate".to_string(),
        })
    );

    let mut placeholder = spec.clone();
    placeholder.readme.push_str("{{owner}}\n");
    assert_eq!(
        reload(&placeholder),
        Err(BoardSpecError::UnknownPlaceholder("owner".to_string()))
    );

    let teal = BOARD_SPEC_YAML.replacen("color: GRAY", "color: TEAL", 1);
    assert!(matches!(
        load_spec(&teal, &lifecycle()),
        Err(BoardSpecError::Parse(_))
    ));
}
