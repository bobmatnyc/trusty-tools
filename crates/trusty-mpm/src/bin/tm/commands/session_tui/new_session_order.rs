//! What order the `tm ls` new-session picker lists registered projects in
//! (#7421, grouped by owner since #7488).
//!
//! Why: the registry read returns `HashMap::values()` with no sort, so the
//! picker's rows arrived in whatever order the map iterated that run. The
//! overlay shows eight rows, and the owner had over twenty registrations, so
//! the handful of projects they actually had sessions in could sit below the
//! fold — in a different place on every run.
//!
//! What: [`ordered_targets`] is the one place that order is decided. Rows are
//! GROUPED by owner-or-domain (#7488) — [`group_of`] answers which group a row
//! belongs to — and a group is never broken apart. The groups themselves are
//! ordered by the best session any of their projects has, through the SAME
//! [`group_rank`](crate::commands::session_picker_order::group_rank) /
//! [`recency_key`](crate::commands::session_picker_order::recency_key) the
//! numbered picker orders its own rows by, so #7421's "the projects I am
//! working in come first" survives as "the OWNER I am working in comes first".
//! Within a group the same two keys order the projects, then the displayed
//! label. Projects with no remote are one fixed group, always last.
//!
//! Test: `new_session_order_*` in `super::tests`.

use std::cmp::Reverse;
use std::collections::HashMap;

use trusty_common::github_path::parse_remote_url;
use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::project::Project;

use crate::commands::session_picker_order::{group_rank, recency_key};

use super::new_session::{Target, repo_is_session_project, row_labels};

/// Sort tier for a project no session in the list belongs to.
///
/// One past [`group_rank`]'s widest answer, so any session at all outranks no
/// session at all.
const NO_SESSION: u8 = 3;

/// Hosts whose first path segment is an OWNER rather than something to group by
/// domain (#7488).
///
/// Why: `github.com` is one "place" holding hundreds of unrelated owners, so
/// grouping its rows by host would put every project in one bucket and answer
/// nothing. A self-hosted forge is the opposite — the host IS the organisation,
/// and its owners are that organisation's teams.
const FORGE_HOSTS: [&str; 4] = ["github.com", "gitlab.com", "bitbucket.org", "codeberg.org"];

/// Group key for a registration whose `repo_url` names no remote (#7488).
///
/// A local-only checkout has no owner and no domain; one fixed key keeps them
/// together instead of scattering them through the owners.
const NO_REMOTE_GROUP: &str = "(local)";

/// Which group a registry row's `repo_url` belongs to (#7488).
///
/// Why: "sort by owner/domain" needs ONE answer per row that both the grouping
/// and the group ordering read, or the two could disagree about where a row
/// belongs.
/// What: `(0, owner)` for a known forge remote ([`FORGE_HOSTS`]), `(0, host)`
/// for any other parseable remote, and `(1, `[`NO_REMOTE_GROUP`]`)` for a local
/// path or an empty `repo_url` — the leading tier is what puts the remote-less
/// rows last whatever they are called. The key is lowercased, because two rows
/// spelling one owner in two cases are one group.
/// Test: `new_session_order_group_of_reads_owner_then_domain`,
/// `new_session_order_groups_by_owner_then_domain`.
pub(crate) fn group_of(repo: &str) -> (u8, String) {
    let Ok(remote) = parse_remote_url(repo) else {
        return (1, NO_REMOTE_GROUP.to_string());
    };
    let host = remote.host.to_lowercase();
    let is_forge = FORGE_HOSTS
        .iter()
        .any(|f| host == *f || host.ends_with(&format!(".{f}")));
    if is_forge {
        (0, remote.owner.to_lowercase())
    } else {
        (0, host)
    }
}

/// Registry rows → picker targets, in an order the registry cannot change
/// (#7421), grouped by owner-or-domain (#7488).
///
/// Why: see the module doc — the same registry contents must render the same
/// rows, the rows for one owner must sit together, and the owner the operator
/// is already working in must be the one the eight-row window shows.
/// What: drops every row
/// [`is_offerable_project`](crate::commands::projects::offerable::is_offerable_project)
/// rejects (#7406, unchanged), sorts the rest by [`Rank`], and appends
/// [`Target::Other`] so the typed-path escape is always the last row.
/// Test: `new_session_order_is_independent_of_registry_iteration_order`,
/// `new_session_order_puts_a_live_session_project_first`,
/// `new_session_order_groups_by_owner_then_domain`.
pub(crate) fn ordered_targets(
    projects: &[Project],
    sessions: &[ManagedSessionSummary],
) -> Vec<Target> {
    let registered: Vec<Target> = projects
        .iter()
        .filter(|p| crate::commands::projects::offerable::is_offerable_project(p))
        .map(|p| Target::Registered {
            name: p.name.clone(),
            repo: p.repo_url.clone(),
        })
        .collect();
    // The label is what the operator reads, so it is also what the alphabetical
    // tie-break sorts on — `row_labels` answers for the whole set at once.
    let labels = row_labels(&registered);
    let facts: Vec<Facts> = registered
        .iter()
        .zip(&labels)
        .map(|(target, label)| facts(target, label, sessions))
        .collect();
    let groups = group_ranks(&facts);
    let mut ranked: Vec<(Rank, Target)> = registered
        .into_iter()
        .zip(&facts)
        .map(|(target, row)| (rank(row, &groups), target))
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    ranked
        .into_iter()
        .map(|(_, target)| target)
        .chain(std::iter::once(Target::Other))
        .collect()
}

/// Everything one row contributes to its own order and to its group's.
struct Facts {
    /// The row's group, from [`group_of`].
    group: (u8, String),
    /// Best session tier for this project; lower sorts first.
    session: u8,
    /// Newest activity timestamp among its sessions.
    recency: String,
    /// The displayed label, lowercased.
    label: String,
    /// Registry key, so equal labels still order deterministically.
    name: String,
}

/// Reduce one target to its [`Facts`] against the session list the TUI holds.
fn facts(target: &Target, label: &str, sessions: &[ManagedSessionSummary]) -> Facts {
    let (name, repo) = match target {
        Target::Registered { name, repo } => (name.as_str(), repo.as_str()),
        // Never ranked: `ordered_targets` appends the escape row after sorting.
        Target::Other => ("", ""),
    };
    let mut session = NO_SESSION;
    let mut recency = "";
    for summary in sessions.iter().filter(|s| repo_is_session_project(repo, s)) {
        session = session.min(group_rank(summary));
        recency = recency.max(recency_key(summary));
    }
    Facts {
        group: group_of(repo),
        session,
        recency: recency.to_string(),
        label: label.to_lowercase(),
        name: name.to_string(),
    }
}

/// The best session tier and recency each group has, over all its rows.
///
/// Why (#7488): a group moves as a unit, so its position has to be decided by
/// the WHOLE group — ranking each row on its own session would split an owner
/// whose projects have different session states across the list.
fn group_ranks(facts: &[Facts]) -> HashMap<(u8, String), (u8, String)> {
    let mut best: HashMap<(u8, String), (u8, String)> = HashMap::new();
    for row in facts {
        let slot = best
            .entry(row.group.clone())
            .or_insert((NO_SESSION, String::new()));
        slot.0 = slot.0.min(row.session);
        if row.recency > slot.1 {
            slot.1 = row.recency.clone();
        }
    }
    best
}

/// One row's sort position, in the order the fields are compared.
///
/// Why: a derived `Ord` over named fields states the whole precedence in one
/// place, and `name` last makes the order TOTAL — two registrations that spell
/// the same label still come out in a fixed sequence.
/// What: the four `group_*` fields place the row's GROUP (#7488) — remote-less
/// groups last, then the best session tier and recency any of the group's
/// projects has, then the group key alphabetically. The remaining fields place
/// the row WITHIN its group by the same session/recency keys, then by label.
/// Test: `new_session_order_is_independent_of_registry_iteration_order`,
/// `new_session_order_puts_a_live_session_project_first`,
/// `new_session_order_groups_by_owner_then_domain`.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    /// 0 for a group with a remote, 1 for the remote-less group.
    group_tier: u8,
    /// Best session tier anywhere in this group.
    group_session: u8,
    /// Newest activity anywhere in this group, descending.
    group_recency: Reverse<String>,
    /// The owner-or-domain key, lowercased.
    group_key: String,
    /// This project's own session tier.
    session: u8,
    /// This project's own newest activity, descending.
    recency: Reverse<String>,
    /// The displayed label, lowercased.
    label: String,
    /// Registry key, so equal labels still order deterministically.
    name: String,
}

/// Place one row, given the aggregates its group earned.
fn rank(row: &Facts, groups: &HashMap<(u8, String), (u8, String)>) -> Rank {
    let (group_session, group_recency) = groups
        .get(&row.group)
        .cloned()
        .unwrap_or((NO_SESSION, String::new()));
    Rank {
        group_tier: row.group.0,
        group_session,
        group_recency: Reverse(group_recency),
        group_key: row.group.1.clone(),
        session: row.session,
        recency: Reverse(row.recency.clone()),
        label: row.label.clone(),
        name: row.name.clone(),
    }
}
