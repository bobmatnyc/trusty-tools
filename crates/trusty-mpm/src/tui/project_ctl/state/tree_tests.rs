//! Tests for the tree model (#10476): owner parsing, counters, collapse rules
//! and worker indentation.

use super::*;
use crate::tui::project_ctl::state::ProjectCtlState;
use crate::tui::project_ctl::state::session_col::{SessionColRow, build_session_rows};

fn aliases() -> SshHostAliases {
    SshHostAliases::empty()
}

fn project(name: &str, repo_url: &str) -> ProjectRow {
    ProjectRow {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        ..Default::default()
    }
}

fn session(id: &str, name: &str, state: &str, parent: Option<&str>) -> SessionRow {
    SessionRow {
        id: id.to_string(),
        name: name.to_string(),
        state: state.to_string(),
        parent_session_id: parent.map(str::to_string),
        ..Default::default()
    }
}

fn owner(name: &str) -> NodeId {
    NodeId::Owner(name.to_string())
}

fn proj(owner: &str, name: &str) -> NodeId {
    NodeId::Project {
        owner: owner.to_string(),
        name: name.to_string(),
    }
}

/// Two owners: acme (alpha, beta) and zeta (gamma), plus one local project.
fn fixture() -> (Vec<ProjectRow>, BTreeMap<String, Vec<SessionRow>>) {
    let projects = vec![
        project("alpha", "https://github.com/acme/alpha"),
        project("gamma", "git@github.com:zeta/gamma.git"),
        project("beta", "https://github.com/acme/beta.git"),
        project("scratch", "/Users/me/scratch"),
    ];
    let mut sessions = BTreeMap::new();
    sessions.insert(
        "alpha".to_string(),
        vec![
            session("a1", "alpha-1", "active", None),
            session("a2", "alpha-2", "stopped", None),
            session("a3", "alpha-3", "provisioning", None),
        ],
    );
    sessions.insert(
        "beta".to_string(),
        vec![session("b1", "beta-1", "errored", None)],
    );
    (projects, sessions)
}

/// Every owner and project of [`fixture`], for a fully collapsed tree.
fn all_nodes() -> BTreeSet<NodeId> {
    [
        owner("acme"),
        owner("zeta"),
        owner(LOCAL_OWNER),
        proj("acme", "alpha"),
        proj("acme", "beta"),
        proj("zeta", "gamma"),
        proj(LOCAL_OWNER, "scratch"),
    ]
    .into_iter()
    .collect()
}

fn ids(rows: &[TreeRow]) -> Vec<NodeId> {
    rows.iter().map(|r| r.id.clone()).collect()
}

/// The indented session names, as `"<depth>:<name>"` or `"anchor:<id>"`.
fn placement_names(sessions: &[SessionRow]) -> Vec<String> {
    session_placements(sessions)
        .into_iter()
        .map(|p| match p {
            Placement::Session(i, depth) => format!("{depth}:{}", sessions[i].name),
            Placement::Anchor(parent) => format!("anchor:{parent}"),
        })
        .collect()
}

#[test]
fn owner_of_https_with_and_without_dot_git() {
    for url in [
        "https://github.com/acme/widget",
        "https://github.com/acme/widget.git",
    ] {
        assert_eq!(owner_of_repo_url(url, &aliases()), "acme", "{url}");
    }
}

#[test]
fn owner_of_ssh_url() {
    assert_eq!(
        owner_of_repo_url("ssh://git@github.com/acme/widget.git", &aliases()),
        "acme"
    );
}

#[test]
fn owner_of_scp_like() {
    assert_eq!(
        owner_of_repo_url("git@github.com:acme/widget.git", &aliases()),
        "acme"
    );
}

#[test]
fn owner_of_local_path_is_local() {
    for url in ["/Users/me/code/widget", "file:///tmp/widget", "./widget"] {
        assert_eq!(owner_of_repo_url(url, &aliases()), LOCAL_OWNER, "{url}");
    }
}

#[test]
fn owner_of_unparsable_is_local() {
    for url in [
        "",
        "not a url",
        "https://github.com/",
        "git@unknownalias:a/b",
    ] {
        assert_eq!(owner_of_repo_url(url, &aliases()), LOCAL_OWNER, "{url:?}");
    }
}

#[test]
fn owner_of_non_default_host() {
    assert_eq!(
        owner_of_repo_url("https://ghe.corp.example/acme/widget", &aliases()),
        "ghe.corp.example/acme"
    );
}

/// The session column rows for project `alpha` of `by_project`.
fn alpha_session_rows(
    by_project: &BTreeMap<String, Vec<SessionRow>>,
    indent: bool,
) -> Vec<SessionColRow> {
    let projects = vec![project("alpha", "https://github.com/acme/alpha")];
    let source = proj("acme", "alpha");
    build_session_rows(Some(&source), &projects, by_project, indent, |_| {
        Some("acme".to_string())
    })
}

#[test]
fn parent_chain_walks_to_the_owner() {
    assert_eq!(proj("acme", "alpha").parent(), Some(owner("acme")));
    assert_eq!(owner("acme").parent(), None);
}

#[test]
fn counts_total_and_live_per_project_and_owner() {
    let (projects, sessions) = fixture();
    let rows = build_tree(&projects, &sessions, &BTreeSet::new(), &aliases());
    let acme = rows
        .iter()
        .find(|r| r.id == owner("acme"))
        .expect("acme bar");
    assert_eq!(
        acme.kind,
        TreeRowKind::Owner {
            name: "acme".to_string(),
            projects: 2,
            total: 4,
            live: 2,
            expanded: true,
        }
    );
    let alpha = rows.iter().find(|r| r.id == proj("acme", "alpha")).unwrap();
    assert_eq!(
        alpha.kind,
        TreeRowKind::Project {
            name: "alpha".to_string(),
            total: 3,
            live: 2,
        }
    );
    let gamma = rows.iter().find(|r| r.id == proj("zeta", "gamma")).unwrap();
    assert!(matches!(
        gamma.kind,
        TreeRowKind::Project {
            total: 0,
            live: 0,
            ..
        }
    ));
}

#[test]
fn owners_sort_by_name_with_local_last() {
    let (projects, sessions) = fixture();
    let rows = build_tree(&projects, &sessions, &all_nodes(), &aliases());
    assert_eq!(
        ids(&rows),
        vec![owner("acme"), owner("zeta"), owner(LOCAL_OWNER)]
    );
}

#[test]
fn collapsed_owner_hides_projects() {
    let (projects, sessions) = fixture();
    let collapsed = [owner("acme")].into_iter().collect();
    let rows = build_tree(&projects, &sessions, &collapsed, &aliases());
    assert!(!rows.iter().any(|r| r.id.project_name() == Some("alpha")));
    assert!(!rows.iter().any(|r| r.id.project_name() == Some("beta")));
    assert!(rows.iter().any(|r| r.id == proj("zeta", "gamma")));
}

/// #10754: two-column MPM pane (Bob 11-06) — the left column is owner >
/// project only; the sessions draw in the right column.
#[test]
fn left_column_draws_no_session_rows() {
    let (projects, sessions) = fixture();
    let rows = build_tree(&projects, &sessions, &BTreeSet::new(), &aliases());
    assert_eq!(ids(&rows).into_iter().collect::<BTreeSet<_>>(), all_nodes());
    assert_eq!(rows.len(), all_nodes().len(), "{rows:?}");
    assert!(
        !proj("acme", "alpha").is_expandable(),
        "a project is a leaf"
    );
}

#[test]
fn worker_indents_under_parent_by_link() {
    // Names share no prefix; only the link groups them.
    let sessions = vec![
        session("w1", "zebra", "active", Some("m1")),
        session("m1", "boss", "active", None),
        session("w2", "yak", "active", Some("w1")),
        session("solo", "solo", "active", None),
    ];
    assert_eq!(
        placement_names(&sessions),
        vec!["0:boss", "1:zebra", "2:yak", "0:solo"]
    );
}

#[test]
fn worker_indents_by_name_heuristic_without_link() {
    let sessions = vec![
        session("1", "reports-migration-worker-1", "active", None),
        session("2", "mfe-e2e", "active", None),
        session("3", "reports-migration-manager", "active", None),
        session("4", "reports-migration-worker-2", "active", None),
    ];
    assert_eq!(
        placement_names(&sessions),
        vec![
            "0:mfe-e2e",
            "0:reports-migration-manager",
            "1:reports-migration-worker-1",
            "1:reports-migration-worker-2",
        ]
    );
}

#[test]
fn heuristic_needs_a_manager_and_two_shared_segments() {
    // No manager-named session: numbered sessions stay roots.
    let sessions = vec![
        session("1", "tm-trusty-tools-1", "active", None),
        session("2", "tm-trusty-tools-2", "active", None),
    ];
    assert_eq!(
        placement_names(&sessions),
        vec!["0:tm-trusty-tools-1", "0:tm-trusty-tools-2"]
    );
    // One shared segment is not enough.
    let sessions = vec![
        session("1", "app-manager", "active", None),
        session("2", "app-2", "active", None),
    ];
    assert_eq!(placement_names(&sessions), vec!["0:app-manager", "0:app-2"]);
}

#[test]
fn missing_parent_shows_dimmed_anchor() {
    let sessions = vec![
        session("x", "root", "active", None),
        session("c1", "child-a", "active", Some("gone")),
        session("c2", "child-b", "active", Some("gone")),
    ];
    assert_eq!(
        placement_names(&sessions),
        vec!["0:root", "anchor:gone", "1:child-a", "1:child-b"]
    );

    let by_project: BTreeMap<_, _> = [("alpha".to_string(), sessions)].into_iter().collect();
    let rows = alpha_session_rows(&by_project, true);
    assert!(rows.contains(&SessionColRow::Anchor {
        parent: "gone".to_string(),
        depth: 0,
    }));
}

#[test]
fn parent_cycle_places_each_session_once() {
    let sessions = vec![
        session("a", "a", "active", Some("b")),
        session("b", "b", "active", Some("a")),
    ];
    let names = placement_names(&sessions);
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"0:a".to_string()));
    assert!(names.contains(&"1:b".to_string()));
}

#[test]
fn indentation_off_lists_flat_in_poll_order() {
    let sessions = vec![
        session("w1", "zebra", "active", Some("m1")),
        session("m1", "boss", "active", None),
        session("c", "orphan", "active", Some("gone")),
    ];
    let by_project: BTreeMap<_, _> = [("alpha".to_string(), sessions)].into_iter().collect();
    let rows = alpha_session_rows(&by_project, false);
    let depths: Vec<_> = rows
        .iter()
        .filter_map(|r| match r {
            SessionColRow::Session { index, depth, .. } => Some((*index, *depth)),
            SessionColRow::Anchor { .. } => panic!("no anchor without indentation"),
            SessionColRow::Header { .. } => None,
        })
        .collect();
    assert_eq!(depths, vec![(0, 0), (1, 0), (2, 0)]);
}

/// The owner bars of `rows`: `(id, shown name, project count, expanded)`.
fn owner_bars(rows: &[TreeRow]) -> Vec<(NodeId, String, usize, bool)> {
    rows.iter()
        .filter_map(|r| match &r.kind {
            TreeRowKind::Owner {
                name,
                projects,
                expanded,
                ..
            } => Some((r.id.clone(), name.clone(), *projects, *expanded)),
            _ => None,
        })
        .collect()
}

#[test]
fn case_only_owner_variants_group_as_one_owner() {
    let projects = vec![
        project("a", "https://github.com/BTA-agent-AI/a"),
        project("b", "https://github.com/bta-agent-ai/b"),
        project("c", "https://github.com/bta-agent/c"),
    ];
    let rows = build_tree(&projects, &BTreeMap::new(), &BTreeSet::new(), &aliases());
    assert_eq!(
        owner_bars(&rows),
        vec![
            (owner("bta-agent"), "bta-agent".to_string(), 1, true),
            (owner("bta-agent-ai"), "BTA-agent-AI".to_string(), 2, true),
        ],
        "case-only variants share one bar showing the first-seen spelling; \
         an owner differing by more than case stays separate"
    );
}

#[test]
fn owner_expand_state_survives_first_seen_spelling_change() {
    let upper = project("a", "https://github.com/BTA-agent-AI/a");
    let lower = project("b", "https://github.com/bta-agent-ai/b");
    let empty = BTreeMap::new();
    // A collapse keyed by the owner key holds under either first-seen spelling.
    let collapsed: BTreeSet<NodeId> = [owner("bta-agent-ai")].into_iter().collect();
    let first = build_tree(
        &[upper.clone(), lower.clone()],
        &empty,
        &collapsed,
        &aliases(),
    );
    let second = build_tree(
        &[lower.clone(), upper.clone()],
        &empty,
        &collapsed,
        &aliases(),
    );
    assert_eq!(
        owner_bars(&first),
        vec![(owner("bta-agent-ai"), "BTA-agent-AI".to_string(), 2, false)]
    );
    assert_eq!(
        owner_bars(&second),
        vec![(owner("bta-agent-ai"), "bta-agent-ai".to_string(), 2, false)]
    );
    assert_eq!(ids(&first), [owner("bta-agent-ai")]);
    assert_eq!(ids(&second), [owner("bta-agent-ai")]);

    // Open, the projects sit under the one key in registry order.
    let open = BTreeSet::new();
    let first = build_tree(&[upper.clone(), lower.clone()], &empty, &open, &aliases());
    let second = build_tree(&[lower, upper], &empty, &open, &aliases());
    assert_eq!(
        ids(&first)[1..],
        [proj("bta-agent-ai", "a"), proj("bta-agent-ai", "b")]
    );
    assert_eq!(
        ids(&second)[1..],
        [proj("bta-agent-ai", "b"), proj("bta-agent-ai", "a")]
    );
}

/// The polled state of `projects` (owner from the URL) and `sessions`.
fn poll_into(
    state: &mut ProjectCtlState,
    projects: Vec<ProjectRow>,
    sessions: BTreeMap<String, Vec<SessionRow>>,
) {
    use crate::tui::project_ctl::poll::tick::{CoreFetch, apply_core};
    apply_core(
        state,
        CoreFetch {
            reachable: true,
            fleet: Some((projects, sessions, BTreeMap::new())),
        },
    );
}

/// True when every owner row of `rows` draws open.
fn all_rows_open(rows: &[TreeRow]) -> bool {
    rows.iter().all(|r| match r.kind {
        TreeRowKind::Owner { expanded, .. } => expanded,
        TreeRowKind::Project { .. } => true,
    })
}

/// The session ids the right column lists, in draw order.
fn column_ids(state: &ProjectCtlState) -> Vec<&str> {
    state
        .session_col
        .rows
        .iter()
        .filter_map(SessionColRow::session_id)
        .collect()
}

/// REGRESSION (#10476, Bob 11-05 17:30Z): the first poll drew only owner bars,
/// so no session showed until the operator opened each node. #10754: the
/// left column now holds owners and projects only, and the first selection's
/// sessions show in the right column.
#[test]
fn fresh_state_shows_every_node_expanded_without_sessions() {
    let (projects, sessions) = fixture();
    let mut state = ProjectCtlState::default();
    poll_into(&mut state, projects, sessions);
    let rows = &state.tree.rows;
    for node in all_nodes() {
        assert!(rows.iter().any(|r| r.id == node), "{node:?} is drawn");
        assert!(state.tree.is_expanded(&node), "{node:?} starts open");
    }
    assert!(all_rows_open(rows), "{rows:?}");
    assert_eq!(rows.len(), all_nodes().len(), "no session rows on the left");
    assert_eq!(state.selected_node(), Some(&owner("acme")));
    assert_eq!(
        column_ids(&state),
        ["a1", "a2", "a3", "b1"],
        "the first owner's session rows on first render"
    );
}

/// REGRESSION (#10476): a node the operator closes stays closed across
/// repolls, and opening it again works. #10754: owners are the nodes that
/// close; a project is a leaf.
#[test]
fn collapse_survives_a_rebuild() {
    let (projects, sessions) = fixture();
    let mut state = ProjectCtlState::default();
    poll_into(&mut state, projects.clone(), sessions.clone());
    let acme = owner("acme");
    assert!(state.tree.is_expanded(&acme), "acme starts open");

    state.select_node(acme.clone());
    state.toggle_selected();
    poll_into(&mut state, projects.clone(), sessions.clone());
    poll_into(&mut state, projects, sessions);
    assert!(!state.tree.is_expanded(&acme), "the collapse survives");
    assert!(
        !state
            .tree
            .rows
            .iter()
            .any(|r| r.id == proj("acme", "alpha"))
    );
    assert_eq!(state.selected_node(), Some(&acme));

    state.toggle_selected();
    assert!(state.tree.is_expanded(&acme), "it opens again");
    assert!(
        state
            .tree
            .rows
            .iter()
            .any(|r| r.id == proj("acme", "alpha"))
    );
}

/// REGRESSION (#10476): an owner and project first seen on a later poll start
/// open; one the operator closed before stays closed when it comes back.
#[test]
fn node_added_on_a_later_poll_starts_expanded() {
    let (mut projects, mut sessions) = fixture();
    let mut state = ProjectCtlState::default();
    poll_into(&mut state, projects.clone(), sessions.clone());
    state.select_node(owner("zeta"));
    state.toggle_selected();
    let gamma = projects.remove(1);
    poll_into(&mut state, projects.clone(), sessions.clone());

    projects.push(gamma);
    projects.push(project("delta", "https://github.com/omega/delta"));
    sessions.insert(
        "delta".to_string(),
        vec![session("d1", "delta-1", "active", None)],
    );
    poll_into(&mut state, projects, sessions);
    assert!(state.tree.is_expanded(&owner("omega")));
    assert!(
        state
            .tree
            .rows
            .iter()
            .any(|r| r.id == proj("omega", "delta"))
    );
    state.select_node(proj("omega", "delta"));
    assert_eq!(
        column_ids(&state),
        ["d1"],
        "the new project's session shows"
    );
    assert!(
        !state.tree.is_expanded(&owner("zeta")),
        "a closed id stays closed when it returns"
    );
}
