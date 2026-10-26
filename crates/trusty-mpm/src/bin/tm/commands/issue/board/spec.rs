//! `board-v1.yaml` — the standard board spec, embedded and loaded (#10275).
//!
//! Why: the board format is configuration owned by trusty-tools, not code. The
//! spec is the single source of field and option names (epic #10274, D1), and
//! compiling it in means every `tm` carries the format it was released with.
//! What: the serde types of the spec ([`BoardSpec`] and its parts), the
//! embedded [`BOARD_SPEC_YAML`], and [`load_spec`] / [`embedded_spec`], which
//! parse and then run [`super::validate::validate_spec`].
//! Test: `board/tests.rs`.

use serde::{Deserialize, Serialize};

use super::validate::{BoardSpecError, validate_spec};
use crate::commands::issue::config::{STATUS_LIFECYCLE_YAML, StateModel};

/// The embedded board spec (#10275).
///
/// Why: Phase 2's `tm issue board` verbs need the format with zero on-disk
/// config, the same way `tm issue` needs its lifecycle.
/// What: the verbatim contents of `examples/issue-state/board-v1.yaml`, embedded
/// the way `STATUS_LIFECYCLE_YAML` is.
/// Test: `embedded_spec_has_the_version_and_the_six_standard_fields_10275`.
pub(crate) const BOARD_SPEC_YAML: &str =
    include_str!("../../../../../../examples/issue-state/board-v1.yaml");

/// The only board-spec version this build understands.
pub(crate) const SUPPORTED_SPEC_VERSION: u32 = 1;

/// Root of `board-v1.yaml`.
///
/// Why: one deserialization target for the whole board format.
/// What: the spec `version`, the custom `fields`, the GitHub `builtin_fields`
/// a view may show, the `views`, the built-in `workflows` intents and the
/// `readme` template. Unknown keys are refused at parse.
/// Test: `embedded_spec_has_the_version_and_the_six_standard_fields_10275`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoardSpec {
    /// Spec version (must equal [`SUPPORTED_SPEC_VERSION`]).
    pub(crate) version: u32,
    /// The standard custom fields, in board order.
    pub(crate) fields: Vec<FieldSpec>,
    /// GitHub-provided fields; `tm` creates none of them.
    #[serde(default)]
    pub(crate) builtin_fields: Vec<String>,
    /// The standard views.
    #[serde(default)]
    pub(crate) views: Vec<ViewSpec>,
    /// The built-in workflow intents.
    #[serde(default)]
    pub(crate) workflows: Vec<WorkflowSpec>,
    /// The board README template.
    #[serde(default)]
    pub(crate) readme: String,
}

impl BoardSpec {
    /// The field named `name`, if the spec declares it.
    pub(crate) fn field(&self, name: &str) -> Option<&FieldSpec> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// One custom board field.
///
/// Why: a field's name, type and options are what `init` creates and `check`
/// compares.
/// What: `name`, `type`, `enabled` (default `true`; `false` declares the field
/// present but off, D4) and, for a single-select, its ordered `options`.
/// Test: `a_spec_without_hold_is_refused_and_hold_enabled_is_accepted_10275`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldSpec {
    /// Field name, the stable key.
    pub(crate) name: String,
    /// GitHub field data type.
    #[serde(rename = "type")]
    pub(crate) kind: FieldKind,
    /// `false` declares the field present and switched off.
    #[serde(default = "enabled_by_default")]
    pub(crate) enabled: bool,
    /// Single-select options, in board order.
    #[serde(default)]
    pub(crate) options: Vec<OptionSpec>,
}

fn enabled_by_default() -> bool {
    true
}

/// The field data types the standard uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FieldKind {
    /// A single-select field with options.
    SingleSelect,
    /// A date field.
    Date,
}

/// One single-select option.
///
/// Why: an option's name, color and description are what a conformant board
/// carries; a Status option also names the `status:*` label it stands for.
/// What: `name`, `color`, `description`, and on a Status option exactly one of
/// `state` (an issue-state.yaml state) or `label: none`.
/// Test: `a_status_option_without_a_mapping_is_refused_naming_it_10275`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OptionSpec {
    /// Option name, the stable key.
    pub(crate) name: String,
    /// GitHub option color.
    pub(crate) color: OptionColor,
    /// Option description.
    #[serde(default)]
    pub(crate) description: String,
    /// The issue-state.yaml state whose label this option stands for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<String>,
    /// `label: none` — the option stands for no label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<NoLabel>,
}

/// The only value `label:` takes: `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum NoLabel {
    /// The option stands for no `status:*` label.
    None,
}

/// GitHub's ProjectV2 single-select option colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum OptionColor {
    /// Gray.
    Gray,
    /// Blue.
    Blue,
    /// Green.
    Green,
    /// Yellow.
    Yellow,
    /// Orange.
    Orange,
    /// Red.
    Red,
    /// Pink.
    Pink,
    /// Purple.
    Purple,
}

/// One board view.
///
/// Why: `init` creates the views and `apply` restores a missing one by name.
/// What: `name`, `layout`, the visible `fields` in order, and an optional
/// `vertical_group_by` single-select field.
/// Test: `a_view_naming_a_repo_extra_is_refused_10275`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewSpec {
    /// View name, the stable key.
    pub(crate) name: String,
    /// View layout.
    pub(crate) layout: ViewLayout,
    /// Visible fields, in order.
    #[serde(default)]
    pub(crate) fields: Vec<String>,
    /// The field a board layout groups its columns by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vertical_group_by: Option<String>,
    /// The view filter; empty for none.
    // #10276: owner decision 1 — `init` and `apply` set a view's filter.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) filter: String,
}

/// GitHub's ProjectV2 view layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ViewLayout {
    /// `TABLE_LAYOUT`.
    Table,
    /// `BOARD_LAYOUT`.
    Board,
    /// `ROADMAP_LAYOUT`.
    Roadmap,
}

/// One built-in workflow intent.
///
/// Why: the API cannot enable a workflow, so the spec records which ones a
/// board should have on, for `init` to print and `check` to report.
/// What: the workflow `name`, whether it is `enabled`, and a `description`.
/// Test: `embedded_spec_has_the_version_and_the_six_standard_fields_10275`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowSpec {
    /// Workflow name as GitHub shows it.
    pub(crate) name: String,
    /// Whether the board should have it on.
    pub(crate) enabled: bool,
    /// What the workflow is for.
    #[serde(default)]
    pub(crate) description: String,
}

/// Parse and validate a board spec against an issue-state model.
///
/// Why: every rule must hold before any later phase acts on the spec.
/// What: parses `yaml` (unknown keys, colors, types and layouts are refused
/// here), then runs [`validate_spec`], checking the Status mapping against
/// `lifecycle`.
/// Test: every refusal test in `board/tests.rs`.
pub(crate) fn load_spec(yaml: &str, lifecycle: &StateModel) -> Result<BoardSpec, BoardSpecError> {
    let spec: BoardSpec =
        serde_yaml::from_str(yaml).map_err(|e| BoardSpecError::Parse(e.to_string()))?;
    validate_spec(&spec, lifecycle)?;
    Ok(spec)
}

/// The embedded spec, validated against the built-in `status:*` lifecycle.
///
/// Why: the Status options map to that lifecycle's labels, the one TICKETING.md
/// documents.
/// What: parses `STATUS_LIFECYCLE_YAML`, then [`load_spec`]s
/// [`BOARD_SPEC_YAML`] against it.
/// Test: `embedded_spec_has_the_version_and_the_six_standard_fields_10275`.
pub(crate) fn embedded_spec() -> Result<BoardSpec, BoardSpecError> {
    let lifecycle: StateModel = serde_yaml::from_str(STATUS_LIFECYCLE_YAML)
        .map_err(|e| BoardSpecError::Lifecycle(e.to_string()))?;
    load_spec(BOARD_SPEC_YAML, &lifecycle)
}
