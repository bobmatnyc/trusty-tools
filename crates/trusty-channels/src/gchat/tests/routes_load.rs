//! Route loading: the schema-1 load rules and the git load gate (E1).

use std::path::Path;

use wiremock::MockServer;

use super::{all_requests, git, pem_line, Project, JANET, THREE_ROUTES};
use crate::gchat::channel::LoadStatus;
use crate::gchat::error::{RouteError, SendError};
use crate::gchat::load_gate::check_committed;
use crate::gchat::routes::{load_routes, parse_routes, MessageKind};

fn parse(text: &str) -> Result<crate::gchat::routes::RouteTable, RouteError> {
    parse_routes(Path::new("routes.toml"), text, Some(Path::new("/home/t")))
}

const CONN: &str = "version = 1\n[gchat.connection]\nproject_id = \"p\"\n\
                    subscription = \"s\"\nkey_file = \"/k.json\"\n";

fn route(name: &str, recipient: &str, kinds: &str) -> String {
    format!("[[gchat.routes]]\nname = \"{name}\"\nrecipient = \"{recipient}\"\nkinds = {kinds}\n")
}

#[test]
fn missing_file_is_zero_routes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let table = load_routes(dir.path()).expect("a missing file is not an error");
    assert!(table.routes.is_empty());
    assert!(!table.file_present);
}

#[tokio::test]
async fn missing_file_refuses_every_send() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let channel = crate::gchat::GchatChannel::open_with(
        dir.path(),
        crate::gchat::api::client::Endpoints::single_host(&server.uri()),
    )
    .expect("open");
    assert_eq!(channel.load_status(), LoadStatus::Missing);
    let err = channel
        .send_question(JANET, "q")
        .await
        .expect_err("refused");
    assert!(matches!(err, SendError::NoRoute { .. }), "{err:?}");
    assert_eq!(all_requests(&server).await, 0);
}

#[test]
fn load_gate_refuses_untracked_modified_and_staged() {
    let project = Project::uncommitted(THREE_ROUTES);
    let dir = project.dir();
    let refused = |what: &str| {
        let err = load_routes(dir).expect_err(what);
        assert!(
            matches!(err, RouteError::NotCommitted { .. }),
            "{what}: {err:?}"
        );
    };
    refused("untracked");
    git(dir, &["add", ".trusty-channels/routes.toml"]);
    refused("staged new file, never committed");
    git(dir, &["commit", "-q", "-m", "routes"]);
    assert_eq!(
        load_routes(dir).expect("committed and clean").routes.len(),
        3
    );

    let edited = std::fs::read_to_string(project.routes_file()).expect("read") + "\n# edit\n";
    std::fs::write(project.routes_file(), edited).expect("edit");
    refused("tracked but modified");
    git(dir, &["add", ".trusty-channels/routes.toml"]);
    refused("staged but uncommitted");
    git(dir, &["commit", "-q", "-m", "edit"]);
    assert!(load_routes(dir).is_ok(), "committed edit must load");

    // Outside any git repo: refused, never loaded.
    let bare = tempfile::tempdir().expect("tempdir");
    let config = bare.path().join(".trusty-channels");
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::copy(project.routes_file(), config.join("routes.toml")).expect("copy");
    let err = load_routes(bare.path()).expect_err("no repo");
    assert!(matches!(err, RouteError::NotCommitted { .. }), "{err:?}");
}

#[tokio::test]
async fn refused_load_refuses_every_send() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    std::fs::write(project.routes_file(), THREE_ROUTES).expect("uncommitted edit");
    let channel = project.channel(&server);
    assert!(matches!(channel.load_status(), LoadStatus::Refused { .. }));
    let err = channel
        .send_question("janet", "Ship it?")
        .await
        .expect_err("refused");
    assert!(
        matches!(err, SendError::RoutesUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(all_requests(&server).await, 0, "a request left the gate");
    let lines = project.audit_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["reason"], "routes_unavailable");
}

#[test]
fn load_rules_refuse_each_invalid_file() {
    let janet = route("janet", JANET, "[\"question\"]");
    let cases: Vec<(&str, String)> = vec![
        (
            "version 2",
            format!("{}{janet}", CONN.replace("version = 1", "version = 2")),
        ),
        ("unknown top-level key", format!("{CONN}extra = 1\n{janet}")),
        (
            "unknown connection key",
            format!(
                "{}{janet}",
                CONN.replace("key_file", "token = \"x\"\nkey_file")
            ),
        ),
        (
            "unknown gchat key",
            format!("version = 1\n[gchat]\nmode = \"x\"\n{}{janet}", &CONN[12..]),
        ),
        (
            "unknown route key",
            format!(
                "{CONN}{}",
                janet.replace("kinds", "space = \"spaces/A\"\nkinds")
            ),
        ),
        (
            "empty kinds",
            format!("{CONN}{}", route("janet", JANET, "[]")),
        ),
        (
            "unknown kind",
            format!("{CONN}{}", route("janet", JANET, "[\"chat\"]")),
        ),
        (
            "recipient not an email",
            format!("{CONN}{}", route("janet", "janet", "[\"question\"]")),
        ),
        (
            "recipient with a space",
            format!(
                "{CONN}{}",
                route("janet", "ja net@example.com", "[\"question\"]")
            ),
        ),
        (
            "recipient without a dotted domain",
            format!("{CONN}{}", route("j", "j@host", "[\"question\"]")),
        ),
        (
            "credential in key_file",
            CONN.replace("/k.json", &pem_line("BEGIN", "PRIVATE KEY")) + &janet,
        ),
        (
            "subscription as a path",
            CONN.replace("\"s\"", "\"projects/p/subscriptions/s\"") + &janet,
        ),
        (
            "version as a string",
            CONN.replace("version = 1", "version = \"1\"") + &janet,
        ),
    ];
    for (what, text) in cases {
        assert!(parse(&text).is_err(), "{what} loaded:\n{text}");
    }
    assert!(
        matches!(
            parse(&format!(
                "{}{janet}",
                CONN.replace("version = 1", "version = 2")
            )),
            Err(RouteError::Version { found: 2 })
        ),
        "version error is not typed"
    );
}

#[test]
fn duplicates_name_both_entries() {
    let text = format!(
        "{CONN}{}{}{}",
        route("janet", JANET, "[\"question\"]"),
        route("other", "x@example.com", "[\"question\"]"),
        route("janet", "y@example.com", "[\"question\"]"),
    );
    let msg = parse(&text).expect_err("duplicate name").to_string();
    assert!(msg.contains(r#"gchat.routes[0] "janet""#), "{msg}");
    assert!(msg.contains(r#"gchat.routes[2] "janet""#), "{msg}");

    let text = format!(
        "{CONN}{}{}",
        route("a", "Janet@Example.com", "[\"question\"]"),
        route("b", JANET, "[\"review_notice\"]"),
    );
    let err = parse(&text).expect_err("duplicate recipient");
    assert!(
        matches!(
            &err,
            RouteError::Duplicate {
                field: "recipient",
                ..
            }
        ),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains(r#"gchat.routes[0] "a""#) && msg.contains(r#"gchat.routes[1] "b""#),
        "{msg}"
    );
}

#[test]
fn valid_file_loads_routes_in_order() {
    let text = CONN.replace("/k.json", "~/keys/sa.json")
        + &route(
            "janet",
            "Janet@Example.com",
            "[\"question\", \"review_notice\", \"question\"]",
        )
        + &route("notices", "rev@example.com", "[\"review_notice\"]");
    let table = parse(&text).expect("valid");
    let conn = table.connection.as_ref().expect("connection");
    assert_eq!(conn.key_file, Path::new("/home/t/keys/sa.json"));
    assert_eq!(conn.subscription_name(), "projects/p/subscriptions/s");
    let names: Vec<&str> = table.routes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["janet", "notices"]);
    assert_eq!(table.routes[0].recipient, JANET, "recipient is lowercased");
    assert_eq!(table.routes[0].kinds.len(), 2);
    assert!(!table.routes[1].allows(MessageKind::Question));
    assert!(parse("version = 1\n")
        .expect("no gchat table")
        .routes
        .is_empty());
}

/// `git status --porcelain` for the routes file: what the old gate read.
fn porcelain(dir: &Path) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "status",
            "--porcelain=v1",
            "--",
            ".trusty-channels/routes.toml",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git status");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The routes file with one extra, never-reviewed route appended.
fn with_mallory(project: &Project) -> String {
    std::fs::read_to_string(project.routes_file()).expect("read")
        + &route("mallory", "mallory@example.com", "[\"question\"]")
}

#[test]
fn load_gate_refuses_edits_hidden_by_assume_unchanged_or_skip_worktree() {
    for flag in ["--skip-worktree", "--assume-unchanged"] {
        let project = Project::committed(THREE_ROUTES);
        let dir = project.dir();
        git(dir, &["update-index", flag, ".trusty-channels/routes.toml"]);
        std::fs::write(project.routes_file(), with_mallory(&project)).expect("edit");
        assert_eq!(porcelain(dir), "", "{flag} did not hide the edit");
        let err = load_routes(dir).expect_err(flag);
        assert!(
            matches!(err, RouteError::NotCommitted { .. }),
            "{flag}: {err:?}"
        );
    }
}

#[test]
fn load_gate_checks_the_bytes_read_not_the_file_after() {
    let project = Project::committed(THREE_ROUTES);
    let dir = project.dir();
    let path = project.routes_file();
    std::fs::write(&path, with_mallory(&project)).expect("edit");
    let dirty = std::fs::read(&path).expect("read dirty");
    // The file is reverted between the read and the gate.
    git(dir, &["checkout", "--", ".trusty-channels/routes.toml"]);
    let err = check_committed(&path, &dirty)
        .expect_err("the gate passed while the caller holds uncommitted bytes");
    assert!(matches!(err, RouteError::NotCommitted { .. }), "{err:?}");
    let clean = std::fs::read(&path).expect("read clean");
    check_committed(&path, &clean).expect("the committed bytes pass");
}
