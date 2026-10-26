//! Load-time rules for `board-v1.yaml` (#10275).
//!
//! Why: a spec that drifts from the standard, or whose Status options do not
//! map one-to-one onto the `status:*` labels, must be refused before Phase 2
//! creates or checks a board from it. Each refusal names the field, option or
//! state at fault.
//! What: [`BoardSpecError`] (`thiserror`) and [`validate_spec`], which checks
//! the version, the version-1 field set and Status option names, each
//! field's options, the Status-to-state mapping, the views, the workflows and
//! the README placeholders.
//! Test: `board/tests.rs`.

use std::collections::{BTreeMap, BTreeSet};

use super::spec::{BoardSpec, FieldKind, SUPPORTED_SPEC_VERSION};
use crate::commands::issue::config::StateModel;

/// The field the Status-to-state mapping lives on.
pub(crate) const STATUS_FIELD: &str = "Status";

/// The field declared present and off until #9401 (D4).
pub(crate) const HOLD_FIELD: &str = "Hold";

/// What `version: 1` means: these six fields, no more and no fewer (D6).
///
/// Why: AC1 of #10275 pins the standard set, so a spec that adds Crate or
/// drops a date field is refused rather than shipped. Every other property —
/// type, colors, descriptions, order — comes from the spec itself (D1).
pub(crate) const V1_FIELDS: [&str; 6] = [
    STATUS_FIELD,
    "Start date",
    "Target date",
    "Deployed on",
    "Published on",
    HOLD_FIELD,
];

/// What `version: 1` means for Status: these nine option names, in order.
pub(crate) const V1_STATUS_OPTIONS: [&str; 9] = [
    "Backlog",
    "Ready for Development",
    "In Development",
    "In PR",
    "On Preview",
    "Merged",
    "On Prod",
    "Published",
    "Won't Do",
];

/// README placeholders the spec may use.
const README_PLACEHOLDERS: [&str; 2] = ["repo", "status_table"];

/// A board spec or `project:` block refusal.
///
/// Why: one variant per rule lets a test assert the exact refusal and the name
/// it carries.
/// What: parse failures, then one variant per rule in [`validate_spec`] and in
/// `super::project::validate_project`.
/// Test: each variant is produced by a test in `board/tests.rs`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum BoardSpecError {
    /// The spec YAML does not parse.
    #[error("board spec does not parse: {0}")]
    Parse(String),
    /// The lifecycle the spec maps onto does not parse.
    #[error("the built-in status:* lifecycle does not parse: {0}")]
    Lifecycle(String),
    /// `version` is not the supported value.
    #[error("unsupported board spec version {found}; this build supports version {supported}")]
    UnsupportedVersion {
        /// The version found.
        found: u32,
        /// The version this build supports.
        supported: u32,
    },
    /// A field name appears twice.
    #[error("board spec declares field `{0}` twice")]
    DuplicateField(String),
    /// A field outside the standard set.
    #[error("field `{0}` is not a standard field; a repo declares it under `project.extra_fields`")]
    NonStandardField(String),
    /// A standard field is missing.
    #[error("board spec omits the standard field `{0}`")]
    MissingField(String),
    /// The Status option names are not the nine standard names in order.
    #[error("Status options must be exactly {expected:?} in that order; found {found:?}")]
    StatusOptions {
        /// The standard names.
        expected: Vec<String>,
        /// The names found.
        found: Vec<String>,
    },
    /// A single-select field with no options.
    #[error("single-select field `{0}` has no options")]
    NoOptions(String),
    /// Options on a field that is not a single-select.
    #[error("field `{0}` is not a single-select and cannot have options")]
    OptionsOnNonSelect(String),
    /// An option name appears twice in one field.
    #[error("field `{field}` declares option `{option}` twice")]
    DuplicateOption {
        /// The field.
        field: String,
        /// The repeated option.
        option: String,
    },
    /// A Status option with neither `state` nor `label: none`.
    #[error(
        "Status option `{0}` maps to no state; give it `state: <issue-state name>` or `label: none`"
    )]
    UnmappedOption(String),
    /// A Status option with both `state` and `label: none`.
    #[error("Status option `{0}` declares both `state` and `label: none`; give it one")]
    DoublyMappedOption(String),
    /// A mapping key on an option outside Status.
    #[error(
        "option `{option}` of field `{field}` carries a state mapping; only Status options map to states"
    )]
    MappingOutsideStatus {
        /// The field.
        field: String,
        /// The option.
        option: String,
    },
    /// A Status option names a state that is not a labelled lifecycle state.
    #[error(
        "Status option `{option}` maps to `{state}`, which is not a labelled state of the \
         issue-state model; map it to one of {known:?} or declare `label: none`"
    )]
    UnknownState {
        /// The option.
        option: String,
        /// The state it names.
        state: String,
        /// The labelled states of the model.
        known: Vec<String>,
    },
    /// Two Status options map to one state.
    #[error(
        "Status options `{first}` and `{second}` both map to state `{state}`; each state is reached by exactly one option"
    )]
    StateMappedTwice {
        /// The state.
        state: String,
        /// The first option mapping to it.
        first: String,
        /// The second option mapping to it.
        second: String,
    },
    /// A labelled lifecycle state no Status option reaches.
    #[error("no Status option maps to state `{0}`; each state is reached by exactly one option")]
    StateNotMapped(String),
    /// A view name appears twice.
    #[error("board spec declares view `{0}` twice")]
    DuplicateView(String),
    /// A view shows a field that is neither standard nor built-in.
    #[error(
        "view `{view}` shows `{field}`, which is neither a standard field nor a GitHub built-in field"
    )]
    ViewUnknownField {
        /// The view.
        view: String,
        /// The field.
        field: String,
    },
    /// A view groups by a field that is not a standard single-select.
    #[error("view `{view}` groups by `{field}`, which is not a standard single-select field")]
    ViewBadGroupBy {
        /// The view.
        view: String,
        /// The field.
        field: String,
    },
    /// A workflow name appears twice.
    #[error("board spec declares workflow `{0}` twice")]
    DuplicateWorkflow(String),
    /// The README uses an undefined placeholder.
    #[error(
        "README template uses undefined placeholder `{{{{{0}}}}}`; defined: {{{{repo}}}}, {{{{status_table}}}}"
    )]
    UnknownPlaceholder(String),
    /// A `project:` block with an empty owner.
    #[error("`project.owner` is empty")]
    EmptyOwner,
    /// A `project:` block with project number 0.
    #[error("`project.number` must be a GitHub project number, 1 or greater")]
    ZeroProjectNumber,
    /// A `project:` block drops an option the spec does not declare.
    #[error("`project.drop_options` names `{0}`, which is not a Status option of the board spec")]
    UnknownDropOption(String),
    /// A `project:` block declares a standard field as an extra.
    #[error("`project.extra_fields` names `{0}`, which is a standard field of the board spec")]
    ExtraIsStandard(String),
}

/// Run every load-time rule on a parsed spec.
///
/// Why: see the module doc; `lifecycle` is the issue-state model whose
/// labelled states the Status options map onto.
/// What: version, then the field set, then per-field options, then the Status
/// mapping, views, workflows and README placeholders. The first broken rule
/// is returned.
/// Test: every refusal test in `board/tests.rs`.
pub(crate) fn validate_spec(
    spec: &BoardSpec,
    lifecycle: &StateModel,
) -> Result<(), BoardSpecError> {
    if spec.version != SUPPORTED_SPEC_VERSION {
        return Err(BoardSpecError::UnsupportedVersion {
            found: spec.version,
            supported: SUPPORTED_SPEC_VERSION,
        });
    }
    check_field_set(spec)?;
    check_options(spec)?;
    check_status_mapping(spec, lifecycle)?;
    check_views(spec)?;
    check_workflows(spec)?;
    check_readme(&spec.readme)
}

/// Exactly the six standard fields, each once, and the nine Status names.
fn check_field_set(spec: &BoardSpec) -> Result<(), BoardSpecError> {
    let mut seen = BTreeSet::new();
    for field in &spec.fields {
        if !seen.insert(field.name.as_str()) {
            return Err(BoardSpecError::DuplicateField(field.name.clone()));
        }
        if !V1_FIELDS.contains(&field.name.as_str()) {
            return Err(BoardSpecError::NonStandardField(field.name.clone()));
        }
    }
    if let Some(missing) = V1_FIELDS.iter().find(|n| !seen.contains(*n)) {
        return Err(BoardSpecError::MissingField((*missing).to_string()));
    }
    let found: Vec<&str> = spec
        .field(STATUS_FIELD)
        .map(|f| f.options.iter().map(|o| o.name.as_str()).collect())
        .unwrap_or_default();
    if found != V1_STATUS_OPTIONS {
        return Err(BoardSpecError::StatusOptions {
            expected: V1_STATUS_OPTIONS.iter().map(|s| s.to_string()).collect(),
            found: found.iter().map(|s| s.to_string()).collect(),
        });
    }
    Ok(())
}

/// Options only on single-selects, at least one there, each name once.
fn check_options(spec: &BoardSpec) -> Result<(), BoardSpecError> {
    for field in &spec.fields {
        match (field.kind, field.options.is_empty()) {
            (FieldKind::SingleSelect, true) => {
                return Err(BoardSpecError::NoOptions(field.name.clone()));
            }
            (FieldKind::Date, false) => {
                return Err(BoardSpecError::OptionsOnNonSelect(field.name.clone()));
            }
            _ => {}
        }
        let mut seen = BTreeSet::new();
        for option in &field.options {
            if !seen.insert(option.name.as_str()) {
                return Err(BoardSpecError::DuplicateOption {
                    field: field.name.clone(),
                    option: option.name.clone(),
                });
            }
            let mapped = option.state.is_some() || option.label.is_some();
            if field.name != STATUS_FIELD && mapped {
                return Err(BoardSpecError::MappingOutsideStatus {
                    field: field.name.clone(),
                    option: option.name.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Each Status option maps once; each labelled state is reached exactly once.
fn check_status_mapping(spec: &BoardSpec, lifecycle: &StateModel) -> Result<(), BoardSpecError> {
    let known: Vec<String> = lifecycle
        .states
        .iter()
        .filter(|s| s.label.is_some())
        .map(|s| s.name.clone())
        .collect();
    let Some(status) = spec.field(STATUS_FIELD) else {
        return Err(BoardSpecError::MissingField(STATUS_FIELD.to_string()));
    };
    let mut reached: BTreeMap<&str, &str> = BTreeMap::new();
    for option in &status.options {
        let state = match (&option.state, option.label) {
            (None, None) => return Err(BoardSpecError::UnmappedOption(option.name.clone())),
            (Some(_), Some(_)) => {
                return Err(BoardSpecError::DoublyMappedOption(option.name.clone()));
            }
            (None, Some(_)) => continue,
            (Some(state), None) => state,
        };
        if !known.contains(state) {
            return Err(BoardSpecError::UnknownState {
                option: option.name.clone(),
                state: state.clone(),
                known,
            });
        }
        if let Some(first) = reached.insert(state.as_str(), option.name.as_str()) {
            return Err(BoardSpecError::StateMappedTwice {
                state: state.clone(),
                first: first.to_string(),
                second: option.name.clone(),
            });
        }
    }
    if let Some(unreached) = known.iter().find(|s| !reached.contains_key(s.as_str())) {
        return Err(BoardSpecError::StateNotMapped(unreached.clone()));
    }
    Ok(())
}

/// Views name each field once and show only standard or built-in fields.
fn check_views(spec: &BoardSpec) -> Result<(), BoardSpecError> {
    let mut seen = BTreeSet::new();
    for view in &spec.views {
        if !seen.insert(view.name.as_str()) {
            return Err(BoardSpecError::DuplicateView(view.name.clone()));
        }
        for field in &view.fields {
            if spec.field(field).is_none() && !spec.builtin_fields.contains(field) {
                return Err(BoardSpecError::ViewUnknownField {
                    view: view.name.clone(),
                    field: field.clone(),
                });
            }
        }
        if let Some(group) = &view.vertical_group_by {
            let is_select = spec
                .field(group)
                .is_some_and(|f| f.kind == FieldKind::SingleSelect);
            if !is_select {
                return Err(BoardSpecError::ViewBadGroupBy {
                    view: view.name.clone(),
                    field: group.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Each workflow is named once.
fn check_workflows(spec: &BoardSpec) -> Result<(), BoardSpecError> {
    let mut seen = BTreeSet::new();
    for workflow in &spec.workflows {
        if !seen.insert(workflow.name.as_str()) {
            return Err(BoardSpecError::DuplicateWorkflow(workflow.name.clone()));
        }
    }
    Ok(())
}

/// Every `{{name}}` in the README is a defined placeholder.
fn check_readme(readme: &str) -> Result<(), BoardSpecError> {
    let mut rest = readme;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            break;
        };
        let name = after[..close].trim();
        if !README_PLACEHOLDERS.contains(&name) {
            return Err(BoardSpecError::UnknownPlaceholder(name.to_string()));
        }
        rest = &after[close + 2..];
    }
    Ok(())
}
