//! Shared deterministic project-config edit model (DOC-35 §6, #2120).
//!
//! Why: `tm projects config <name> set/unset/tags` (CLI, `bin/tm`) and the TUI
//! config form (`tui::project_ctl`) are both thin clients over the same
//! `PATCH /api/v1/projects/{name}` endpoint (§6 RESOLVED — "one
//! validation/persistence implementation behind two front ends"). This module
//! is the client-side half of that "one implementation": [`ConfigEdit`] is the
//! deterministic, front-end-neutral shape of a single field mutation, and
//! [`build_patch_args`]/[`merge_patch_args`] are the ONE place that turns an
//! edit (or several) into the wire-shape [`PatchProjectArgs`] the daemon
//! expects — so the CLI's `set`/`unset`/`tags` verbs and the TUI form's
//! multi-field diff-and-submit both route through identical logic and cannot
//! drift on what a given edit means on the wire.
//! What: [`ConfigField`]/[`ClearableField`] (the settable / clearable field
//! vocabulary — `default_branch` is settable but NOT clearable, mirroring
//! `PatchProjectBody`'s lack of an unset story for that field),
//! [`ConfigEdit`] (one edit operation), [`build_patch_args`] (one edit → a
//! [`PatchProjectArgs`]), [`merge_patch_args`] (several edits → one merged
//! [`PatchProjectArgs`], for the TUI form's "one PATCH reflecting every
//! changed field" submit), and [`config_edit_cases`]/[`assert_matches_case`]
//! — the shared table of (edit, expected wire shape) cases the issue's "one
//! shared validation/persistence test suite exercised from both a CLI
//! integration test and a TUI form unit test" requirement calls for. NOT
//! gated behind `#[cfg(test)]`: the `tm` binary's integration tests
//! (`bin/tm/tests_projects.rs`) are a separate compilation unit from this
//! library crate's own `#[cfg(test)]` items (a different target in the same
//! package), so the shared case table has to be plain `pub` to be reachable
//! from both front ends' test code.
//! Test: `edit_cases_build_expected_patch_args` / `merge_patch_args_combines_independent_field_edits`
//! (in this module, exercising [`config_edit_cases`] against [`build_patch_args`]
//! directly) plus the two front-end-specific consumers:
//! `tests_projects.rs::projects_config_shared_cases_match_cli_arg_building` (CLI)
//! and `tui::project_ctl::state::modals::tests::config_form_shared_cases_match_tui_diff` (TUI).

use crate::client::http_client::projects::PatchProjectArgs;

/// A field that can be SET via `tm projects config <name> set <field> <value>`
/// or a TUI form field edit (DOC-35 §6).
///
/// Why: mirrors `PatchProjectBody`'s settable fields exactly — `repo_url` is
/// deliberately excluded (§6's field table and the CLI design both scope the
/// configurator to `default_branch`/`description`/`tags`/`stack_hint`/`gh_user`;
/// `repo_url` is a `register`-time field, not a configurator field).
/// What: the four settable fields.
/// Test: `edit_cases_build_expected_patch_args`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigField {
    /// `default_branch` — required-string field, no clear story (see
    /// [`ClearableField`]'s doc for why it has no `Unset` counterpart).
    DefaultBranch,
    /// `description` — double-Option field, clearable.
    Description,
    /// `stack_hint` — double-Option field, clearable.
    StackHint,
    /// `gh_user` — double-Option field, clearable (#2081; deep `gh auth
    /// status` validation deferred to #2121, out of scope here).
    GhUser,
}

/// A field that can be UNSET (cleared) via `tm projects config <name> unset
/// <field>` or a TUI form field clear.
///
/// Why: deliberately NARROWER than [`ConfigField`] — `default_branch` is a
/// required, non-double-Option field on the wire
/// (`PatchProjectBody::default_branch: Option<String>`, absent=unchanged,
/// present+blank=400, present+non-blank=set) with NO wire representation for
/// "clear" at all, unlike `description`/`stack_hint`/`gh_user`'s double-Option
/// `Some(None)` = JSON `null` = clear. Modeling this as a smaller enum
/// (rather than a runtime check) rejects `unset default_branch` at the type
/// level in both front ends — the CLI's clap `ValueEnum` simply has no such
/// variant to parse, and the TUI form's field-clear key is only wired for
/// these three rows.
/// What: the three clearable fields.
/// Test: `edit_cases_build_expected_patch_args`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearableField {
    /// `description`.
    Description,
    /// `stack_hint`.
    StackHint,
    /// `gh_user`.
    GhUser,
}

/// One deterministic config-field mutation (DOC-35 §6: "deterministic forms,
/// not free text").
///
/// Why: the front-end-neutral shape both `tm projects config` verbs and the
/// TUI form's per-field diff produce; [`build_patch_args`]/[`merge_patch_args`]
/// are the only code that turns this into wire bytes.
/// What: `Set` (field + new value), `Unset` (clearable field only), `Tags`
/// (add/remove lists, mirroring the `--add`/`--remove` CLI verb and the "no
/// free-text replace-whole-list footgun" §6 rule).
/// Test: `edit_cases_build_expected_patch_args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigEdit {
    /// Set `field` to `value`.
    Set(ConfigField, String),
    /// Clear `field` back to absent.
    Unset(ClearableField),
    /// Add/remove tags (server applies `add` before `remove`, DOC-35 §6).
    Tags {
        /// Tags to add.
        add: Vec<String>,
        /// Tags to remove.
        remove: Vec<String>,
    },
}

/// Apply one [`ConfigEdit`] onto an accumulating [`PatchProjectArgs`].
///
/// Why: the shared mutation step [`build_patch_args`] and [`merge_patch_args`]
/// both fold through — keeping it as one `&mut` helper (rather than
/// duplicating the match in each caller) is what makes "several edits merge
/// into one PATCH" and "one edit builds one PATCH" provably consistent.
/// What: sets exactly the wire fields `edit` touches; an empty `add`/`remove`
/// list in `Tags` leaves the corresponding wire field `None` (omitted),
/// matching the "absent = don't touch" contract rather than serializing an
/// empty array.
fn apply_edit(args: &mut PatchProjectArgs, edit: &ConfigEdit) {
    match edit {
        ConfigEdit::Set(ConfigField::DefaultBranch, v) => args.default_branch = Some(v.clone()),
        ConfigEdit::Set(ConfigField::Description, v) => args.description = Some(Some(v.clone())),
        ConfigEdit::Set(ConfigField::StackHint, v) => args.stack_hint = Some(Some(v.clone())),
        ConfigEdit::Set(ConfigField::GhUser, v) => args.gh_user = Some(Some(v.clone())),
        ConfigEdit::Unset(ClearableField::Description) => args.description = Some(None),
        ConfigEdit::Unset(ClearableField::StackHint) => args.stack_hint = Some(None),
        ConfigEdit::Unset(ClearableField::GhUser) => args.gh_user = Some(None),
        ConfigEdit::Tags { add, remove } => {
            if !add.is_empty() {
                args.tags_add = Some(add.clone());
            }
            if !remove.is_empty() {
                args.tags_remove = Some(remove.clone());
            }
        }
    }
}

/// Build a [`PatchProjectArgs`] reflecting exactly one [`ConfigEdit`].
///
/// Why: the CLI's `set`/`unset`/`tags` verbs each apply exactly one edit per
/// invocation; this is their direct call site.
/// What: an otherwise-empty (`..Default::default()`) args value with only the
/// fields `edit` touches populated.
/// Test: `edit_cases_build_expected_patch_args`.
pub fn build_patch_args(edit: &ConfigEdit) -> PatchProjectArgs {
    let mut args = PatchProjectArgs::default();
    apply_edit(&mut args, edit);
    args
}

/// Merge several [`ConfigEdit`]s into ONE [`PatchProjectArgs`] (DOC-35 §6, TUI
/// form submit).
///
/// Why: the TUI config form lets the operator change several fields before
/// submitting once; the server's absent=unchanged PATCH contract means the
/// single outgoing request must carry every changed field and nothing else —
/// this is the one place that fold happens, built from the same per-edit
/// [`apply_edit`] step as [`build_patch_args`].
/// What: applies each edit in order onto one accumulating [`PatchProjectArgs`].
/// Test: `merge_patch_args_combines_independent_field_edits`.
pub fn merge_patch_args(edits: &[ConfigEdit]) -> PatchProjectArgs {
    let mut args = PatchProjectArgs::default();
    for edit in edits {
        apply_edit(&mut args, edit);
    }
    args
}

/// One shared (edit, expected wire shape) case (#2120 issue requirement: "one
/// shared validation/persistence test suite exercised from both a CLI
/// integration test and a TUI form unit test").
///
/// Why: a plain data record (rather than a closure) so both consumers can
/// print `label` on assertion failure and neither needs to guess the other's
/// expectations.
/// What: one [`ConfigEdit`] plus the wire-shape it must produce, expressed
/// field-by-field so [`assert_matches_case`] can pinpoint exactly which wire
/// field diverged.
pub struct ConfigEditCase {
    /// Human-readable case name, printed in assertion failures.
    pub label: &'static str,
    /// The edit under test.
    pub edit: ConfigEdit,
    /// Expected `PatchProjectArgs::default_branch`.
    pub expect_default_branch: Option<&'static str>,
    /// Expected `PatchProjectArgs::description` (outer `None` = absent).
    pub expect_description: Option<Option<&'static str>>,
    /// Expected `PatchProjectArgs::stack_hint`.
    pub expect_stack_hint: Option<Option<&'static str>>,
    /// Expected `PatchProjectArgs::gh_user`.
    pub expect_gh_user: Option<Option<&'static str>>,
    /// Expected `PatchProjectArgs::tags_add`, empty when absent.
    pub expect_tags_add: &'static [&'static str],
    /// Expected `PatchProjectArgs::tags_remove`, empty when absent.
    pub expect_tags_remove: &'static [&'static str],
}

/// The shared case table (#2120 issue requirement — see the module doc).
///
/// Why: one field per settable/clearable field plus a combined tags
/// add+remove case is enough to pin every distinct wire transformation
/// [`apply_edit`] performs without inflating to per-value-variant redundancy.
/// What: eight cases: set/unset for each of the three double-Option fields,
/// set for `default_branch` (no unset case exists — see [`ClearableField`]'s
/// doc), and one combined tags add+remove case.
pub fn config_edit_cases() -> Vec<ConfigEditCase> {
    vec![
        ConfigEditCase {
            label: "set default_branch",
            edit: ConfigEdit::Set(ConfigField::DefaultBranch, "develop".to_string()),
            expect_default_branch: Some("develop"),
            expect_description: None,
            expect_stack_hint: None,
            expect_gh_user: None,
            expect_tags_add: &[],
            expect_tags_remove: &[],
        },
        ConfigEditCase {
            label: "set description",
            edit: ConfigEdit::Set(ConfigField::Description, "new desc".to_string()),
            expect_default_branch: None,
            expect_description: Some(Some("new desc")),
            expect_stack_hint: None,
            expect_gh_user: None,
            expect_tags_add: &[],
            expect_tags_remove: &[],
        },
        ConfigEditCase {
            label: "unset description",
            edit: ConfigEdit::Unset(ClearableField::Description),
            expect_default_branch: None,
            expect_description: Some(None),
            expect_stack_hint: None,
            expect_gh_user: None,
            expect_tags_add: &[],
            expect_tags_remove: &[],
        },
        ConfigEditCase {
            label: "set stack_hint",
            edit: ConfigEdit::Set(ConfigField::StackHint, "rust".to_string()),
            expect_default_branch: None,
            expect_description: None,
            expect_stack_hint: Some(Some("rust")),
            expect_gh_user: None,
            expect_tags_add: &[],
            expect_tags_remove: &[],
        },
        ConfigEditCase {
            label: "unset stack_hint",
            edit: ConfigEdit::Unset(ClearableField::StackHint),
            expect_default_branch: None,
            expect_description: None,
            expect_stack_hint: Some(None),
            expect_gh_user: None,
            expect_tags_add: &[],
            expect_tags_remove: &[],
        },
        ConfigEditCase {
            label: "set gh_user",
            edit: ConfigEdit::Set(ConfigField::GhUser, "acme-bot".to_string()),
            expect_default_branch: None,
            expect_description: None,
            expect_stack_hint: None,
            expect_gh_user: Some(Some("acme-bot")),
            expect_tags_add: &[],
            expect_tags_remove: &[],
        },
        ConfigEditCase {
            label: "unset gh_user",
            edit: ConfigEdit::Unset(ClearableField::GhUser),
            expect_default_branch: None,
            expect_description: None,
            expect_stack_hint: None,
            expect_gh_user: Some(None),
            expect_tags_add: &[],
            expect_tags_remove: &[],
        },
        ConfigEditCase {
            label: "tags add and remove together",
            edit: ConfigEdit::Tags {
                add: vec!["ml".to_string()],
                remove: vec!["oss".to_string()],
            },
            expect_default_branch: None,
            expect_description: None,
            expect_stack_hint: None,
            expect_gh_user: None,
            expect_tags_add: &["ml"],
            expect_tags_remove: &["oss"],
        },
    ]
}

/// Assert `args` matches `case`'s expected wire shape.
///
/// Why: the ONE comparison routine both front ends' shared-suite tests call,
/// so the assertion logic itself cannot drift between the two call sites.
/// What: field-by-field equality, each with `case.label` in the failure
/// message so a mismatch names both the case AND the field.
pub fn assert_matches_case(args: &PatchProjectArgs, case: &ConfigEditCase) {
    assert_eq!(
        args.default_branch.as_deref(),
        case.expect_default_branch,
        "{}: default_branch",
        case.label
    );
    assert_eq!(
        args.description.as_ref().map(|o| o.as_deref()),
        case.expect_description,
        "{}: description",
        case.label
    );
    assert_eq!(
        args.stack_hint.as_ref().map(|o| o.as_deref()),
        case.expect_stack_hint,
        "{}: stack_hint",
        case.label
    );
    assert_eq!(
        args.gh_user.as_ref().map(|o| o.as_deref()),
        case.expect_gh_user,
        "{}: gh_user",
        case.label
    );
    let tags_add: Vec<&str> = args
        .tags_add
        .as_ref()
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        tags_add,
        case.expect_tags_add.to_vec(),
        "{}: tags_add",
        case.label
    );
    let tags_remove: Vec<&str> = args
        .tags_remove
        .as_ref()
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        tags_remove,
        case.expect_tags_remove.to_vec(),
        "{}: tags_remove",
        case.label
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_cases_build_expected_patch_args() {
        for case in config_edit_cases() {
            let args = build_patch_args(&case.edit);
            assert_matches_case(&args, &case);
        }
    }

    #[test]
    fn merge_patch_args_combines_independent_field_edits() {
        let edits = vec![
            ConfigEdit::Set(ConfigField::StackHint, "rust".to_string()),
            ConfigEdit::Unset(ClearableField::Description),
        ];
        let args = merge_patch_args(&edits);
        assert_eq!(args.stack_hint, Some(Some("rust".to_string())));
        assert_eq!(args.description, Some(None));
        assert!(args.default_branch.is_none());
    }

    #[test]
    fn build_patch_args_omits_empty_tags_lists() {
        let args = build_patch_args(&ConfigEdit::Tags {
            add: vec![],
            remove: vec![],
        });
        assert!(args.tags_add.is_none());
        assert!(args.tags_remove.is_none());
    }
}
