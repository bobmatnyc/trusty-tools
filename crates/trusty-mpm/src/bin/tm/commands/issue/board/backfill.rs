//! `apply --backfill`: put the repo's open issues on the board (#10276, AC4).
//!
//! Why: a board created after the repo has issues starts empty, and auto-add
//! only sees new ones. #9402 later reuses this module for its migration.
//! What: [`backfill`] reads the board's items once, then walks the repo's open
//! issues a page (100) at a time, oldest first. Each issue not on the board is
//! added, with a [`Pacer`] pause between writes. After each page the next
//! cursor is recorded in a [`CursorFile`]; a rerun after a failure resumes
//! from it, and the membership read means no issue is added twice. With
//! `status_from_labels` (D3's explicit flag) — and only then — each open
//! issue's Status is set from its `status:*` label.
//! Test: `backfill_pages_paces_and_resumes_without_adding_twice_10276`,
//! `backfill_writes_status_only_under_the_flag_10276`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::client::BoardClient;
use super::error::BoardError;
use super::snapshot::{Board, OpenIssue, board_items, open_issues_page};
use super::spec::BoardSpec;
use super::validate::STATUS_FIELD;
use crate::commands::issue::config::StateModel;

/// A pause between writes.
///
/// Why: GitHub's secondary rate limit throttles bursts of writes; a test must
/// not sleep.
pub(crate) trait Pacer {
    /// Wait before the next write.
    fn pause(&self);
}

/// A [`Pacer`] that sleeps a fixed interval.
pub(crate) struct SleepPacer(pub(crate) std::time::Duration);

impl Pacer for SleepPacer {
    fn pause(&self) {
        std::thread::sleep(self.0);
    }
}

/// The recorded resume point of one repo's backfill onto one board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    /// `owner/repo`.
    repo: String,
    /// The board's project number.
    number: u64,
    /// The cursor of the next page to read.
    after: String,
}

/// Where the resume point lives.
///
/// What: a JSON file; [`CursorFile::for_board`] puts it under the user config
/// directory, never in the repo.
pub(crate) struct CursorFile(pub(crate) PathBuf);

impl CursorFile {
    /// `~/.trusty-tools/trusty-mpm/board-backfill/<owner>-<number>-<repo>.json`.
    pub(crate) fn for_board(owner: &str, number: u64, repo: &str) -> Option<Self> {
        let base = crate::commands::issue::config::user_config_path()?;
        let dir = base.parent()?.join("board-backfill");
        let name = format!("{owner}-{number}-{}.json", repo.replace('/', "-"));
        Some(Self(dir.join(name)))
    }

    fn err(&self, e: impl std::fmt::Display) -> BoardError {
        BoardError::Cursor {
            path: self.0.display().to_string(),
            detail: e.to_string(),
        }
    }

    fn read(&self, repo: &str, number: u64) -> Result<Option<String>, BoardError> {
        if !self.0.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&self.0).map_err(|e| self.err(e))?;
        let cursor: Cursor = serde_json::from_str(&text).map_err(|e| self.err(e))?;
        Ok((cursor.repo == repo && cursor.number == number).then_some(cursor.after))
    }

    fn write(&self, repo: &str, number: u64, after: &str) -> Result<(), BoardError> {
        if let Some(dir) = self.0.parent() {
            std::fs::create_dir_all(dir).map_err(|e| self.err(e))?;
        }
        let cursor = Cursor {
            repo: repo.to_string(),
            number,
            after: after.to_string(),
        };
        let text = serde_json::to_string(&cursor).map_err(|e| self.err(e))?;
        let tmp = self.0.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| self.err(e))?;
        std::fs::rename(&tmp, &self.0).map_err(|e| self.err(e))
    }

    fn clear(&self) -> Result<(), BoardError> {
        match std::fs::remove_file(&self.0) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(self.err(e)),
        }
    }
}

/// What a backfill did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BackfillReport {
    /// Issues added to the board.
    pub(crate) added: Vec<u64>,
    /// Status writes made (only under the flag).
    pub(crate) status_set: Vec<(u64, String)>,
    /// Issues skipped with the reason.
    pub(crate) skipped: Vec<String>,
    /// Whether the run resumed from a recorded cursor.
    pub(crate) resumed: bool,
    /// `true` when nothing was written (dry run).
    pub(crate) dry_run: bool,
}

/// Everything a backfill run needs besides the client.
pub(crate) struct BackfillRun<'a> {
    /// The spec (its Status options map to states).
    pub(crate) spec: &'a BoardSpec,
    /// The repo's lifecycle (states → labels).
    pub(crate) model: &'a StateModel,
    /// The board.
    pub(crate) board: &'a Board,
    /// `owner/repo`.
    pub(crate) repo: &'a str,
    /// D3: set Status from labels.
    pub(crate) status_from_labels: bool,
    /// Report only.
    pub(crate) dry_run: bool,
    /// The pause between writes.
    pub(crate) pacer: &'a dyn Pacer,
    /// The resume point.
    pub(crate) cursor: &'a CursorFile,
}

/// Run the backfill; see the module doc.
pub(crate) fn backfill(
    client: &BoardClient<'_>,
    run: &BackfillRun<'_>,
) -> Result<BackfillReport, BoardError> {
    let mut report = BackfillReport {
        dry_run: run.dry_run,
        ..BackfillReport::default()
    };
    let mut on_board: BTreeMap<u64, (String, Option<String>)> =
        board_items(client, &run.board.id, STATUS_FIELD)?
            .into_iter()
            .filter_map(|item| match item.issue {
                Some((repo, n)) if repo == run.repo => Some((n, (item.id, item.option_id))),
                _ => None,
            })
            .collect();
    let mut after = run.cursor.read(run.repo, run.board.number)?;
    report.resumed = after.is_some();
    loop {
        let (issues, next) = open_issues_page(client, run.repo, after.as_deref())?;
        for issue in &issues {
            let present = on_board.get(&issue.number).cloned();
            let item = match present {
                Some((id, option)) => Some((id, option)),
                None if run.dry_run => {
                    report.added.push(issue.number);
                    None
                }
                None => {
                    run.pacer.pause();
                    let id = client.add_item(&run.board.id, &issue.id)?;
                    report.added.push(issue.number);
                    on_board.insert(issue.number, (id.clone(), None));
                    Some((id, None))
                }
            };
            // D3: Status is written only under the explicit flag.
            if run.status_from_labels {
                set_status(client, run, issue, item, &mut report)?;
            }
        }
        match next {
            Some(cursor) => {
                if !run.dry_run {
                    run.cursor.write(run.repo, run.board.number, &cursor)?;
                }
                after = Some(cursor);
            }
            None => break,
        }
    }
    if !run.dry_run {
        run.cursor.clear()?;
    }
    Ok(report)
}

/// The Status option an issue's labels map to, by option name.
///
/// What: the option whose `state`'s label the issue carries; with no mapped
/// label, the first Status option (the lifecycle's label-less open state,
/// Backlog in version 1) — written only when the item has no Status, so a
/// hand-set Ready for Development is kept. Two mapped labels is ambiguous.
pub(crate) fn status_for(
    spec: &BoardSpec,
    model: &StateModel,
    labels: &[String],
) -> Result<Option<(String, bool)>, String> {
    let Some(status) = spec.field(STATUS_FIELD) else {
        return Ok(None);
    };
    let mapped: Vec<&str> = status
        .options
        .iter()
        .filter(|o| {
            o.state.as_ref().is_some_and(|state| {
                model
                    .states
                    .iter()
                    .find(|s| &s.name == state)
                    .and_then(|s| s.label.as_ref())
                    .is_some_and(|l| labels.contains(&l.name))
            })
        })
        .map(|o| o.name.as_str())
        .collect();
    match mapped.as_slice() {
        [] => Ok(status.options.first().map(|o| (o.name.clone(), false))),
        [one] => Ok(Some(((*one).to_string(), true))),
        many => Err(format!("carries labels for {}", many.join(" and "))),
    }
}

/// Set one issue's Status from its labels, when it differs.
fn set_status(
    client: &BoardClient<'_>,
    run: &BackfillRun<'_>,
    issue: &OpenIssue,
    item: Option<(String, Option<String>)>,
    report: &mut BackfillReport,
) -> Result<(), BoardError> {
    let Some(field) = run.board.field(STATUS_FIELD) else {
        return Ok(());
    };
    let (name, from_label) = match status_for(run.spec, run.model, &issue.labels) {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(()),
        Err(why) => {
            report
                .skipped
                .push(format!("#{}: Status not set — {why}", issue.number));
            return Ok(());
        }
    };
    let Some(option) = field.options.iter().find(|o| o.name == name) else {
        report.skipped.push(format!(
            "#{}: Status `{name}` is not an option on this board",
            issue.number
        ));
        return Ok(());
    };
    let current = item.as_ref().and_then(|(_, o)| o.clone());
    let keep_manual = !from_label && current.is_some();
    if current.as_deref() == Some(option.id.as_str()) || keep_manual {
        return Ok(());
    }
    if let Some((item_id, _)) = item {
        run.pacer.pause();
        client.set_option(&run.board.id, &item_id, &field.id, &option.id)?;
    }
    report.status_set.push((issue.number, name));
    Ok(())
}
