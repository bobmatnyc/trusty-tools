//! The intake data and the ranking rule of `tm issue next`.
//!
//! Why: the factory takes issues by their status signal (board Status or the
//! repo's existing labels), so the rule that turns a repo snapshot into a
//! ranked pick list is pure and tested without GitHub.
//! What: [`Snapshot`] (what the fetch read), [`LabelMap`] (how the repo's
//! labels map to status, priority and type, detected or overridden),
//! [`rank`] (filter and order), and its [`Outcome`].
//! Test: `next/tests.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use trusty_mpm::core::issue_audit::has_acceptance_criteria;

use super::goals::Goal;

/// One open issue, as the fetch read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IssueRow {
    /// Issue number.
    pub(crate) number: u64,
    /// Title.
    pub(crate) title: String,
    /// `createdAt`, RFC 3339; orders the age tie-break.
    pub(crate) created_at: String,
    /// Body, read for acceptance criteria.
    pub(crate) body: String,
    /// Label names.
    pub(crate) labels: Vec<String>,
    /// Assignee logins.
    pub(crate) assignees: Vec<String>,
}

/// One board linked to the repo and the Status of each of its repo issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BoardStatus {
    /// Project number.
    pub(crate) number: u64,
    /// Issue number to its Status option name; an issue with no Status is absent.
    pub(crate) statuses: BTreeMap<u64, String>,
}

/// Everything one intake read needs, cached as one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    /// `owner/repo`.
    pub(crate) repo: String,
    /// The authenticated login; an issue assigned only to others is skipped.
    pub(crate) viewer: String,
    /// Every label the repo defines.
    pub(crate) repo_labels: Vec<String>,
    /// The open boards linked to the repo.
    pub(crate) boards: Vec<BoardStatus>,
    /// The open issues, oldest first.
    pub(crate) issues: Vec<IssueRow>,
}

/// A per-repo override of the detected mapping. Every field is optional; a
/// present field replaces the detected value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LabelOverride {
    /// Labels that mean ready.
    pub(crate) ready_labels: Option<Vec<String>>,
    /// Labels that mean a status other than ready.
    pub(crate) not_ready_labels: Option<Vec<String>>,
    /// Label to priority rank, 0 highest.
    pub(crate) priority_labels: Option<BTreeMap<String, u8>>,
    /// Board Status options that mean ready.
    pub(crate) ready_status: Option<Vec<String>>,
}

/// How this repo's labels and boards map to the intake signals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LabelMap {
    /// Repo labels that mean ready.
    pub(crate) ready: Vec<String>,
    /// Repo labels that mean another status.
    pub(crate) not_ready: Vec<String>,
    /// Repo label (lowercase) to priority rank, 0 highest.
    pub(crate) priority: BTreeMap<String, u8>,
    /// Board Status options that mean ready.
    pub(crate) ready_status: Vec<String>,
}

/// The default Status options that mean ready (TICKETING.md "Groomed").
pub(crate) const READY_STATUS: [&str; 2] = ["Ready for Development", "Ready"];

/// A status label's suffix that means ready.
const READY_WORDS: [&str; 6] = [
    "ready",
    "ready-for-dev",
    "ready-for-development",
    "ready for development",
    "groomed",
    "todo",
];

/// The status suffix of `label` (`status:ready` → `ready`), or `None`.
fn status_suffix(label: &str) -> Option<String> {
    let lower = label.to_ascii_lowercase();
    ["status:", "status/", "status-"]
        .iter()
        .find_map(|p| lower.strip_prefix(p).map(|s| s.trim().to_string()))
}

/// The priority rank `label` names, 0 highest, or `None`.
///
/// What: `P0`-`P3`, `priority:p0`-`p3`, and `priority:critical|high|medium|low`
/// (also with `/` or `-` after `priority`).
fn priority_rank(label: &str) -> Option<u8> {
    let lower = label.to_ascii_lowercase();
    let word = ["priority:", "priority/", "priority-"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
        .unwrap_or(&lower)
        .trim();
    match word {
        "p0" | "critical" | "urgent" => Some(0),
        "p1" | "high" => Some(1),
        "p2" | "medium" | "normal" => Some(2),
        "p3" | "low" => Some(3),
        _ => None,
    }
}

/// The type `label` names (`bug`, `enhancement`, `type:*`), or `None`.
fn type_name(label: &str) -> Option<String> {
    let lower = label.to_ascii_lowercase();
    if let Some(t) = ["type:", "type/", "kind:", "kind/"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
    {
        return Some(t.trim().to_string());
    }
    ["bug", "enhancement", "feature", "documentation", "chore"]
        .contains(&lower.as_str())
        .then_some(lower)
}

impl LabelMap {
    /// Detect the mapping from the repo's existing labels, then apply `ovr`.
    ///
    /// Why: owner ruling — intake reads the repo's existing conventions and
    /// creates no label.
    /// What: a `status:*` label whose suffix is a ready word is a ready label;
    /// every other `status:*` label is a not-ready label; `P0`-`P3` and
    /// `priority:*` labels are priorities.
    /// Test: `labels_are_detected_from_repo_conventions`,
    /// `a_repo_override_replaces_the_detected_mapping`.
    pub(crate) fn detect(repo_labels: &[String], ovr: Option<&LabelOverride>) -> Self {
        let mut map = Self {
            ready: Vec::new(),
            not_ready: Vec::new(),
            priority: BTreeMap::new(),
            ready_status: READY_STATUS.iter().map(|s| s.to_string()).collect(),
        };
        for label in repo_labels {
            if let Some(suffix) = status_suffix(label) {
                if READY_WORDS.contains(&suffix.as_str()) {
                    map.ready.push(label.clone());
                } else {
                    map.not_ready.push(label.clone());
                }
            } else if let Some(rank) = priority_rank(label) {
                map.priority.insert(label.to_ascii_lowercase(), rank);
            }
        }
        let Some(ovr) = ovr else { return map };
        if let Some(v) = &ovr.ready_labels {
            map.ready = v.clone();
        }
        if let Some(v) = &ovr.not_ready_labels {
            map.not_ready = v.clone();
        }
        if let Some(v) = &ovr.priority_labels {
            map.priority = v
                .iter()
                .map(|(k, r)| (k.to_ascii_lowercase(), *r))
                .collect();
        }
        if let Some(v) = &ovr.ready_status {
            map.ready_status = v.clone();
        }
        map
    }

    fn is_ready_label(&self, label: &str) -> bool {
        self.ready.iter().any(|r| r.eq_ignore_ascii_case(label))
    }

    fn is_not_ready_label(&self, label: &str) -> bool {
        self.not_ready.iter().any(|r| r.eq_ignore_ascii_case(label))
    }

    fn is_ready_status(&self, status: &str) -> bool {
        self.ready_status
            .iter()
            .any(|r| r.eq_ignore_ascii_case(status))
    }

    /// The best (lowest) priority rank among `labels`; unprioritized is 4.
    fn priority_of(&self, labels: &[String]) -> u8 {
        labels
            .iter()
            .filter_map(|l| self.priority.get(&l.to_ascii_lowercase()).copied())
            .min()
            .unwrap_or(4)
    }
}

/// Which signal the repo offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RepoSignal {
    /// A linked board and ready labels.
    BoardAndLabels,
    /// A linked board only.
    Board,
    /// Ready labels only.
    Labels,
    /// Neither: nothing is taken.
    None,
}

/// One issue `tm issue next` picked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Pick {
    /// Issue number.
    pub(crate) number: u64,
    /// Title.
    pub(crate) title: String,
    /// The signal that made it ready: `board Status: <option>`, `label <name>`,
    /// or both joined by ` + `.
    pub(crate) signal: String,
    /// Why it ranks where it does.
    pub(crate) reason: String,
    /// Priority rank, 0 highest, 4 none.
    pub(crate) priority: u8,
    /// Type labels it carries.
    pub(crate) types: Vec<String>,
    /// The label and the board disagree; `tm issue repair` should settle it.
    pub(crate) repair_candidate: bool,
}

/// Counts of the issues the rule passed over.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Skipped {
    /// No ready signal.
    pub(crate) not_ready: usize,
    /// Ready, but assigned to someone else.
    pub(crate) assigned_to_others: usize,
    /// Ready, but no acceptance criteria.
    pub(crate) no_acceptance_criteria: Vec<u64>,
}

/// The result of one intake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Outcome {
    /// `owner/repo`.
    pub(crate) repo: String,
    /// Which signal the repo offers.
    pub(crate) signal: RepoSignal,
    /// The goal ranked first, if any.
    pub(crate) goal: Option<String>,
    /// The picks, best first, at most `limit`.
    pub(crate) picks: Vec<Pick>,
    /// Every ready issue whose label and board disagree, picked or not.
    pub(crate) repair_candidates: Vec<u64>,
    /// What was passed over.
    pub(crate) skipped: Skipped,
}

/// The ready verdict of one issue.
struct Readiness {
    signals: Vec<String>,
    conflict: bool,
}

/// Classify one issue's board Status values and labels.
///
/// What: ready when any board Status is a ready option or any label is a
/// ready label. A conflict is a ready board Status beside a not-ready label,
/// a ready label beside a non-ready board Status, or two board Status values
/// that disagree on readiness.
fn readiness(issue: &IssueRow, statuses: &[&String], map: &LabelMap) -> Readiness {
    let mut signals = Vec::new();
    let ready_status: Vec<&&String> = statuses.iter().filter(|s| map.is_ready_status(s)).collect();
    if let Some(s) = ready_status.first() {
        signals.push(format!("board Status: {s}"));
    }
    let ready_label = issue.labels.iter().find(|l| map.is_ready_label(l));
    if let Some(l) = ready_label {
        signals.push(format!("label {l}"));
    }
    let not_ready_label = issue.labels.iter().any(|l| map.is_not_ready_label(l));
    let not_ready_status = statuses.len() > ready_status.len();
    let conflict = (!ready_status.is_empty() && (not_ready_label || not_ready_status))
        || (ready_label.is_some() && not_ready_status);
    Readiness { signals, conflict }
}

/// Filter and rank `snap`'s open issues.
///
/// Why: owner rulings — take only ready issues with acceptance criteria that
/// nobody else holds; rank by the session's goal, then priority label, then
/// age; with no board and no ready label, take nothing.
/// What: a repo with no linked board and no ready label yields
/// [`RepoSignal::None`] and no picks. Otherwise each issue is ready by
/// [`readiness`]; a ready issue assigned only to others, or whose body fails
/// [`has_acceptance_criteria`], is skipped and counted. The rest sort by
/// (served by `goal` first, priority rank, `created_at`, number) and are cut
/// to `limit`. A conflicted ready issue is still taken and is listed in
/// `repair_candidates`.
/// Test: `ranking_orders_by_goal_then_priority_then_age`,
/// `an_issue_without_acceptance_criteria_is_never_taken`,
/// `an_issue_assigned_to_someone_else_is_skipped`,
/// `a_label_only_repo_takes_by_label`, `a_board_only_repo_takes_by_status`,
/// `a_repo_with_no_signal_takes_nothing`,
/// `conflicting_signals_are_taken_and_listed_for_repair`.
pub(crate) fn rank(snap: &Snapshot, map: &LabelMap, goal: Option<&Goal>, limit: usize) -> Outcome {
    let has_ready_label = snap.repo_labels.iter().any(|l| map.is_ready_label(l));
    let signal = match (!snap.boards.is_empty(), has_ready_label) {
        (true, true) => RepoSignal::BoardAndLabels,
        (true, false) => RepoSignal::Board,
        (false, true) => RepoSignal::Labels,
        (false, false) => RepoSignal::None,
    };
    let mut out = Outcome {
        repo: snap.repo.clone(),
        signal,
        goal: goal.map(|g| g.id.clone()),
        picks: Vec::new(),
        repair_candidates: Vec::new(),
        skipped: Skipped::default(),
    };
    if signal == RepoSignal::None {
        return out;
    }
    let mut ranked: Vec<(bool, u8, &IssueRow, Pick)> = Vec::new();
    for issue in &snap.issues {
        let statuses: Vec<&String> = snap
            .boards
            .iter()
            .filter_map(|b| b.statuses.get(&issue.number))
            .collect();
        let ready = readiness(issue, &statuses, map);
        if ready.signals.is_empty() {
            out.skipped.not_ready += 1;
            continue;
        }
        if !issue.assignees.is_empty()
            && !issue
                .assignees
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&snap.viewer))
        {
            out.skipped.assigned_to_others += 1;
            continue;
        }
        if !has_acceptance_criteria(&issue.body) {
            out.skipped.no_acceptance_criteria.push(issue.number);
            continue;
        }
        if ready.conflict {
            out.repair_candidates.push(issue.number);
        }
        let served = goal.is_some_and(|g| g.serves(issue.number, &issue.labels));
        let priority = map.priority_of(&issue.labels);
        let mut why = Vec::new();
        if let (true, Some(g)) = (served, goal) {
            why.push(format!("serves goal {}", g.id));
        }
        why.push(match priority {
            4 => "no priority label".to_string(),
            p => format!("priority P{p}"),
        });
        why.push(format!("created {}", issue.created_at));
        let pick = Pick {
            number: issue.number,
            title: issue.title.clone(),
            signal: ready.signals.join(" + "),
            reason: why.join("; "),
            priority,
            types: issue.labels.iter().filter_map(|l| type_name(l)).collect(),
            repair_candidate: ready.conflict,
        };
        ranked.push((!served, priority, issue, pick));
    }
    ranked.sort_by(|a, b| {
        (a.0, a.1, &a.2.created_at, a.2.number).cmp(&(b.0, b.1, &b.2.created_at, b.2.number))
    });
    out.picks = ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, _, p)| p)
        .collect();
    out
}
