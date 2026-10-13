//! #9832: the commit a load verified, and who last changed the routes there.
//!
//! Why: the route-change audit names the commit and the author of the last
//! routes change at it (A1). A lookup that reads the branch tip's author, or
//! that lets a caller pass an option to git, records the wrong person.
//! What: real temp repos with three authors; a PATH with no git proves a
//! refused id never reaches git.

use std::ffi::OsString;

use super::repo::{slack_routes, tempdir, Home, Repo};
use crate::policy::gate::with_git_path;
use crate::policy::{commit_author, load_effective, FileState, GateError, PolicyLoader};

fn bob() -> String {
    slack_routes("bob-dm", "U0ABCDEF1")
}

fn eve() -> String {
    slack_routes("eve-dm", "U0ABCDEF2")
}

/// Commit the routes file as `who`.
fn commit_routes_as(repo: &Repo, text: &str, who: &str) {
    repo.write_routes(text);
    repo.git(&["add", ".trusty-channels/routes.toml"]);
    let name = format!("user.name={who}");
    repo.git(&["-c", &name, "commit", "-q", "-m", "routes"]);
}

#[test]
fn verified_commit_is_the_gated_commit() {
    let repo = Repo::init("main");
    commit_routes_as(&repo, &bob(), "Alice");
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let home = Home::listing(&[repo.dir()]);
    let report = load_effective(&home.request());
    assert!(matches!(
        report.per_file[0].state,
        FileState::Effective { .. }
    ));
    assert_eq!(report.per_file[0].verified_commit.as_deref(), Some(&*tip));

    // An unreviewed edit is refused: no commit vouches for the bytes.
    repo.write_routes(&eve());
    let refused = load_effective(&home.request());
    assert_eq!(refused.per_file[0].state, FileState::Refused);
    assert_eq!(refused.per_file[0].verified_commit, None);

    // A reloader keeping last-good routes over that refusal names no commit.
    repo.write_routes(&bob());
    let mut loader = PolicyLoader::new(home.request());
    assert!(loader.report().per_file[0].verified_commit.is_some());
    repo.write_routes(&eve());
    assert!(loader.refresh());
    assert_eq!(loader.report().per_file[0].state, FileState::Stale);
    assert_eq!(loader.report().per_file[0].verified_commit, None);
}

#[test]
fn commit_author_reads_author_and_time_of_last_routes_commit() {
    let repo = Repo::init("main");
    commit_routes_as(&repo, &bob(), "Alice");
    commit_routes_as(&repo, &eve(), "Bob");
    let bob_time: i64 = repo
        .git(&["log", "-1", "--format=%ct"])
        .parse()
        .expect("%ct");
    std::fs::write(repo.dir().join("README"), "x").expect("write");
    repo.git(&["add", "README"]);
    repo.git(&["-c", "user.name=Carol", "commit", "-q", "-m", "readme"]);
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let author = commit_author(repo.dir(), &tip).expect("an author");
    assert_eq!(author.name, "Bob", "not the routes change's author");
    assert_eq!(author.time, bob_time);
}

#[test]
fn commit_author_refuses_non_hex_commit() {
    let repo = Repo::init("main");
    commit_routes_as(&repo, &bob(), "Alice");
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let (_tmp, empty) = tempdir();
    let bad = [
        "HEAD".to_string(),
        "--output=/tmp/x".to_string(),
        String::new(),
        tip.to_uppercase(),
        tip[..39].to_string(),
        format!("{tip} "),
    ];
    for id in bad {
        // No git on PATH: a spawn would answer GitUnavailable, not GitFailed.
        let got = with_git_path(OsString::from(&empty), || commit_author(repo.dir(), &id));
        assert!(
            matches!(got, Err(GateError::GitFailed { .. })),
            "{id:?}: {got:?}"
        );
    }
}

#[test]
fn missing_commit_object_refuses() {
    let repo = Repo::init("main");
    let root = repo.git(&["rev-parse", "HEAD"]);
    commit_routes_as(&repo, &bob(), "Alice");
    let absent = "1".repeat(root.len());
    assert!(commit_author(repo.dir(), &absent).is_err(), "absent object");
    // The root commit predates the routes file: no change to name.
    assert!(
        commit_author(repo.dir(), &root).is_err(),
        "no routes change"
    );
}
