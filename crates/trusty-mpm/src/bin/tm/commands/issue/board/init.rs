//! `tm issue board init` — create a conformant board from nothing (#10276, AC3).
//!
//! Why: O1 — a repo adopting the workflow gets the standard board through the
//! API, never by copying Project #45 (D5).
//! What: [`init`] reads first — the owner's projects (a same-title one
//! refuses), the repo's linked projects (any refuses), and whether
//! `issue-state.yaml` has uncommitted changes (refuses). Without `apply` it
//! stops there and prints the plan: reads only. With `apply` it creates the
//! project, reconciles it to the spec (the three default Status options are
//! replaced, Hold and the dates and the views are created, the repo linked,
//! the README written), deletes GitHub's default view, and returns the
//! `project:` block to commit.
//! Test: `init_dry_run_makes_only_reads_10276`,
//! `init_apply_refuses_a_same_title_project_10276`,
//! `init_apply_refuses_a_repo_linked_project_10276`,
//! `init_apply_refuses_a_dirty_issue_state_yaml_10276`,
//! `init_apply_creates_the_board_and_prints_a_parseable_block_10276`.

use std::fmt::Write as _;

use super::client::BoardClient;
use super::error::BoardError;
use super::project::ProjectBlock;
use super::readme::manual_steps;
use super::reconcile::{data_type, desired_options, execute, layout_name, plan_board};
use super::snapshot::{owner_node, owner_projects, read_board, repo_projects};
use super::spec::BoardSpec;

/// The `init` inputs.
#[derive(Debug, Clone)]
pub(crate) struct InitOptions {
    /// Owner login (user or organisation).
    pub(crate) owner: String,
    /// `owner/repo` to link.
    pub(crate) repo: String,
    /// Project title.
    pub(crate) title: String,
    /// Status options this repo's board omits.
    pub(crate) drop_options: Vec<String>,
    /// Create the board; without it, plan only.
    pub(crate) apply: bool,
}

/// What `init` did, and the text it prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitReport {
    /// The created project number (`None` on a dry run).
    pub(crate) number: Option<u64>,
    /// The printed report, ending with the `project:` block on success.
    pub(crate) text: String,
}

/// The `project:` block for `issue-state.yaml`.
pub(crate) fn block_yaml(block: &ProjectBlock) -> String {
    let mut out = format!(
        "project:\n  owner: {}\n  number: {}\n",
        block.owner, block.number
    );
    if block.drop_options.is_empty() {
        out.push_str("  drop_options: []\n");
    } else {
        out.push_str("  drop_options:\n");
        for o in &block.drop_options {
            let _ = writeln!(out, "    - {}", yaml_scalar(o));
        }
    }
    out.push_str("  extra_fields: []\n");
    out
}

/// Quote a scalar that YAML would otherwise misread (`Won't Do`, `On: x`).
fn yaml_scalar(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || c == ' ' || c == '-')
    {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// `git status --porcelain -- issue-state.yaml` at the repo root, non-empty.
fn issue_state_dirty(client: &BoardClient<'_>) -> Result<Option<String>, BoardError> {
    let top = client.run("git", &["rev-parse", "--show-toplevel"], "git rev-parse")?;
    let top = top.trim();
    let status = client.run(
        "git",
        &["-C", top, "status", "--porcelain", "--", "issue-state.yaml"],
        "git status --porcelain",
    )?;
    Ok((!status.trim().is_empty()).then(|| format!("{top}/issue-state.yaml")))
}

/// Run `init`; see the module doc.
pub(crate) fn init(
    client: &BoardClient<'_>,
    spec: &BoardSpec,
    opts: &InitOptions,
) -> Result<InitReport, BoardError> {
    if let Some(p) = owner_projects(client, &opts.owner)?
        .into_iter()
        .find(|p| p.title == opts.title)
    {
        return Err(BoardError::SameTitle {
            owner: opts.owner.clone(),
            title: opts.title.clone(),
            number: p.number,
        });
    }
    let linked = repo_projects(client, &opts.repo)?;
    if !linked.is_empty() {
        return Err(BoardError::RepoLinked {
            repo: opts.repo.clone(),
            numbers: linked.iter().map(|p| p.number).collect(),
        });
    }
    if let Some(path) = issue_state_dirty(client)? {
        return Err(BoardError::DirtyIssueState { path });
    }
    let mut text = String::new();
    if !opts.apply {
        text.push_str(&dry_run_plan(spec, opts));
        return Ok(InitReport { number: None, text });
    }
    let owner_id = owner_node(client, &opts.owner)?;
    let (_, number) = client.create_project(&owner_id, &opts.title)?;
    let board = read_board(client, &opts.owner, number)?;
    let defaults: Vec<String> = board.views.iter().map(|v| v.id.clone()).collect();
    let plan = plan_board(client, spec, &opts.drop_options, &board, &opts.repo)?;
    let writes = execute(
        client,
        spec,
        &opts.drop_options,
        &board,
        (&opts.owner, &opts.repo),
        &plan,
    )?;
    // #10276: GitHub gives a new project a default "View 1"; the spec's views
    // replace it. Only views that existed before this run's creates go.
    for id in &defaults {
        client.delete_view(id)?;
    }
    let block = ProjectBlock {
        owner: opts.owner.clone(),
        number,
        drop_options: opts.drop_options.clone(),
        extra_fields: Vec::new(),
    };
    let _ = writeln!(
        text,
        "created project #{number} \"{}\" for {} ({} change(s)):",
        opts.title,
        opts.repo,
        writes + 1 + defaults.len()
    );
    for step in &plan.steps {
        let _ = writeln!(text, "  + {}", step.describe());
    }
    text.push_str(&manual_text(spec));
    text.push_str("\ncommit this block to the repo's issue-state.yaml through a PR:\n\n");
    text.push_str(&block_yaml(&block));
    Ok(InitReport {
        number: Some(number),
        text,
    })
}

/// The plan a dry run prints, from the spec alone.
fn dry_run_plan(spec: &BoardSpec, opts: &InitOptions) -> String {
    let mut out = format!(
        "[dry-run] would create project \"{}\" for {} owned by {}:\n",
        opts.title, opts.repo, opts.owner
    );
    for field in &spec.fields {
        let options: Vec<&str> = desired_options(field, &opts.drop_options)
            .iter()
            .map(|o| o.name.as_str())
            .collect();
        let verb = if field.name == super::validate::STATUS_FIELD {
            "replace the default Status options with"
        } else {
            "create"
        };
        let _ = write!(
            out,
            "  + {verb} `{}` ({})",
            field.name,
            data_type(field.kind)
        );
        if !options.is_empty() {
            let _ = write!(out, " [{}]", options.join(", "));
        }
        if !field.enabled {
            out.push_str(" — declared off");
        }
        out.push('\n');
    }
    for view in &spec.views {
        let _ = writeln!(
            out,
            "  + create view `{}` ({})",
            view.name,
            layout_name(view.layout)
        );
    }
    let _ = writeln!(out, "  + link {}\n  + write the board README", opts.repo);
    out.push_str(&manual_text(spec));
    out.push_str("\nrerun with --apply to create it\n");
    out
}

/// The manual auto-add step and the other settings the API cannot make.
fn manual_text(spec: &BoardSpec) -> String {
    let mut out = String::from(
        "\nmanual steps (the API cannot make these; the board README lists them too):\n",
    );
    for step in manual_steps(spec) {
        let _ = writeln!(out, "  - {step}");
    }
    out.push_str(
        "  - alternative to the auto-add workflow: the `actions/add-to-project` Action, with a \
         stored PAT or App token (the workflow's own token cannot write Projects)\n",
    );
    out
}
