//! The board README and its manual checklist (#10276, owner decision 1).
//!
//! Why: the Phase 2 trial showed the API cannot set a Roadmap view's visible
//! fields, any group-by or sort, or a built-in workflow. Those settings go in
//! a checklist in the board README, and `check` reports each as information,
//! not drift.
//! What: [`manual_steps`] lists those settings for a spec; [`render_readme`]
//! fills the spec's README template (`{{repo}}`, `{{status_table}}`) and
//! appends the checklist.
//! Test: `init_apply_creates_the_board_and_prints_a_parseable_block_10276`,
//! `manual_steps_list_roadmap_fields_and_workflows_but_not_board_status_grouping_10276`.

use super::spec::{BoardSpec, ViewLayout};
use super::validate::STATUS_FIELD;

/// One setting the API cannot make, as a checklist line.
///
/// What: for each Roadmap view with visible fields, the fields to show; for
/// each view's `vertical_group_by`, the field to group by — except a BOARD
/// view grouped by Status, which GitHub does on its own; for each workflow
/// the spec turns on, the workflow to enable.
pub(crate) fn manual_steps(spec: &BoardSpec) -> Vec<String> {
    let mut steps = Vec::new();
    for view in &spec.views {
        if view.layout == ViewLayout::Roadmap && !view.fields.is_empty() {
            steps.push(format!(
                "View `{}`: show the fields {} (Roadmap views take no visible fields through the API).",
                view.name,
                view.fields.join(", ")
            ));
        }
        if let Some(group) = &view.vertical_group_by {
            // Owner decision 1: a BOARD view groups by Status on its own.
            let automatic = view.layout == ViewLayout::Board && group == STATUS_FIELD;
            if !automatic {
                steps.push(format!(
                    "View `{}`: group by `{group}` (the API cannot set a group-by).",
                    view.name
                ));
            }
        }
    }
    for workflow in spec.workflows.iter().filter(|w| w.enabled) {
        steps.push(format!(
            "Workflows: turn on `{}`. {}",
            workflow.name, workflow.description
        ));
    }
    steps
}

/// The README for a board of `repo`, Status options minus `drop_options`.
pub(crate) fn render_readme(spec: &BoardSpec, repo: &str, drop_options: &[String]) -> String {
    let mut table = String::from("| Status | Meaning |\n|---|---|");
    if let Some(status) = spec.field(STATUS_FIELD) {
        for option in status
            .options
            .iter()
            .filter(|o| !drop_options.contains(&o.name))
        {
            table.push_str(&format!("\n| {} | {} |", option.name, option.description));
        }
    }
    let mut readme = spec
        .readme
        .replace("{{repo}}", repo)
        .replace("{{status_table}}", &table);
    let steps = manual_steps(spec);
    if !steps.is_empty() {
        if !readme.ends_with('\n') {
            readme.push('\n');
        }
        readme.push_str(
            "\n## Manual setup\n\nThe GitHub API cannot make these settings; make them in the \
             board's UI.\n\n",
        );
        for step in steps {
            readme.push_str(&format!("- [ ] {step}\n"));
        }
    }
    readme
}
