//! Tests for the tree model (#10476): owner parsing, counters, collapse rules
//! and worker indentation.

use super::*;

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

fn all_expanded() -> BTreeSet<NodeId> {
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
    let rows = build_tree(&projects, &sessions, &all_expanded(), true, &aliases());
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
    let rows = build_tree(&projects, &sessions, &BTreeSet::new(), true, &aliases());
    assert_eq!(
        ids(&rows),
        vec![owner("acme"), owner("zeta"), owner(LOCAL_OWNER)]
    );
}

#[test]
fn collapsed_owner_hides_projects() {
    let (projects, sessions) = fixture();
    let mut expanded = all_expanded();
    expanded.remove(&owner("acme"));
    let rows = build_tree(&projects, &sessions, &expanded, true, &aliases());
    assert!(!rows.iter().any(|r| r.id.project_name() == Some("alpha")));
    assert!(!rows.iter().any(|r| r.id.project_name() == Some("beta")));
    assert!(rows.iter().any(|r| r.id == proj("zeta", "gamma")));
}

#[test]
fn collapsed_project_hides_sessions() {
    let (projects, sessions) = fixture();
    let mut expanded = all_expanded();
    expanded.remove(&proj("acme", "alpha"));
    let rows = build_tree(&projects, &sessions, &expanded, true, &aliases());
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
    let expanded = [owner("acme"), proj("acme", "alpha")].into_iter().collect();
    let rows = build_tree(&projects, &by_project, &expanded, true, &aliases());
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
    let expanded = [owner("acme"), proj("acme", "alpha")].into_iter().collect();
    let rows = build_tree(&projects, &by_project, &expanded, false, &aliases());
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
