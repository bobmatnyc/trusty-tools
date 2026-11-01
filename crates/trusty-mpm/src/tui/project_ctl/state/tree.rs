//! The Owner -> Project -> Sessions tree model for `tm tui` (#10476).
//!
//! Why: the tree pane replaces the Projects and Sessions panes (Architect Q9).
//! The poll rebuilds the project and session lists on every tick, so the tree
//! is a pure projection of them plus the operator's expanded set. Keeping it
//! pure makes the counters, the collapse rules and the worker indentation
//! testable without a terminal or a daemon.
//! What: [`NodeId`] names a node by value, so a repoll keeps the same id.
//! [`build_tree`] flattens the visible nodes into [`TreeRow`]s in draw order.
//! [`owner_of_repo_url`] derives the owner from `repo_url`, because the
//! registry stores no owner field.
//! Test: `tree_tests.rs` (`counts_total_and_live_per_project_and_owner`,
//! `worker_indents_under_parent_by_link`, `owner_of_https_with_and_without_dot_git`).

use std::collections::{BTreeMap, BTreeSet};

use super::{ProjectRow, SessionRow};
use crate::session_manager::ssh_host_alias::SshHostAliases;
use crate::session_manager::worktree_repo_slug::parse_repo_slug;

/// The owner shown for a project whose `repo_url` names no remote repository.
pub const LOCAL_OWNER: &str = "(local)";

/// The deepest worker indentation drawn; a deeper chain draws at this depth.
const MAX_WORKER_DEPTH: usize = 4;

/// Name tokens that mark a session as a manager for the name heuristic.
const MANAGER_TOKENS: &[&str] = &["manager", "lead", "orchestrator"];

/// One node of the tree, named by value.
///
/// Why: rows are rebuilt from scratch on every poll, so the selection and the
/// expanded set must name nodes by what they are, not by row index. Each
/// variant carries its ancestors, so a vanished node can still name the node
/// the selection falls back to.
/// What: owners, projects, sessions, and the dimmed anchor drawn for a parent
/// that is not in the project's list.
/// Test: `parent_chain_walks_to_the_owner`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeId {
    /// An owner bar.
    Owner(String),
    /// A project under `owner`.
    Project { owner: String, name: String },
    /// A session `id` under `project`.
    Session {
        owner: String,
        project: String,
        id: String,
    },
    /// The placeholder for `parent`, a parent session the project does not list.
    Anchor {
        owner: String,
        project: String,
        parent: String,
    },
}

impl NodeId {
    /// The node this one sits under; `None` for an owner.
    pub fn parent(&self) -> Option<NodeId> {
        match self {
            NodeId::Owner(_) => None,
            NodeId::Project { owner, .. } => Some(NodeId::Owner(owner.clone())),
            NodeId::Session { owner, project, .. } | NodeId::Anchor { owner, project, .. } => {
                Some(NodeId::Project {
                    owner: owner.clone(),
                    name: project.clone(),
                })
            }
        }
    }

    /// The project this node belongs to; `None` for an owner.
    pub fn project_name(&self) -> Option<&str> {
        match self {
            NodeId::Owner(_) => None,
            NodeId::Project { name, .. } => Some(name),
            NodeId::Session { project, .. } | NodeId::Anchor { project, .. } => Some(project),
        }
    }

    /// True for the two node kinds Enter, Left and Right expand or collapse.
    pub fn is_expandable(&self) -> bool {
        matches!(self, NodeId::Owner(_) | NodeId::Project { .. })
    }
}

/// What one visible row draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRowKind {
    /// An owner bar with its project count and session counters.
    Owner {
        name: String,
        projects: usize,
        total: usize,
        live: usize,
        expanded: bool,
    },
    /// A project with its session counters.
    Project {
        name: String,
        total: usize,
        live: usize,
        expanded: bool,
    },
    /// A session: `index` into `sessions_by_project[project]`, `depth` its
    /// worker indentation (0 for a session with no parent shown).
    Session {
        project: String,
        index: usize,
        depth: usize,
    },
    /// The dimmed line standing in for a parent the project does not list.
    Anchor { parent: String, depth: usize },
}

/// One visible row of the tree, in draw order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// The node this row draws; the selection key.
    pub id: NodeId,
    /// What the row draws.
    pub kind: TreeRowKind,
}

/// The owner a project's `repo_url` names, or [`LOCAL_OWNER`].
///
/// Why: the registry has no owner field (a stored one is a named follow-up of
/// #10476), so the owner is the `owner` segment of the repository slug.
/// What: reuses [`parse_repo_slug`], the parser the worktree guard trusts.
/// A github.com URL gives `owner`; another host gives `host/owner`, so two
/// hosts with one owner name stay apart. A local path, a `file://` URL or an
/// unparsable string gives [`LOCAL_OWNER`].
/// Test: `owner_of_https_with_and_without_dot_git`, `owner_of_ssh_url`,
/// `owner_of_scp_like`, `owner_of_local_path_is_local`,
/// `owner_of_unparsable_is_local`, `owner_of_non_default_host`.
pub(crate) fn owner_of_repo_url(repo_url: &str, aliases: &SshHostAliases) -> String {
    parse_repo_slug(repo_url, aliases)
        .ok()
        .and_then(|slug| slug.rsplit_once('/').map(|(owner, _)| owner.to_string()))
        .filter(|owner| !owner.is_empty())
        .unwrap_or_else(|| LOCAL_OWNER.to_string())
}

/// True when a session's lifecycle state counts as live for a counter.
fn is_live(state: &str) -> bool {
    matches!(state, "active" | "provisioning")
}

/// Flatten the visible tree into rows, in draw order.
///
/// Why: the pane, the key handler and the selection clamp all need the same
/// row list; one pure builder keeps them in agreement.
/// What: owners are sorted by name with [`LOCAL_OWNER`] last; projects keep
/// registry order inside their owner. An owner or project is expanded only
/// when its [`NodeId`] is in `expanded`. Counters count the polled (non-
/// terminal) session rows: `total` is all of them, `live` those `active` or
/// `provisioning`; an owner sums its projects. With `indent_workers`, sessions
/// follow [`session_placements`]; without it, they list flat in poll order.
/// Test: `counts_total_and_live_per_project_and_owner`,
/// `collapsed_owner_hides_projects`, `collapsed_project_hides_sessions`.
pub(crate) fn build_tree(
    projects: &[ProjectRow],
    sessions_by_project: &BTreeMap<String, Vec<SessionRow>>,
    expanded: &BTreeSet<NodeId>,
    indent_workers: bool,
    aliases: &SshHostAliases,
) -> Vec<TreeRow> {
    let mut by_owner: BTreeMap<String, Vec<&ProjectRow>> = BTreeMap::new();
    for project in projects {
        by_owner
            .entry(owner_of_repo_url(&project.repo_url, aliases))
            .or_default()
            .push(project);
    }
    let mut owners: Vec<(String, Vec<&ProjectRow>)> = by_owner.into_iter().collect();
    // `(local)` sorts before letters; it belongs last.
    owners.sort_by_key(|(owner, _)| (owner == LOCAL_OWNER, owner.clone()));

    let empty = Vec::new();
    let mut rows = Vec::new();
    for (owner, owner_projects) in owners {
        let counts: Vec<(usize, usize)> = owner_projects
            .iter()
            .map(|p| counters(sessions_by_project.get(&p.name).unwrap_or(&empty)))
            .collect();
        let owner_id = NodeId::Owner(owner.clone());
        let owner_open = expanded.contains(&owner_id);
        rows.push(TreeRow {
            id: owner_id,
            kind: TreeRowKind::Owner {
                name: owner.clone(),
                projects: owner_projects.len(),
                total: counts.iter().map(|c| c.0).sum(),
                live: counts.iter().map(|c| c.1).sum(),
                expanded: owner_open,
            },
        });
        if !owner_open {
            continue;
        }
        for (project, (total, live)) in owner_projects.into_iter().zip(counts) {
            let project_id = NodeId::Project {
                owner: owner.clone(),
                name: project.name.clone(),
            };
            let project_open = expanded.contains(&project_id);
            rows.push(TreeRow {
                id: project_id,
                kind: TreeRowKind::Project {
                    name: project.name.clone(),
                    total,
                    live,
                    expanded: project_open,
                },
            });
            if project_open {
                let sessions = sessions_by_project.get(&project.name).unwrap_or(&empty);
                push_sessions(&mut rows, &owner, &project.name, sessions, indent_workers);
            }
        }
    }
    rows
}

/// `(total, live)` for one project's session rows.
fn counters(sessions: &[SessionRow]) -> (usize, usize) {
    let live = sessions.iter().filter(|s| is_live(&s.state)).count();
    (sessions.len(), live)
}

/// Append one expanded project's session rows (and anchors) to `rows`.
fn push_sessions(
    rows: &mut Vec<TreeRow>,
    owner: &str,
    project: &str,
    sessions: &[SessionRow],
    indent_workers: bool,
) {
    let placements = if indent_workers {
        session_placements(sessions)
    } else {
        (0..sessions.len())
            .map(|i| Placement::Session(i, 0))
            .collect()
    };
    for placement in placements {
        let (id, kind) = match placement {
            Placement::Session(index, depth) => (
                NodeId::Session {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    id: sessions[index].id.clone(),
                },
                TreeRowKind::Session {
                    project: project.to_string(),
                    index,
                    depth,
                },
            ),
            Placement::Anchor(parent) => (
                NodeId::Anchor {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    parent: parent.clone(),
                },
                TreeRowKind::Anchor { parent, depth: 0 },
            ),
        };
        rows.push(TreeRow { id, kind });
    }
}

/// Where one entry of a project's session list draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Placement {
    /// Session at `index` in the project's list, at worker `depth`.
    Session(usize, usize),
    /// A dimmed anchor for the missing parent id.
    Anchor(String),
}

/// Who a session hangs under inside its project.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParentRef {
    /// A root: drawn at depth 0.
    None,
    /// A session in the same list, by index.
    Listed(usize),
    /// A `parent_session_id` the list does not hold.
    Missing(String),
}

/// Order one project's sessions so each worker follows its parent, indented.
///
/// Why: the owner asked for workers indented under the session that launched
/// them (#10476 comment 2). The daemon's `parent_session_id` is the truth; a
/// session without it falls back to [`heuristic_parent`], so pre-link sessions
/// still group.
/// What: a session whose link names a listed session hangs under it; a link
/// that names no listed session hangs under one [`Placement::Anchor`] per
/// missing id, placed where its first child would have drawn. Children keep
/// poll order. A cycle in the links cannot loop: a session is placed once, and
/// any session a cycle leaves unplaced is drawn as a root. Depth stops at
/// [`MAX_WORKER_DEPTH`].
/// Test: `worker_indents_under_parent_by_link`,
/// `worker_indents_by_name_heuristic_without_link`,
/// `missing_parent_shows_dimmed_anchor`, `parent_cycle_places_each_session_once`.
pub(crate) fn session_placements(sessions: &[SessionRow]) -> Vec<Placement> {
    let parents: Vec<ParentRef> = (0..sessions.len())
        .map(|i| parent_of(sessions, i))
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); sessions.len()];
    for (i, parent) in parents.iter().enumerate() {
        if let ParentRef::Listed(p) = parent {
            children[*p].push(i);
        }
    }
    let mut placed = vec![false; sessions.len()];
    let mut out = Vec::with_capacity(sessions.len());
    let mut anchors_done: BTreeSet<&str> = BTreeSet::new();
    for (i, parent) in parents.iter().enumerate() {
        match parent {
            ParentRef::None => place(i, 0, &children, &mut placed, &mut out),
            ParentRef::Missing(pid) if anchors_done.insert(pid.as_str()) => {
                out.push(Placement::Anchor(pid.clone()));
                for (j, other) in parents.iter().enumerate() {
                    if matches!(other, ParentRef::Missing(q) if q == pid) {
                        place(j, 1, &children, &mut placed, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    // Only a cycle leaves a session unplaced; draw it as a root.
    for i in 0..sessions.len() {
        place(i, 0, &children, &mut placed, &mut out);
    }
    out
}

/// Place session `i` at `depth`, then its children one level deeper.
fn place(
    i: usize,
    depth: usize,
    children: &[Vec<usize>],
    placed: &mut [bool],
    out: &mut Vec<Placement>,
) {
    if placed[i] {
        return;
    }
    placed[i] = true;
    out.push(Placement::Session(i, depth.min(MAX_WORKER_DEPTH)));
    for &child in &children[i] {
        place(child, depth + 1, children, placed, out);
    }
}

/// The parent of session `i`: its link first, the name heuristic second.
fn parent_of(sessions: &[SessionRow], i: usize) -> ParentRef {
    let session = &sessions[i];
    if let Some(pid) = session
        .parent_session_id
        .as_deref()
        .filter(|p| !p.is_empty())
    {
        return match sessions.iter().position(|s| s.id == pid) {
            Some(p) if p != i => ParentRef::Listed(p),
            Some(_) => ParentRef::None,
            None => ParentRef::Missing(pid.to_string()),
        };
    }
    heuristic_parent(sessions, i).map_or(ParentRef::None, ParentRef::Listed)
}

/// The listed manager a session without a link most likely works for.
///
/// Why: sessions spawned before the daemon recorded `parent_session_id` (and
/// sessions from an older daemon) carry no link, but their names often say
/// who launched them (`reports-migration-manager`, `reports-migration-worker-1`).
/// What: a session is a worker when its name has a `worker` segment or ends
/// in a numeric segment, and is not itself a manager. Its manager is the
/// session whose name has a [`MANAGER_TOKENS`] segment and shares the longest
/// leading run of `-`-separated segments with it, at least two. Comparison is
/// case-insensitive. No candidate gives `None`.
/// Test: `worker_indents_by_name_heuristic_without_link`,
/// `heuristic_needs_a_manager_and_two_shared_segments`.
fn heuristic_parent(sessions: &[SessionRow], i: usize) -> Option<usize> {
    let name = segments(&sessions[i].name);
    let is_worker = name.iter().any(|s| s == "worker")
        || name
            .last()
            .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()));
    if !is_worker || is_manager(&name) {
        return None;
    }
    sessions
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != i)
        .filter_map(|(j, s)| {
            let candidate = segments(&s.name);
            if !is_manager(&candidate) {
                return None;
            }
            let shared = name
                .iter()
                .zip(&candidate)
                .take_while(|(a, b)| a == b)
                .count();
            (shared >= 2).then_some((shared, j))
        })
        .max_by_key(|(shared, j)| (*shared, std::cmp::Reverse(*j)))
        .map(|(_, j)| j)
}

/// Lowercased, non-empty `-`-separated segments of a session name.
fn segments(name: &str) -> Vec<String> {
    name.split('-')
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// True when a name's segments include a manager token.
fn is_manager(segments: &[String]) -> bool {
    segments
        .iter()
        .any(|s| MANAGER_TOKENS.contains(&s.as_str()))
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
