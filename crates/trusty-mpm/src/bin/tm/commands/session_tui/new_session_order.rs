//! What order the `tm ls` new-session picker lists registered projects in
//! (#7421).
//!
//! Why: the registry read returns `HashMap::values()` with no sort, so the
//! picker's rows arrived in whatever order the map iterated that run. The
//! overlay shows eight rows, and the owner had over twenty registrations, so
//! the handful of projects they actually had sessions in could sit below the
//! fold — in a different place on every run.
//!
//! What: [`ordered_targets`] is the one place that order is decided. Projects
//! with a managed session come first, best session state first and most
//! recently active within that, through the SAME
//! [`group_rank`](crate::commands::session_picker_order::group_rank) /
//! [`recency_key`](crate::commands::session_picker_order::recency_key) the
//! numbered picker orders its own rows by. Everything else follows
//! alphabetically by the `owner/repo` label the row displays: the registry
//! record carries no timestamp of its own, so alphabetical is the only stable
//! order left for it.
//!
//! Test: `new_session_order_*` in `super::tests`.

use std::cmp::Reverse;

use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::project::Project;

use crate::commands::session_picker_order::{group_rank, recency_key};

use super::new_session::{Target, repo_is_session_project, row_labels};

/// Sort tier for a project no session in the list belongs to.
///
/// One past [`group_rank`]'s widest answer, so any session at all outranks no
/// session at all.
const NO_SESSION: u8 = 3;

/// Registry rows → picker targets, in an order the registry cannot change
/// (#7421).
///
/// Why: see the module doc — the same registry contents must render the same
/// rows, and the projects the operator is already working in must be the ones
/// the eight-row window shows.
/// What: drops every row
/// [`is_offerable_project`](crate::commands::projects::offerable::is_offerable_project)
/// rejects (#7406, unchanged), sorts the rest by [`Rank`], and appends
/// [`Target::Other`] so the typed-path escape is always the last row.
/// Test: `new_session_order_is_independent_of_registry_iteration_order`,
/// `new_session_order_puts_a_live_session_project_first`.
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
    let mut ranked: Vec<(Rank, Target)> = registered
        .into_iter()
        .zip(labels)
        .map(|(target, label)| (rank(&target, &label, sessions), target))
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    ranked
        .into_iter()
        .map(|(_, target)| target)
        .chain(std::iter::once(Target::Other))
        .collect()
}

/// One row's sort position, in the order the fields are compared.
///
/// Why: a derived `Ord` over four named fields states the whole precedence in
/// one place, and `name` last makes the order TOTAL — two registrations that
/// spell the same label still come out in a fixed sequence.
/// What: `session` is the best [`group_rank`] among the project's sessions
/// ([`NO_SESSION`] when it has none); `recency` is the newest [`recency_key`]
/// among them, reversed so later sorts first; `label` is the displayed
/// `owner/repo`, lowercased.
/// Test: `new_session_order_is_independent_of_registry_iteration_order`,
/// `new_session_order_puts_a_live_session_project_first`.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    /// Best session tier for this project; lower sorts first.
    session: u8,
    /// Newest activity timestamp among its sessions, descending.
    recency: Reverse<String>,
    /// The displayed label, lowercased.
    label: String,
    /// Registry key, so equal labels still order deterministically.
    name: String,
}

/// Rank one target against the session list the `tm ls` TUI already holds.
fn rank(target: &Target, label: &str, sessions: &[ManagedSessionSummary]) -> Rank {
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
    Rank {
        session,
        recency: Reverse(recency.to_string()),
        label: label.to_lowercase(),
        name: name.to_string(),
    }
}
