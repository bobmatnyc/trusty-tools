//! `tm issue next` — the factory's default intake.
//!
//! Why: owner rulings — a factory session chooses its next issue by the
//! repo's status signals (board Status or the existing labels), ranked by the
//! session's goal, then priority, then age; it never takes an issue without
//! acceptance criteria or one someone else holds.
//! What: [`run`] reads the repo snapshot ([`fetch::cached_snapshot`]),
//! detects the label mapping ([`model::LabelMap::detect`], with the optional
//! override in `~/.trusty-tools/trusty-mpm/issue-next.yaml`), ranks
//! ([`model::rank`]) and prints text or JSON. Design and mapping:
//! `docs/reference/issue-next.md`.
//! Test: `next/tests.rs`.

pub(crate) mod fetch;
pub(crate) mod goals;
pub(crate) mod model;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::commands::issue::board::client::BoardClient;
use crate::commands::issue::board::verbs::repo_slug;
use crate::commands::ticket::runner::CommandRunner;
use model::{LabelMap, LabelOverride, Outcome, RepoSignal};

/// A `tm issue next` failure. Every arm exits non-zero.
#[derive(Debug, thiserror::Error)]
pub(crate) enum NextError {
    /// GitHub refused on its rate limit; nothing is returned.
    #[error("GitHub rate limit reached; no issues returned (check `gh api rate_limit`): {detail}")]
    RateLimited {
        /// The refusal text.
        detail: String,
    },
    /// Any other read failure.
    #[error("intake read failed; no issues returned: {detail}")]
    Fetch {
        /// The failure text.
        detail: String,
    },
    /// The goals file or the goal section could not be read.
    #[error("goal lookup failed: {detail}")]
    Goals {
        /// What was wrong.
        detail: String,
    },
    /// The per-repo override file is malformed.
    #[error("label override file {path} is invalid: {detail}")]
    Override {
        /// The file.
        path: String,
        /// The parse error.
        detail: String,
    },
}

/// The override file: `repos: { owner/repo: LabelOverride }`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideFile {
    #[serde(default)]
    repos: BTreeMap<String, LabelOverride>,
}

/// `~/.trusty-tools/trusty-mpm/issue-next.yaml`.
fn override_path() -> Option<PathBuf> {
    let base = crate::commands::issue::config::user_config_path()?;
    Some(base.parent()?.join("issue-next.yaml"))
}

/// `repo`'s entry of the override file at `path`; an absent file is `None`.
///
/// Test: `a_malformed_override_file_is_an_error`.
pub(crate) fn load_override(path: &Path, repo: &str) -> Result<Option<LabelOverride>, NextError> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let file: OverrideFile = serde_yaml::from_str(&text).map_err(|e| NextError::Override {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    Ok(file
        .repos
        .into_iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(repo))
        .map(|(_, v)| v))
}

/// Parsed `tm issue next` flags.
pub(crate) struct NextArgs {
    /// `owner/repo`; the cwd's repo when absent.
    pub(crate) repo: Option<String>,
    /// Goal id ranked first.
    pub(crate) goal: Option<String>,
    /// Most picks to print.
    pub(crate) limit: usize,
    /// JSON output.
    pub(crate) json: bool,
    /// Skip the cache read and rewrite the cache from a fresh fetch.
    pub(crate) refresh: bool,
}

/// `tm issue next` entry point.
///
/// Why: one read-only command the factory sessions call for their next issue.
/// What: resolves the repo and goal, reads the cached snapshot, ranks, and
/// prints. A read failure (rate limit included) is an error, so the command
/// exits non-zero and prints no issue list.
/// Test: the parts are tested in `next/tests.rs`; this is glue.
pub(crate) fn run(runner: &dyn CommandRunner, args: NextArgs) -> anyhow::Result<()> {
    let client = BoardClient::new(runner);
    let repo = repo_slug(&client, args.repo.as_deref())?;
    let goal = match &args.goal {
        Some(id) => {
            let path = goals::goals_path().ok_or_else(|| NextError::Goals {
                detail: "no home directory to find goals.md".into(),
            })?;
            Some(goals::load_goal(&path, id)?)
        }
        None => None,
    };
    let ovr = match override_path() {
        Some(p) => load_override(&p, &repo)?,
        None => None,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let source = fetch::GhSource { client };
    let cache = fetch::cache_path(&repo);
    let (snap, cached) =
        fetch::cached_snapshot(&source, &repo, cache.as_deref(), now, args.refresh)?;
    let map = LabelMap::detect(&snap.repo_labels, ovr.as_ref());
    let outcome = model::rank(&snap, &map, goal.as_ref(), args.limit);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        print!("{}", render(&outcome, cached));
    }
    Ok(())
}

/// The text form of an [`Outcome`].
///
/// Test: `the_text_report_names_signal_reason_and_repair`.
pub(crate) fn render(o: &Outcome, cached: bool) -> String {
    let mut s = String::new();
    let from = if cached { " (cached)" } else { "" };
    if o.signal == RepoSignal::None {
        s.push_str(&format!(
            "{}: no intake signal — no linked board and no ready status label; nothing taken{from}\n",
            o.repo
        ));
        return s;
    }
    let goal = o
        .goal
        .as_deref()
        .map_or(String::new(), |g| format!(", goal {g}"));
    s.push_str(&format!(
        "{}: {} ready issue(s) shown, signal {:?}{goal}{from}\n",
        o.repo,
        o.picks.len(),
        o.signal
    ));
    for p in &o.picks {
        let repair = if p.repair_candidate {
            " [repair candidate]"
        } else {
            ""
        };
        s.push_str(&format!("#{} {}{repair}\n", p.number, p.title));
        s.push_str(&format!(
            "    signal: {}\n    reason: {}\n",
            p.signal, p.reason
        ));
    }
    if !o.repair_candidates.is_empty() {
        let list: Vec<String> = o
            .repair_candidates
            .iter()
            .map(|n| format!("#{n}"))
            .collect();
        s.push_str(&format!(
            "label and board disagree (run `tm issue repair`): {}\n",
            list.join(" ")
        ));
    }
    s.push_str(&format!(
        "skipped: {} not ready, {} assigned to others, {} without acceptance criteria\n",
        o.skipped.not_ready,
        o.skipped.assigned_to_others,
        o.skipped.no_acceptance_criteria.len()
    ));
    s
}
