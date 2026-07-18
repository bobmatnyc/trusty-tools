//! Modal view/form state for the `tm projects` TUI (DOC-35 §10.8/§6, #2383/#2120).
//!
//! Why: hoisted out of `state/mod.rs` pre-emptively (per CLAUDE.md's SLOC-cap
//! convention: "one public module per logical concept, a thin `mod.rs` that
//! re-exports, and sibling files with clear single responsibilities") so
//! adding the #2120 config form's state does not push `state/mod.rs` over the
//! 500-SLOC production cap. Groups the two overlay/modal types this screen
//! has — the read-only [`DeliverableView`] (#2383) and the editable
//! [`ConfigFormView`] (#2120) — plus their [`ProjectCtlState`] mutator
//! methods, since both are "a second thing captured on `ProjectCtlState` that
//! captures all keyboard input while `Some`" in the same way.
//! What: [`DeliverableView`] (unchanged from its pre-split shape) and the
//! config-form types: [`ConfigFormField`] (one editable text field, pairing a
//! bounded buffer with its server-loaded original so a diff can tell "edited"
//! from "untouched"), [`ConfigFormTags`] (the tags row's comma-separated
//! editable buffer + original list), [`ConfigFormFocus`] (which of the five
//! rows Tab/Shift+Tab currently lands on), and [`ConfigFormView`] itself
//! (`diff_edits` — the ONE place the form's current buffers turn into the
//! shared [`crate::project_config::ConfigEdit`]s
//! [`super::super::actions::dispatch`] PATCHes). The second `impl
//! ProjectCtlState` block here (`open_config_form`/`close_config_form`/
//! `config_form_focus_next`/`config_form_focus_prev`/`config_form_push_char`/
//! `config_form_backspace`/`set_config_form_error`) is exactly analogous to
//! the Deliverable view's `open_deliverable_view`/`close_deliverable_view`/
//! `scroll_deliverable_view` trio, just with more mutators since the form is
//! editable rather than read-only.
//! Test: `super::tests` (moved-and-unchanged Deliverable-view coverage) plus
//! this module's own `tests` (config-form diff/edit/focus-cycle coverage,
//! including `config_form_shared_cases_match_tui_diff` — the TUI half of the
//! #2120 shared validation/persistence test suite).

use crate::deliverable::{Deliverable, Milestone};
use crate::project::Project;
use crate::project_config::{ClearableField, ConfigEdit, ConfigField};

use super::ProjectCtlState;

/// The read-only Deliverable/Milestone view for one project (DOC-35 §10.8
/// `show`, #2383), reachable from the Projects pane.
///
/// Why: the view is opened for exactly the project selected when the
/// operator requested it, and — like `PendingConfirm` — must pin that
/// target rather than silently following a later Projects-pane selection
/// change (which cannot happen anyway while the view is open, since it
/// captures all input, but pinning by value keeps the render layer from
/// re-deriving "which project" from a selection that a same-tick poll could
/// otherwise race). `deliverables` is populated from the same per-tick fetch
/// [`ProjectCtlState::deliverables`] uses for the Sessions-pane glyph (no
/// duplicate call — see `poll.rs`); `milestones` is fetched only while this
/// view is open, the one extra per-tick call this feature adds.
/// What: the target project's name, its Deliverables and Milestones as of
/// the last successful fetch, and a line-based `scroll` offset for a list
/// too tall for the overlay.
/// Test: `super::super::panes::deliverables_view::tests`, `super::tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverableView {
    /// The project this view is scoped to.
    pub project_name: String,
    /// The project's Deliverables, as of the last successful fetch.
    pub deliverables: Vec<Deliverable>,
    /// The project's Milestones, as of the last successful fetch.
    pub milestones: Vec<Milestone>,
    /// How many lines the body is scrolled down from the top.
    pub scroll: u16,
}

/// One editable text field within the config form (DOC-35 §6, #2120).
///
/// Why: pairs a bounded editable buffer with the value LOADED from the
/// server when the form opened, so [`ConfigFormView::diff_edits`] can tell
/// "the operator touched this field" from "unchanged" without a separate
/// dirty flag per field — a field is unchanged iff its trimmed `value`
/// equals `original` (treating an absent `original` as empty).
/// What: `original` — `None` means the field was unset/absent on the server;
/// `value` — the current editable buffer, seeded to `original`'s text (or
/// empty) when the form opens.
/// Test: `config_form_diff_detects_set_and_unset`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigFormField {
    /// The value loaded when the form opened (`None` = unset/absent).
    pub original: Option<String>,
    /// The current editable buffer.
    pub value: String,
}

impl ConfigFormField {
    fn seeded(original: Option<String>) -> Self {
        let value = original.clone().unwrap_or_default();
        Self { original, value }
    }
}

/// The tags row's editable state (DOC-35 §6, #2120).
///
/// Why: **disclosed judgment call** — the issue's field table specifies tags
/// mutate via `--add`/`--remove` (no free-text whole-list replace), and the
/// CLI honors that with two flags. A 5-row form has no natural place for two
/// independent list-editors per the task's own "5 fields total" framing, so
/// this models tags as ONE editable comma-separated buffer (seeded from the
/// current tag list, e.g. `"backend, oss"`) and DERIVES the add/remove split
/// from a diff against `original` at submit time — the operator edits one
/// field's own string value (allowed: the "never free text" ban is about not
/// accepting arbitrary prose as a single command, not about banning text
/// editing of a field's own value), while the wire request still only ever
/// carries the deltas, never a whole-list replace.
/// What: `original` — the tag list as loaded; `value` — the editable
/// comma-separated buffer.
/// Test: `config_form_tags_diff_computes_add_and_remove`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigFormTags {
    /// The tag list loaded when the form opened.
    pub original: Vec<String>,
    /// The current editable comma-separated buffer.
    pub value: String,
}

impl ConfigFormTags {
    fn seeded(tags: Vec<String>) -> Self {
        let value = tags.join(", ");
        Self {
            original: tags,
            value,
        }
    }

    /// Diff the edited buffer against `original`, producing a
    /// [`ConfigEdit::Tags`] with exactly the added/removed entries, or `None`
    /// when the edited set is identical to `original`.
    fn diff_edit(&self) -> Option<ConfigEdit> {
        let current: Vec<String> = self
            .value
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let add: Vec<String> = current
            .iter()
            .filter(|t| !self.original.contains(t))
            .cloned()
            .collect();
        let remove: Vec<String> = self
            .original
            .iter()
            .filter(|t| !current.contains(t))
            .cloned()
            .collect();
        if add.is_empty() && remove.is_empty() {
            None
        } else {
            Some(ConfigEdit::Tags { add, remove })
        }
    }
}

/// Which of the config form's five rows currently has focus (Tab/Shift+Tab,
/// DOC-35 §6, #2120) — a DIFFERENT tab-cycle than the outer pane-focus Tab,
/// scoped to the modal (the outer `Tab` is unreachable while the form is
/// open — see [`super::super::events`]'s modal-precedence doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigFormFocus {
    /// `default_branch` row.
    #[default]
    DefaultBranch,
    /// `description` row.
    Description,
    /// `stack_hint` row.
    StackHint,
    /// `gh_user` row.
    GhUser,
    /// `tags` row.
    Tags,
}

impl ConfigFormFocus {
    /// Advance to the next row, wrapping after `Tags`.
    pub fn next(self) -> Self {
        match self {
            Self::DefaultBranch => Self::Description,
            Self::Description => Self::StackHint,
            Self::StackHint => Self::GhUser,
            Self::GhUser => Self::Tags,
            Self::Tags => Self::DefaultBranch,
        }
    }

    /// Step back to the previous row, wrapping before `DefaultBranch`.
    pub fn prev(self) -> Self {
        match self {
            Self::DefaultBranch => Self::Tags,
            Self::Description => Self::DefaultBranch,
            Self::StackHint => Self::Description,
            Self::GhUser => Self::StackHint,
            Self::Tags => Self::GhUser,
        }
    }
}

/// A deterministic config-edit form for one project (DOC-35 §6, #2120),
/// opened by `c` in the Projects pane.
///
/// Why: the fixed-field, tab-navigable, edit-then-confirm form §6 requires —
/// "not a chat box". Seeded once from the project's currently-known full
/// record ([`ProjectCtlState::projects_full`]) when opened; every subsequent
/// keystroke mutates only the editable buffers, never touches the server
/// until an explicit submit.
/// What: one [`ConfigFormField`] per settable/clearable field, one
/// [`ConfigFormTags`] for the tags row, the current [`ConfigFormFocus`], and
/// an inline `error` from the last failed submit (kept in the form, not a
/// transient toast, so the operator can fix and resubmit without losing
/// their other unsaved edits — explicit #2120 requirement).
/// Test: `config_form_diff_detects_set_and_unset`,
/// `config_form_tags_diff_computes_add_and_remove`,
/// `config_form_shared_cases_match_tui_diff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFormView {
    /// The project this form is scoped to.
    pub project_name: String,
    /// `default_branch` field state.
    pub default_branch: ConfigFormField,
    /// `description` field state.
    pub description: ConfigFormField,
    /// `stack_hint` field state.
    pub stack_hint: ConfigFormField,
    /// `gh_user` field state.
    pub gh_user: ConfigFormField,
    /// `tags` row state.
    pub tags: ConfigFormTags,
    /// Which row currently has focus.
    pub focus: ConfigFormFocus,
    /// An inline error from the last failed submit, if any — rendered IN the
    /// form (per the explicit #2120 requirement) rather than as a transient
    /// notice, so it survives until the operator edits again or resubmits.
    pub error: Option<String>,
}

impl ConfigFormView {
    fn from_project(project: &Project) -> Self {
        Self {
            project_name: project.name.clone(),
            default_branch: ConfigFormField::seeded(Some(project.default_branch.clone())),
            description: ConfigFormField::seeded(project.description.clone()),
            stack_hint: ConfigFormField::seeded(project.stack_hint.clone()),
            gh_user: ConfigFormField::seeded(project.gh_user.clone()),
            tags: ConfigFormTags::seeded(project.tags.clone()),
            focus: ConfigFormFocus::default(),
            error: None,
        }
    }

    /// Diff every field's current buffer against its loaded original,
    /// producing exactly the [`ConfigEdit`]s that changed.
    ///
    /// Why: the server's absent=unchanged PATCH contract (DOC-35 §6) means
    /// an unedited field must never be sent — this is the ONE place that
    /// rule is enforced for the form as a whole. `default_branch` has no
    /// "clear" story (see [`ClearableField`]'s doc), so a blanked buffer is
    /// simply skipped (treated as unchanged) rather than sent as an
    /// always-rejected empty `Set`.
    /// What: at most one [`ConfigEdit`] per field, plus at most one
    /// [`ConfigEdit::Tags`] for the tags row, in field-declaration order.
    /// Test: `config_form_diff_detects_set_and_unset`,
    /// `config_form_shared_cases_match_tui_diff`.
    pub fn diff_edits(&self) -> Vec<ConfigEdit> {
        let mut edits = Vec::new();
        let branch = self.default_branch.value.trim();
        if !branch.is_empty() && Some(branch) != self.default_branch.original.as_deref() {
            edits.push(ConfigEdit::Set(
                ConfigField::DefaultBranch,
                branch.to_string(),
            ));
        }
        push_clearable_edit(
            &mut edits,
            ConfigField::Description,
            ClearableField::Description,
            &self.description,
        );
        push_clearable_edit(
            &mut edits,
            ConfigField::StackHint,
            ClearableField::StackHint,
            &self.stack_hint,
        );
        push_clearable_edit(
            &mut edits,
            ConfigField::GhUser,
            ClearableField::GhUser,
            &self.gh_user,
        );
        if let Some(tags_edit) = self.tags.diff_edit() {
            edits.push(tags_edit);
        }
        edits
    }

    fn push_char(&mut self, c: char) {
        self.focused_buffer_mut().push(c);
    }

    fn backspace(&mut self) {
        self.focused_buffer_mut().pop();
    }

    fn focused_buffer_mut(&mut self) -> &mut String {
        match self.focus {
            ConfigFormFocus::DefaultBranch => &mut self.default_branch.value,
            ConfigFormFocus::Description => &mut self.description.value,
            ConfigFormFocus::StackHint => &mut self.stack_hint.value,
            ConfigFormFocus::GhUser => &mut self.gh_user.value,
            ConfigFormFocus::Tags => &mut self.tags.value,
        }
    }
}

/// Diff one clearable field, pushing a `Set`/`Unset` edit when its trimmed
/// buffer differs from its loaded original.
///
/// Why: shared by `description`/`stack_hint`/`gh_user` in
/// [`ConfigFormView::diff_edits`] — the three fields share IDENTICAL
/// set-vs-clear-vs-unchanged logic, differing only in which [`ConfigField`]/
/// [`ClearableField`] pair they target.
fn push_clearable_edit(
    edits: &mut Vec<ConfigEdit>,
    set_field: ConfigField,
    clear_field: ClearableField,
    field: &ConfigFormField,
) {
    let trimmed = field.value.trim();
    let original = field.original.as_deref().unwrap_or("");
    if trimmed == original {
        return;
    }
    if trimmed.is_empty() {
        edits.push(ConfigEdit::Unset(clear_field));
    } else {
        edits.push(ConfigEdit::Set(set_field, trimmed.to_string()));
    }
}

impl ProjectCtlState {
    /// Open the Deliverable/Milestone view for the given project (`v` in the
    /// Projects pane, DOC-35 §10.8 `show`, #2383).
    ///
    /// Why: the single seam [`super::super::events`]'s `v` handler calls; seeds
    /// the view with whatever `deliverables` the last poll already fetched (no
    /// duplicate call — see [`ProjectCtlState::deliverables`]'s doc) and an
    /// empty `milestones` list that
    /// [`super::super::poll::project_ctl_poll_daemon`] fills in on the next
    /// tick now that the view is open.
    /// What: sets [`ProjectCtlState::deliverable_view`], overriding any prior one.
    /// Test: `super::super::events::tests`.
    pub fn open_deliverable_view(&mut self, project_name: impl Into<String>) {
        self.deliverable_view = Some(DeliverableView {
            project_name: project_name.into(),
            deliverables: self.deliverables.clone().unwrap_or_default(),
            milestones: Vec::new(),
            scroll: 0,
        });
    }

    /// Close the Deliverable/Milestone view (`Esc`/`v` while it is open).
    pub fn close_deliverable_view(&mut self) {
        self.deliverable_view = None;
    }

    /// Scroll the open Deliverable/Milestone view by `delta` lines, floored
    /// at zero (no known-max clamp — the render layer clamps visually).
    pub fn scroll_deliverable_view(&mut self, delta: i16) {
        if let Some(view) = &mut self.deliverable_view {
            view.scroll = view.scroll.saturating_add_signed(delta);
        }
    }

    /// Open the config form for the given project (`c` in the Projects pane,
    /// DOC-35 §6, #2120).
    ///
    /// Why: the single seam [`super::super::events`]'s `c` handler calls
    /// directly (a pure state mutation, not an async `PendingAction` — see
    /// that module's doc for why). Seeds every field from
    /// [`ProjectCtlState::projects_full`], the full record the last poll
    /// fetched.
    /// What: `Some(project)` → opens [`ConfigFormView::from_project`],
    /// overriding any prior form. `None` (not yet polled, or the project
    /// vanished from the registry between polls) → sets an explanatory
    /// notice and opens nothing, mirroring `v`'s no-selection guard.
    /// Test: `super::super::events::tests`.
    pub fn open_config_form(&mut self, project_name: impl Into<String>) {
        let name = project_name.into();
        match self.projects_full.get(&name) {
            Some(project) => self.config_form = Some(ConfigFormView::from_project(project)),
            None => self.set_notice(format!(
                "no config loaded yet for '{name}' — try again after the next refresh"
            )),
        }
    }

    /// Close the config form, discarding any unsaved edits (`Esc` while open,
    /// or after a successful submit).
    pub fn close_config_form(&mut self) {
        self.config_form = None;
    }

    /// Cycle the config form's focused row forward (Tab).
    pub fn config_form_focus_next(&mut self) {
        if let Some(form) = &mut self.config_form {
            form.focus = form.focus.next();
        }
    }

    /// Cycle the config form's focused row backward (Shift+Tab).
    pub fn config_form_focus_prev(&mut self) {
        if let Some(form) = &mut self.config_form {
            form.focus = form.focus.prev();
        }
    }

    /// Append one character to the focused row's editable buffer.
    pub fn config_form_push_char(&mut self, c: char) {
        if let Some(form) = &mut self.config_form {
            form.push_char(c);
        }
    }

    /// Remove the last character from the focused row's editable buffer.
    pub fn config_form_backspace(&mut self) {
        if let Some(form) = &mut self.config_form {
            form.backspace();
        }
    }

    /// Set the config form's inline error (rendered IN the form, not a
    /// transient toast — explicit #2120 requirement so a rejected submit
    /// never loses the operator's other unsaved edits).
    pub fn set_config_form_error(&mut self, msg: impl Into<String>) {
        if let Some(form) = &mut self.config_form {
            form.error = Some(msg.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        Project {
            name: "widget".to_string(),
            repo_url: "https://github.com/acme/widget".to_string(),
            default_branch: "main".to_string(),
            stack_hint: Some("rust".to_string()),
            tags: vec!["backend".to_string(), "oss".to_string()],
            description: Some("the widget".to_string()),
            gh_user: Some("acme-bot".to_string()),
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
        }
    }

    #[test]
    fn config_form_seeds_from_project() {
        let form = ConfigFormView::from_project(&project());
        assert_eq!(form.default_branch.value, "main");
        assert_eq!(form.description.value, "the widget");
        assert_eq!(form.stack_hint.value, "rust");
        assert_eq!(form.gh_user.value, "acme-bot");
        assert_eq!(form.tags.value, "backend, oss");
        assert!(form.diff_edits().is_empty(), "unedited form has no diff");
    }

    #[test]
    fn config_form_diff_detects_set_and_unset() {
        let mut form = ConfigFormView::from_project(&project());
        form.stack_hint.value = "python".to_string();
        form.description.value = String::new(); // cleared
        let edits = form.diff_edits();
        assert!(edits.contains(&ConfigEdit::Set(
            ConfigField::StackHint,
            "python".to_string()
        )));
        assert!(edits.contains(&ConfigEdit::Unset(ClearableField::Description)));
    }

    #[test]
    fn config_form_blanking_default_branch_is_a_noop_not_a_set() {
        let mut form = ConfigFormView::from_project(&project());
        form.default_branch.value = "   ".to_string();
        assert!(
            form.diff_edits().is_empty(),
            "default_branch has no clear story — a blanked buffer must be skipped, not sent"
        );
    }

    #[test]
    fn config_form_tags_diff_computes_add_and_remove() {
        let mut form = ConfigFormView::from_project(&project());
        form.tags.value = "backend, ml".to_string(); // dropped "oss", added "ml"
        let edits = form.diff_edits();
        assert_eq!(
            edits,
            vec![ConfigEdit::Tags {
                add: vec!["ml".to_string()],
                remove: vec!["oss".to_string()],
            }]
        );
    }

    #[test]
    fn config_form_focus_cycles_through_all_five_rows() {
        let mut focus = ConfigFormFocus::default();
        let mut seen = vec![focus];
        for _ in 0..4 {
            focus = focus.next();
            seen.push(focus);
        }
        assert_eq!(focus.next(), ConfigFormFocus::DefaultBranch);
        assert_eq!(seen.len(), 5);
        assert_eq!(seen[4].next(), ConfigFormFocus::DefaultBranch);
        // prev() is next()'s exact inverse.
        for f in seen {
            assert_eq!(f.next().prev(), f);
        }
    }

    #[test]
    fn state_open_close_config_form() {
        let mut state = ProjectCtlState {
            projects_full: [("widget".to_string(), project())].into_iter().collect(),
            ..Default::default()
        };
        state.open_config_form("widget");
        assert!(state.config_form.is_some());
        assert_eq!(state.config_form.as_ref().unwrap().project_name, "widget");
        state.close_config_form();
        assert!(state.config_form.is_none());
    }

    #[test]
    fn state_open_config_form_without_a_loaded_project_sets_notice() {
        let mut state = ProjectCtlState::default();
        state.open_config_form("ghost");
        assert!(state.config_form.is_none());
        assert!(state.notice.as_deref().unwrap_or("").contains("ghost"));
    }

    #[test]
    fn state_push_char_and_backspace_edit_the_focused_field() {
        let mut state = ProjectCtlState {
            projects_full: [("widget".to_string(), project())].into_iter().collect(),
            ..Default::default()
        };
        state.open_config_form("widget");
        // Focus starts on default_branch.
        state.config_form_push_char('!');
        assert_eq!(
            state.config_form.as_ref().unwrap().default_branch.value,
            "main!"
        );
        state.config_form_backspace();
        assert_eq!(
            state.config_form.as_ref().unwrap().default_branch.value,
            "main"
        );

        state.config_form_focus_next();
        state.config_form_push_char('x');
        assert_eq!(
            state.config_form.as_ref().unwrap().description.value,
            "the widgetx"
        );
    }

    #[test]
    fn state_set_config_form_error_is_inline_not_a_notice() {
        let mut state = ProjectCtlState {
            projects_full: [("widget".to_string(), project())].into_iter().collect(),
            ..Default::default()
        };
        state.open_config_form("widget");
        state.set_config_form_error("project name is the identity key");
        assert_eq!(
            state.config_form.as_ref().unwrap().error.as_deref(),
            Some("project name is the identity key")
        );
        // The form stays open — an error never discards unsaved edits.
        assert!(state.config_form.is_some());
    }

    /// A baseline project whose config fields are all DISTINCT from every
    /// value the shared cases (`config_edit_cases`) set — except `tags`,
    /// which deliberately starts as `["oss"]` so the shared "tags add and
    /// remove together" case's `remove: ["oss"]` has something to remove
    /// while `add: ["ml"]` has something new to add. Kept separate from
    /// [`project`] (used by the OTHER tests in this module) because those
    /// pick values that would collide with a case's target and mask a real
    /// diff bug (e.g. setting `stack_hint` to a value equal to its own
    /// original would produce no edit at all).
    fn shared_case_baseline_project() -> Project {
        Project {
            name: "widget".to_string(),
            repo_url: "https://github.com/acme/widget".to_string(),
            default_branch: "baseline-branch".to_string(),
            stack_hint: Some("baseline-hint".to_string()),
            tags: vec!["oss".to_string()],
            description: Some("baseline desc".to_string()),
            gh_user: Some("baseline-user".to_string()),
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
        }
    }

    /// #2120 issue requirement: "one shared validation/persistence test suite
    /// exercised from both a CLI integration test and a TUI form unit test" —
    /// see `crate::project_config`'s module doc for the full design. This is
    /// the TUI half: seed a form from a baseline project, mutate exactly the
    /// field(s) the case's edit implies, run the form's OWN `diff_edits`, and
    /// assert the result matches the shared table via
    /// `crate::project_config::assert_matches_case` — the same comparison the
    /// CLI test uses, so the wire-shape assertion itself cannot drift between
    /// the two front ends.
    #[test]
    fn config_form_shared_cases_match_tui_diff() {
        use crate::project_config::{config_edit_cases, merge_patch_args};

        for case in config_edit_cases() {
            let mut form = ConfigFormView::from_project(&shared_case_baseline_project());
            apply_case_edit_to_form(&mut form, &case.edit);
            let edits = form.diff_edits();
            let args = merge_patch_args(&edits);
            crate::project_config::assert_matches_case(&args, &case);
        }
    }

    /// Mutate `form`'s buffers so [`ConfigFormView::diff_edits`] will produce
    /// (at least) `edit` — the TUI-side counterpart to
    /// `tests_projects.rs::argv_for_case` on the CLI side.
    fn apply_case_edit_to_form(form: &mut ConfigFormView, edit: &ConfigEdit) {
        match edit {
            ConfigEdit::Set(ConfigField::DefaultBranch, v) => form.default_branch.value = v.clone(),
            ConfigEdit::Set(ConfigField::Description, v) => form.description.value = v.clone(),
            ConfigEdit::Set(ConfigField::StackHint, v) => form.stack_hint.value = v.clone(),
            ConfigEdit::Set(ConfigField::GhUser, v) => form.gh_user.value = v.clone(),
            ConfigEdit::Unset(ClearableField::Description) => form.description.value.clear(),
            ConfigEdit::Unset(ClearableField::StackHint) => form.stack_hint.value.clear(),
            ConfigEdit::Unset(ClearableField::GhUser) => form.gh_user.value.clear(),
            ConfigEdit::Tags { add, remove } => {
                let mut current = form.tags.original.clone();
                current.retain(|t| !remove.contains(t));
                current.extend(add.iter().cloned());
                form.tags.value = current.join(", ");
            }
        }
    }
}
