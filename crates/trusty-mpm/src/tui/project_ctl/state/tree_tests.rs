//! Tests for the tree model (#10476): owner parsing, counters, collapse rules
//! and worker indentation.

use super::*;
use crate::tui::project_ctl::state::ProjectCtlState;

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

#[test]
fn parent_chain_walks_to_the_owner() {
    let s = NodeId::Session {
        owner: "acme".to_string(),
        project: "alpha".to_string(),
        id: "a1".to_string(),
    };
    assert_eq!(s.parent(), Some(proj("acme", "alpha")));
    assert_eq!(proj("acme", "alpha").parent(), Some(owner("acme")));
    assert_eq!(owner("acme").parent(), None);
}

#[test]
fn counts_total_and_live_per_project_and_owner() {
    let (projects, sessions) = fixture();
    let rows = build_tree(&projects, &sessions, &BTreeSet::new(), true, &aliases());
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
            expanded: true,
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
    let rows = build_tree(&projects, &sessions, &all_nodes(), true, &aliases());
    assert_eq!(
        ids(&rows),
        vec![owner("acme"), owner("zeta"), owner(LOCAL_OWNER)]
    );
}

#[test]
fn collapsed_owner_hides_projects() {
    let (projects, sessions) = fixture();
    let collapsed = [owner("acme")].into_iter().collect();
    let rows = build_tree(&projects, &sessions, &collapsed, true, &aliases());
    assert!(!rows.iter().any(|r| r.id.project_name() == Some("alpha")));
    assert!(!rows.iter().any(|r| r.id.project_name() == Some("beta")));
    assert!(rows.iter().any(|r| r.id == proj("zeta", "gamma")));
}

#[test]
fn collapsed_project_hides_sessions() {
    let (projects, sessions) = fixture();
    let collapsed = [proj("acme", "alpha")].into_iter().collect();
    let rows = build_tree(&projects, &sessions, &collapsed, true, &aliases());
    assert!(rows.iter().any(|r| r.id == proj("acme", "alpha")));
    assert!(
        !rows
            .iter()
            .any(|r| matches!(&r.id, NodeId::Session { project, .. } if project == "alpha"))
    );
    assert!(
        rows.iter()
            .any(|r| matches!(&r.id, NodeId::Session { id, .. } if id == "b1"))
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

    let projects = vec![project("alpha", "https://github.com/acme/alpha")];
    let by_project: BTreeMap<_, _> = [("alpha".to_string(), sessions)].into_iter().collect();
    let rows = build_tree(&projects, &by_project, &BTreeSet::new(), true, &aliases());
    assert!(rows.iter().any(|r| r.id
        == NodeId::Anchor {
            owner: "acme".to_string(),
            project: "alpha".to_string(),
            parent: "gone".to_string(),
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
    let projects = vec![project("alpha", "https://github.com/acme/alpha")];
    let by_project: BTreeMap<_, _> = [("alpha".to_string(), sessions)].into_iter().collect();
    let rows = build_tree(&projects, &by_project, &BTreeSet::new(), false, &aliases());
    let depths: Vec<_> = rows
        .iter()
        .filter_map(|r| match &r.kind {
            TreeRowKind::Session { index, depth, .. } => Some((*index, *depth)),
            TreeRowKind::Anchor { .. } => panic!("no anchor without indentation"),
            _ => None,
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
    let rows = build_tree(
        &projects,
        &BTreeMap::new(),
        &BTreeSet::new(),
        true,
        &aliases(),
    );
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
        true,
        &aliases(),
    );
    let second = build_tree(
        &[lower.clone(), upper.clone()],
        &empty,
        &collapsed,
        true,
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
    let first = build_tree(
        &[upper.clone(), lower.clone()],
        &empty,
        &open,
        true,
        &aliases(),
    );
    let second = build_tree(&[lower, upper], &empty, &open, true, &aliases());
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

/// True when every owner and project row of `rows` draws open.
fn all_rows_open(rows: &[TreeRow]) -> bool {
    rows.iter().all(|r| match r.kind {
        TreeRowKind::Owner { expanded, .. } | TreeRowKind::Project { expanded, .. } => expanded,
        _ => true,
    })
}

/// REGRESSION (#10476, Bob 11-05 17:30Z): the first poll drew only owner bars,
/// so no session showed until the operator opened each node.
#[test]
fn fresh_state_shows_every_node_expanded_with_sessions() {
    let (projects, sessions) = fixture();
    let mut state = ProjectCtlState::default();
    poll_into(&mut state, projects, sessions);
    let rows = &state.tree.rows;
    for node in all_nodes() {
        assert!(rows.iter().any(|r| r.id == node), "{node:?} is drawn");
        assert!(state.tree.is_expanded(&node), "{node:?} starts open");
    }
    assert!(all_rows_open(rows), "{rows:?}");
    let shown: Vec<&str> = rows
        .iter()
        .filter_map(|r| match &r.id {
            NodeId::Session { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        shown,
        ["a1", "a2", "a3", "b1"],
        "session rows on first render"
    );
}

/// REGRESSION (#10476): a node the operator closes stays closed across
/// repolls, and opening it again works.
#[test]
fn collapse_survives_a_rebuild() {
    let (projects, sessions) = fixture();
    let mut state = ProjectCtlState::default();
    poll_into(&mut state, projects.clone(), sessions.clone());
    let alpha = proj("acme", "alpha");
    assert!(state.tree.is_expanded(&alpha), "alpha starts open");

    state.select_node(alpha.clone());
    state.toggle_selected();
    poll_into(&mut state, projects.clone(), sessions.clone());
    poll_into(&mut state, projects, sessions);
    assert!(!state.tree.is_expanded(&alpha), "the collapse survives");
    assert!(
        !state
            .tree
            .rows
            .iter()
            .any(|r| matches!(&r.id, NodeId::Session { project, .. } if project == "alpha"))
    );
    assert_eq!(state.selected_node(), Some(&alpha));

    state.toggle_selected();
    assert!(state.tree.is_expanded(&alpha), "it opens again");
    assert!(
        state
            .tree
            .rows
            .iter()
            .any(|r| matches!(&r.id, NodeId::Session { id, .. } if id == "a1"))
    );
}

/// REGRESSION (#10476): an owner and project first seen on a later poll start
/// open; one the operator closed before stays closed when it comes back.
#[test]
fn node_added_on_a_later_poll_starts_expanded() {
    let (mut projects, mut sessions) = fixture();
    let mut state = ProjectCtlState::default();
    poll_into(&mut state, projects.clone(), sessions.clone());
    state.select_node(proj("acme", "beta"));
    state.toggle_selected();
    let beta = projects.remove(2);
    poll_into(&mut state, projects.clone(), sessions.clone());

    projects.push(beta);
    projects.push(project("delta", "https://github.com/omega/delta"));
    sessions.insert(
        "delta".to_string(),
        vec![session("d1", "delta-1", "active", None)],
    );
    poll_into(&mut state, projects, sessions);
    assert!(state.tree.is_expanded(&owner("omega")));
    assert!(state.tree.is_expanded(&proj("omega", "delta")));
    assert!(
        state
            .tree
            .rows
            .iter()
            .any(|r| matches!(&r.id, NodeId::Session { id, .. } if id == "d1")),
        "the new project's session shows"
    );
    assert!(
        !state.tree.is_expanded(&proj("acme", "beta")),
        "a closed id stays closed when it returns"
    );
}
