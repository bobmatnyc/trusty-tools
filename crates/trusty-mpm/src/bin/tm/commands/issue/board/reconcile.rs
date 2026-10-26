//! Reconcile a live board to the spec: plan, then execute (#10276).
//!
//! Why: `init` and `apply` make the same changes — standard fields, their
//! options, missing views, the repo link, the README — so one planner decides
//! them from a [`Board`] snapshot and one executor makes them. A plan with no
//! steps is what makes a second `apply` a no-op (AC4).
//! What: [`plan_board`] diffs the spec (minus the repo's `drop_options`)
//! against a snapshot and refuses, before any write, to remove an option an
//! item still uses (owner decision 2). Every option update sends all kept
//! options with their ids. [`execute`] runs the steps: fields first, then a
//! fresh read for field ids, then views, the repo link and the README.
//! Test: `apply_twice_records_no_mutation_on_the_second_run_10276`,
//! `apply_refuses_to_remove_an_option_items_use_naming_it_and_the_count_10276`,
//! `apply_sends_every_existing_option_id_on_an_option_edit_10276`,
//! `apply_dry_run_makes_no_mutation_10276`.

use std::collections::BTreeMap;

use super::client::{BoardClient, OptionInput, color_name};
use super::error::BoardError;
use super::readme::render_readme;
use super::snapshot::{Board, BoardField, board_items, read_board, repo_node};
use super::spec::{BoardSpec, FieldKind, FieldSpec, ViewLayout};
use super::validate::STATUS_FIELD;

/// One change the reconcile makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// Replace a single-select field's option list.
    UpdateOptions {
        /// Field name.
        field: String,
        /// Field node id.
        field_id: String,
        /// The full list, every kept option carrying its id.
        options: Vec<OptionInput>,
    },
    /// Create a standard field.
    CreateField {
        /// Field name.
        name: String,
        /// `SINGLE_SELECT` or `DATE`.
        data_type: &'static str,
        /// Options of a single-select.
        options: Vec<OptionInput>,
    },
    /// Create a view the board lacks, by name.
    CreateView {
        /// View name.
        name: String,
        /// GitHub layout enum.
        layout: &'static str,
        /// Visible field names (empty for a Roadmap).
        fields: Vec<String>,
        /// Filter, empty for none.
        filter: String,
    },
    /// Link the repo.
    LinkRepo {
        /// `owner/name`.
        repo: String,
    },
    /// Write the README (the board has none).
    SetReadme,
}

impl Step {
    /// One line naming the change.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::UpdateOptions { field, options, .. } => {
                let names: Vec<&str> = options.iter().map(|o| o.name.as_str()).collect();
                format!("set `{field}` options to [{}]", names.join(", "))
            }
            Self::CreateField {
                name, data_type, ..
            } => format!("create field `{name}` ({data_type})"),
            Self::CreateView { name, layout, .. } => format!("create view `{name}` ({layout})"),
            Self::LinkRepo { repo } => format!("link `{repo}`"),
            Self::SetReadme => "write the board README".to_string(),
        }
    }
}

/// What a reconcile would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The changes, in execution order.
    pub(crate) steps: Vec<Step>,
    /// Differences the reconcile reports but does not make.
    pub(crate) notes: Vec<String>,
}

/// The GitHub layout enum of a spec layout.
pub(crate) fn layout_name(layout: ViewLayout) -> &'static str {
    match layout {
        ViewLayout::Table => "TABLE_LAYOUT",
        ViewLayout::Board => "BOARD_LAYOUT",
        ViewLayout::Roadmap => "ROADMAP_LAYOUT",
    }
}

/// The GitHub data type of a spec field kind.
pub(crate) fn data_type(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::SingleSelect => "SINGLE_SELECT",
        FieldKind::Date => "DATE",
    }
}

/// The options a board of this repo carries for `field`: the spec's, minus
/// `drop_options` on Status (D1: names are the keys).
pub(crate) fn desired_options<'s>(
    field: &'s FieldSpec,
    drop_options: &[String],
) -> Vec<&'s super::spec::OptionSpec> {
    field
        .options
        .iter()
        .filter(|o| field.name != STATUS_FIELD || !drop_options.contains(&o.name))
        .collect()
}

/// Plan the reconcile of `board` to `spec` for `repo`.
///
/// What: per standard field, create it when missing, refuse a type mismatch,
/// or rewrite its options when any name, color, description or order
/// differs — reading item usage only when the rewrite would remove an
/// option, and refusing with [`BoardError::OptionInUse`] when an item uses
/// one. Then each spec view missing by name, the repo link when absent, and
/// the README when the board has none. An existing view that differs is a
/// note, not a step.
pub(crate) fn plan_board(
    client: &BoardClient<'_>,
    spec: &BoardSpec,
    drop_options: &[String],
    board: &Board,
    repo: &str,
) -> Result<Plan, BoardError> {
    let mut plan = Plan::default();
    for field in &spec.fields {
        match board.field(&field.name) {
            None => plan.steps.push(Step::CreateField {
                name: field.name.clone(),
                data_type: data_type(field.kind),
                options: desired_options(field, drop_options)
                    .iter()
                    .map(|o| new_option(o))
                    .collect(),
            }),
            Some(live) if live.data_type != data_type(field.kind) => {
                return Err(BoardError::FieldTypeMismatch {
                    field: field.name.clone(),
                    expected: data_type(field.kind).to_string(),
                    found: live.data_type.clone(),
                });
            }
            Some(live) if field.kind == FieldKind::SingleSelect => {
                if let Some(step) = plan_options(client, board, field, live, drop_options)? {
                    plan.steps.push(step);
                }
            }
            Some(_) => {}
        }
    }
    for view in &spec.views {
        match board.view(&view.name) {
            None => plan.steps.push(Step::CreateView {
                name: view.name.clone(),
                layout: layout_name(view.layout),
                fields: if view.layout == ViewLayout::Roadmap {
                    Vec::new()
                } else {
                    view.fields.clone()
                },
                filter: view.filter.clone(),
            }),
            Some(live) if live.layout != layout_name(view.layout) => plan.notes.push(format!(
                "view `{}` is {}, the spec says {}; apply does not rewrite an existing view",
                view.name,
                live.layout,
                layout_name(view.layout)
            )),
            Some(_) => {}
        }
    }
    if !board.repositories.iter().any(|r| r == repo) {
        plan.steps.push(Step::LinkRepo {
            repo: repo.to_string(),
        });
    }
    if board.readme.trim().is_empty() {
        plan.steps.push(Step::SetReadme);
    }
    Ok(plan)
}

/// A new option (no id).
fn new_option(o: &super::spec::OptionSpec) -> OptionInput {
    OptionInput {
        id: None,
        name: o.name.clone(),
        color: o.color,
        description: o.description.clone(),
    }
}

/// The option rewrite for one live single-select, or `None` when it already
/// matches.
fn plan_options(
    client: &BoardClient<'_>,
    board: &Board,
    field: &FieldSpec,
    live: &BoardField,
    drop_options: &[String],
) -> Result<Option<Step>, BoardError> {
    let desired = desired_options(field, drop_options);
    // Owner decision 2: an option that exists by name keeps its id.
    let options: Vec<OptionInput> = desired
        .iter()
        .map(|o| OptionInput {
            id: live
                .options
                .iter()
                .find(|l| l.name == o.name)
                .map(|l| l.id.clone()),
            ..new_option(o)
        })
        .collect();
    let same = live.options.len() == options.len()
        && live.options.iter().zip(&options).all(|(l, o)| {
            l.name == o.name && l.color == color_name(o.color) && l.description == o.description
        });
    if same {
        return Ok(None);
    }
    let removed: Vec<&super::snapshot::BoardOption> = live
        .options
        .iter()
        .filter(|l| !desired.iter().any(|d| d.name == l.name))
        .collect();
    if !removed.is_empty() {
        // #10276: removing an option clears every item using it (trial), so
        // the refusal comes before any write.
        let usage = option_usage(client, board, &field.name)?;
        let in_use: Vec<(String, usize)> = removed
            .iter()
            .filter_map(|r| usage.get(&r.id).map(|n| (r.name.clone(), *n)))
            .collect();
        if !in_use.is_empty() {
            return Err(BoardError::OptionInUse {
                field: field.name.clone(),
                in_use,
            });
        }
    }
    Ok(Some(Step::UpdateOptions {
        field: field.name.clone(),
        field_id: live.id.clone(),
        options,
    }))
}

/// Items per option id of `field`; skipped (empty) on a board with no items.
fn option_usage(
    client: &BoardClient<'_>,
    board: &Board,
    field: &str,
) -> Result<BTreeMap<String, usize>, BoardError> {
    let mut usage = BTreeMap::new();
    if board.item_count == 0 {
        return Ok(usage);
    }
    for item in board_items(client, &board.id, field)? {
        if let Some(id) = item.option_id {
            *usage.entry(id).or_insert(0) += 1;
        }
    }
    Ok(usage)
}

/// Run `plan` against `board`; `target` is (owner login, `owner/repo`).
/// Returns how many writes it made.
///
/// What: field steps, then — when a view is to be created — one fresh
/// snapshot so the new fields' ids are known, then the view steps (a filter is
/// a second call, since create takes none), the link and the README.
pub(crate) fn execute(
    client: &BoardClient<'_>,
    spec: &BoardSpec,
    drop_options: &[String],
    board: &Board,
    target: (&str, &str),
    plan: &Plan,
) -> Result<usize, BoardError> {
    let (owner, repo) = target;
    let mut writes = 0;
    for step in &plan.steps {
        match step {
            Step::UpdateOptions {
                field_id, options, ..
            } => client.update_options(field_id, options)?,
            Step::CreateField {
                name,
                data_type,
                options,
            } => client.create_field(&board.id, name, data_type, options)?,
            _ => continue,
        }
        writes += 1;
    }
    let creates_view = plan
        .steps
        .iter()
        .any(|s| matches!(s, Step::CreateView { .. }));
    let fresh = if creates_view && writes > 0 {
        read_board(client, owner, board.number)?
    } else {
        board.clone()
    };
    for step in &plan.steps {
        match step {
            Step::CreateView {
                name,
                layout,
                fields,
                filter,
            } => {
                let visible = (*layout != "ROADMAP_LAYOUT").then(|| field_ids(&fresh, fields));
                let id = client.create_view(&board.id, name, layout, visible.as_deref())?;
                writes += 1;
                if !filter.is_empty() {
                    client.set_view_filter(&id, filter)?;
                    writes += 1;
                }
            }
            Step::LinkRepo { repo } => {
                client.link_repo(&board.id, &repo_node(client, repo)?)?;
                writes += 1;
            }
            Step::SetReadme => {
                client.set_readme(&board.id, &render_readme(spec, repo, drop_options))?;
                writes += 1;
            }
            Step::UpdateOptions { .. } | Step::CreateField { .. } => {}
        }
    }
    Ok(writes)
}

/// The node ids of `names` on `board`, in order; a name the board lacks is
/// skipped (the planner only names standard and built-in fields).
fn field_ids(board: &Board, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter_map(|n| board.field(n).map(|f| f.id.clone()))
        .collect()
}
