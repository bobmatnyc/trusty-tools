//! The `apply`, `check` and `show` verbs over an injected runner (#10276).
//!
//! Why: the CLI glue in `mod.rs` owns the process (identity, config, cwd);
//! these functions own the decisions, so a fake `gh` drives them whole.
//! What: [`apply_verb`] (plan, then execute under `--apply`, then the
//! optional backfill), [`check_verb`] (the read-only report), [`show_text`]
//! (the embedded spec) and [`repo_slug`].
//! Test: `apply_twice_records_no_mutation_on_the_second_run_10276`,
//! `apply_dry_run_makes_no_mutation_10276`,
//! `check_reports_drift_extras_info_and_missing_issues_without_mutating_10276`,
//! `show_prints_the_embedded_spec_and_its_version_10276`.

use std::fmt::Write as _;

use super::backfill::{BackfillRun, CursorFile, Pacer, backfill};
use super::check::{CheckReport, check_board, token_lines};
use super::client::BoardClient;
use super::error::BoardError;
use super::project::ProjectBlock;
use super::reconcile::{execute, plan_board};
use super::snapshot::{board_items, open_issues_page, read_board};
use super::spec::{BOARD_SPEC_YAML, BoardSpec};
use super::validate::STATUS_FIELD;
use crate::commands::issue::config::StateModel;

/// `--repo`, else `gh repo view`'s `nameWithOwner`.
pub(crate) fn repo_slug(
    client: &BoardClient<'_>,
    repo: Option<&str>,
) -> Result<String, BoardError> {
    if let Some(repo) = repo.map(str::trim).filter(|r| !r.is_empty()) {
        return Ok(repo.to_string());
    }
    let out = client.run(
        "gh",
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ],
        "gh repo view",
    )?;
    let slug = out.trim();
    if slug.contains('/') {
        Ok(slug.to_string())
    } else {
        Err(BoardError::Parse {
            what: "gh repo view".into(),
            detail: format!("no owner/name in {slug:?}"),
        })
    }
}

/// The repo's `project:` block, or the refusal naming the fallback.
pub(crate) fn require_block<'m>(
    model: &'m StateModel,
    default_project: Option<&str>,
) -> Result<&'m ProjectBlock, BoardError> {
    model
        .project
        .as_ref()
        .ok_or_else(|| BoardError::NoProjectBlock {
            fallback: default_project.map_or_else(String::new, |t| {
                format!(" (agents.ticketing.default_project `{t}` is only an attach fallback)")
            }),
        })
}

/// The `apply` switches.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ApplyFlags {
    /// Write; without it, plan only.
    pub(crate) apply: bool,
    /// Put the repo's open issues on the board.
    pub(crate) backfill: bool,
    /// D3: set Status from labels during the backfill.
    pub(crate) status_from_labels: bool,
}

/// `tm issue board apply`.
pub(crate) fn apply_verb(
    client: &BoardClient<'_>,
    spec: &BoardSpec,
    model: &StateModel,
    block: &ProjectBlock,
    repo: &str,
    flags: ApplyFlags,
    (pacer, cursor): (&dyn Pacer, &CursorFile),
) -> Result<String, BoardError> {
    let board = read_board(client, &block.owner, block.number)?;
    let plan = plan_board(client, spec, &block.drop_options, &board, repo)?;
    let mut out = String::new();
    let mode = if flags.apply { "" } else { "[dry-run] would " };
    let _ = writeln!(
        out,
        "board {} #{} \"{}\": {} change(s)",
        block.owner,
        block.number,
        board.title,
        plan.steps.len()
    );
    for step in &plan.steps {
        let _ = writeln!(out, "  + {mode}{}", step.describe());
    }
    for note in &plan.notes {
        let _ = writeln!(out, "  note: {note}");
    }
    if flags.apply && !plan.steps.is_empty() {
        let writes = execute(
            client,
            spec,
            &block.drop_options,
            &board,
            (&block.owner, repo),
            &plan,
        )?;
        let _ = writeln!(out, "  applied {writes} write(s)");
    }
    if flags.backfill {
        let board = if flags.apply && !plan.steps.is_empty() {
            read_board(client, &block.owner, block.number)?
        } else {
            board
        };
        let report = backfill(
            client,
            &BackfillRun {
                spec,
                model,
                board: &board,
                repo,
                status_from_labels: flags.status_from_labels,
                dry_run: !flags.apply,
                pacer,
                cursor,
            },
        )?;
        if report.resumed {
            out.push_str("  backfill resumed from the recorded cursor\n");
        }
        let _ = writeln!(
            out,
            "  backfill: {mode}add {} issue(s){}",
            report.added.len(),
            list(&report.added)
        );
        if flags.status_from_labels {
            let set: Vec<String> = report
                .status_set
                .iter()
                .map(|(n, s)| format!("#{n} → {s}"))
                .collect();
            let _ = writeln!(
                out,
                "  status: {mode}set {} [{}]",
                set.len(),
                set.join(", ")
            );
        }
        for skip in &report.skipped {
            let _ = writeln!(out, "  skipped {skip}");
        }
    }
    Ok(out)
}

fn list(numbers: &[u64]) -> String {
    if numbers.is_empty() {
        return String::new();
    }
    let shown: Vec<String> = numbers.iter().map(|n| format!("#{n}")).collect();
    format!(" [{}]", shown.join(", "))
}

/// `tm issue board check` — reads only.
pub(crate) fn check_verb(
    client: &BoardClient<'_>,
    spec: &BoardSpec,
    block: &ProjectBlock,
    default_project: Option<&str>,
    repo: &str,
) -> Result<CheckReport, BoardError> {
    let (token, sso) = match client.probe_token(repo) {
        Ok(probe) => token_lines(&probe),
        Err(e) => (vec![format!("could not read the token headers: {e}")], None),
    };
    if sso.is_some() {
        return Ok(CheckReport {
            board: format!("{} #{} (not read: SSO)", block.owner, block.number),
            token,
            sso_required: sso,
            ..CheckReport::default()
        });
    }
    let board = read_board(client, &block.owner, block.number)?;
    let mut open = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let (page, next) = open_issues_page(client, repo, after.as_deref())?;
        open.extend(page.iter().map(|i| i.number));
        match next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    let on_board: Vec<u64> = board_items(client, &board.id, STATUS_FIELD)?
        .into_iter()
        .filter_map(|i| i.issue.filter(|(r, _)| r == repo).map(|(_, n)| n))
        .collect();
    let mut report = check_board(spec, block, &board, repo, &open, &on_board);
    report.token = token;
    // D8: the block wins; a default_project naming another board is reported.
    if let Some(title) = default_project.map(str::trim).filter(|t| !t.is_empty())
        && title != board.title
    {
        report.mismatch = Some(format!(
            "agents.ticketing.default_project is `{title}`, but the project: block names #{} \
             `{}`; the block wins and default_project is only a fallback for repos without one",
            board.number, board.title
        ));
    }
    Ok(report)
}

/// `tm issue board show`.
pub(crate) fn show_text(spec: &BoardSpec) -> String {
    format!(
        "board spec version {} (embedded in this tm)\n\n{BOARD_SPEC_YAML}",
        spec.version
    )
}
