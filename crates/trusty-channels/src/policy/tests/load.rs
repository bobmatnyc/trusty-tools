//! The route loader's I/O (#8454 S2 plan §4, S2b plan §2): home, the host
//! file, each project file, symlinks and the size cap. Each deny is named.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::names;
use super::repo::{
    host_yaml, mkfifo, release_fifo, running_as_root, set_mode, slack_routes, tempdir, within,
    Home, Repo,
};
use crate::policy::fs::{open_regular, ReadFault};
use crate::policy::{
    load_effective, Channel, FileState, Finding, GateError, HostError, LoadReport,
    ProjectFileError, MAX_FILE_BYTES,
};

fn host_error(report: &LoadReport) -> Option<&HostError> {
    report.findings.iter().find_map(|f| match f {
        Finding::HostRefused { error } => Some(error),
        _ => None,
    })
}

fn assert_host_denied(report: &LoadReport, want: &HostError) {
    assert!(report.denied, "not denied: {report:#?}");
    assert!(report.policy.is_empty(), "routes survived a deny");
    assert_eq!(host_error(report), Some(want), "{:#?}", report.findings);
}

pub(super) fn file_error<'a>(report: &'a LoadReport, file: &Path) -> Option<&'a ProjectFileError> {
    report.findings.iter().find_map(|f| match f {
        Finding::FileRefused { file: f, error } if f == file => Some(error),
        _ => None,
    })
}

pub(super) fn file_state(report: &LoadReport, dir: &Path) -> FileState {
    report
        .per_file
        .iter()
        .find(|s| s.project_dir == dir)
        .map(|s| s.state)
        .expect("a status per listed project")
}

/// Two repos, each with one committed Slack route, both listed.
fn two_projects() -> (Repo, Repo, Home) {
    let a = Repo::init("main");
    a.commit_routes(&slack_routes("a-dm", "U0ABCDEF1"));
    let b = Repo::init("main");
    b.commit_routes(&slack_routes("b-dm", "U0ABCDEF2"));
    let home = Home::listing(&[a.dir(), b.dir()]);
    (a, b, home)
}

#[test]
fn committed_routes_of_each_listed_project_load() {
    let (_a, _b, home) = two_projects();
    let report = load_effective(&home.request());
    assert!(!report.denied, "{:#?}", report.findings);
    assert_eq!(names(&report), ["a-dm", "b-dm"]);
    assert!(report.findings.is_empty(), "{:#?}", report.findings);
}

#[test]
fn home_unknown_denies() {
    let (_a, _b, home) = two_projects();
    let mut req = home.request();
    req.home = None;
    assert_host_denied(&load_effective(&req), &HostError::HomeUnknown);
}

#[test]
fn host_missing_denies_all() {
    let (_a, _b, home) = two_projects();
    std::fs::remove_file(home.host_path()).expect("remove host");
    assert_host_denied(&load_effective(&home.request()), &HostError::Missing);
    // A missing parent directory is the same fault.
    let (_tmp, empty) = tempdir();
    let mut req = home.request();
    req.host_path = empty.join(".trusty-tools/trusty-mpm/config.yaml");
    assert_host_denied(&load_effective(&req), &HostError::Missing);
}

#[test]
fn symlinked_file_refused() {
    let (a, b, home) = two_projects();
    let (_out, outside) = tempdir();
    // routes.toml: a symlink to a copy of the committed bytes, so only the
    // symlink check can refuse it.
    let copy = outside.join("routes.toml");
    std::fs::copy(a.routes_file(), &copy).expect("copy");
    std::fs::remove_file(a.routes_file()).expect("rm");
    symlink(&copy, a.routes_file()).expect("symlink file");
    // .trusty-channels: a symlink to a moved copy of the directory.
    let moved = outside.join("moved");
    std::fs::rename(b.dir().join(".trusty-channels"), &moved).expect("move");
    symlink(&moved, b.dir().join(".trusty-channels")).expect("symlink dir");
    let report = load_effective(&home.request());
    assert!(!report.denied, "a project fault denied all");
    assert!(names(&report).is_empty(), "{:?}", names(&report));
    assert_eq!(
        file_error(&report, &a.routes_file()),
        Some(&ProjectFileError::NotRegular {
            what: "routes.toml",
            kind: "file"
        })
    );
    assert_eq!(
        file_error(&report, &b.routes_file()),
        Some(&ProjectFileError::NotRegular {
            what: ".trusty-channels",
            kind: "directory"
        })
    );
    // The host file: a symlink to a copy of itself.
    let host_copy = outside.join("config.yaml");
    std::fs::copy(home.host_path(), &host_copy).expect("copy host");
    std::fs::remove_file(home.host_path()).expect("rm host");
    symlink(&host_copy, home.host_path()).expect("symlink host");
    assert_host_denied(
        &load_effective(&home.request()),
        &HostError::NotRegular {
            what: "config.yaml",
            kind: "file",
        },
    );
}

#[test]
fn host_config_parent_dir_symlink_denies_all() {
    let (_a, _b, home) = two_projects();
    let (_out, outside) = tempdir();
    let parent = home.host_path().parent().expect("parent").to_path_buf();
    let moved = outside.join("trusty-mpm");
    std::fs::rename(&parent, &moved).expect("move");
    symlink(&moved, &parent).expect("symlink parent");
    assert_host_denied(
        &load_effective(&home.request()),
        &HostError::NotRegular {
            what: "its parent directory",
            kind: "directory",
        },
    );
}

#[test]
fn symlinked_trusty_tools_dir_denies_all() {
    // #8454 Architect ruling: `~/.trusty-tools` is a symlink to another
    // tree holding a valid ceiling; the file and its parent are real, so
    // only the canonical-path check can refuse it.
    let (_a, _b, home) = two_projects();
    let (_out, outside) = tempdir();
    let tools = home.home.join(".trusty-tools");
    let moved = outside.join("trusty-tools");
    std::fs::rename(&tools, &moved).expect("move");
    symlink(&moved, &tools).expect("symlink .trusty-tools");
    let report = load_effective(&home.request());
    assert!(report.denied, "{:#?}", report.findings);
    assert!(report.policy.is_empty(), "routes survived a deny");
    assert!(
        matches!(
            host_error(&report),
            Some(HostError::NotUnderHome { reason }) if reason.contains("symlinked component")
        ),
        "{:#?}",
        report.findings
    );
}

#[test]
fn symlinked_project_entry_refused() {
    let (a, _b, home) = two_projects();
    let (_out, outside) = tempdir();
    let link = outside.join("link");
    symlink(a.dir(), &link).expect("symlink project");
    home.write_host(&host_yaml(&[&link]));
    let report = load_effective(&home.request());
    assert!(report.denied);
    assert!(
        matches!(
            host_error(&report),
            Some(HostError::Project { index: 0, reason, .. }) if reason.contains("canonical")
        ),
        "{:#?}",
        report.findings
    );
}

#[test]
fn committed_malformed_file_refused() {
    let (a, b, home) = two_projects();
    a.commit_routes("version = 2\n[[slack.routes]]\nname = \n");
    b.commit_routes("version = 2\n");
    std::fs::write(b.routes_file(), b"version = 2\n# \xff\n").expect("write");
    b.git(&["add", ".trusty-channels/routes.toml"]);
    b.git(&["commit", "-q", "-m", "not utf-8"]);
    let report = load_effective(&home.request());
    assert!(!report.denied, "a project fault denied all");
    assert!(matches!(
        file_error(&report, &a.routes_file()),
        Some(ProjectFileError::Parse { .. })
    ));
    assert_eq!(
        file_error(&report, &b.routes_file()),
        Some(&ProjectFileError::NotUtf8)
    );
    assert_eq!(file_state(&report, a.dir()), FileState::Refused);
}

#[test]
fn uncommitted_routes_file_is_refused_by_the_gate() {
    let (a, _b, home) = two_projects();
    std::fs::write(a.routes_file(), slack_routes("a-dm", "U0ABCDEF3")).expect("edit");
    let report = load_effective(&home.request());
    assert_eq!(names(&report), ["b-dm"]);
    assert_eq!(
        file_error(&report, &a.routes_file()),
        Some(&ProjectFileError::Gate {
            error: GateError::ContentDiffers { at: "main".into() }
        })
    );
}

#[test]
fn oversized_files_are_refused() {
    let (a, _b, home) = two_projects();
    let pad = "#".repeat(usize::try_from(MAX_FILE_BYTES).expect("fits"));
    a.commit_routes(&(slack_routes("a-dm", "U0ABCDEF1") + &pad + "\n"));
    let report = load_effective(&home.request());
    assert_eq!(names(&report), ["b-dm"]);
    assert_eq!(
        file_error(&report, &a.routes_file()),
        Some(&ProjectFileError::TooLarge {
            limit: MAX_FILE_BYTES
        })
    );
    let host = std::fs::read_to_string(home.host_path()).expect("read host");
    home.write_host(&(host + &pad + "\n"));
    assert_host_denied(
        &load_effective(&home.request()),
        &HostError::TooLarge {
            limit: MAX_FILE_BYTES,
        },
    );
}

#[test]
fn missing_project_file_is_zero_routes_with_status_missing() {
    let (a, _b, home) = two_projects();
    let empty = Repo::init("main");
    let gone = PathBuf::from(format!("{}-gone", a.dir().display()));
    home.write_host(&host_yaml(&[a.dir(), empty.dir(), &gone]));
    let report = load_effective(&home.request());
    assert!(!report.denied, "{:#?}", report.findings);
    assert_eq!(names(&report), ["a-dm"]);
    assert_eq!(file_state(&report, empty.dir()), FileState::Missing);
    assert_eq!(file_state(&report, &gone), FileState::Missing);
}

#[test]
fn project_filter_keeps_one_listed_dir() {
    let (a, b, home) = two_projects();
    let mut req = home.request();
    req.project = Some(b.dir().to_path_buf());
    let report = load_effective(&req);
    assert_eq!(names(&report), ["b-dm"]);
    assert_eq!(report.per_file.len(), 1);
    // An unlisted dir: no routes, and a named finding.
    let other = Repo::init("main");
    req.project = Some(other.dir().to_path_buf());
    let report = load_effective(&req);
    assert!(report.policy.is_empty());
    assert_eq!(
        report.findings,
        [Finding::ProjectUnlisted {
            project: other.dir().to_path_buf()
        }]
    );
    drop(a);
}

#[test]
fn consumer_channels_limit_the_routes_loaded() {
    let (_a, _b, home) = two_projects();
    let mut req = home.request();
    req.channels = vec![Channel::Gchat];
    let report = load_effective(&req);
    assert!(!report.denied);
    assert!(report.policy.is_empty(), "slack routes in a gchat load");
}

#[test]
fn host_file_not_utf8_denies_all() {
    let (_a, _b, home) = two_projects();
    std::fs::write(home.host_path(), b"channels:\n  version: 1\n# \xff\n").expect("write");
    assert_host_denied(&load_effective(&home.request()), &HostError::NotUtf8);
}

#[test]
fn unreadable_host_file_denies_all_with_read() {
    if running_as_root() {
        eprintln!("skipped: root reads a mode-000 file, so no read can fail");
        return;
    }
    let (_a, _b, home) = two_projects();
    set_mode(&home.host_path(), 0o000);
    assert_host_denied(
        &load_effective(&home.request()),
        &HostError::Read {
            reason: "permission denied".into(),
        },
    );
}

#[test]
fn unreadable_project_file_is_refused_with_read() {
    if running_as_root() {
        eprintln!("skipped: root reads a mode-000 file, so no read can fail");
        return;
    }
    let (a, _b, home) = two_projects();
    set_mode(&a.routes_file(), 0o000);
    let report = load_effective(&home.request());
    assert_eq!(names(&report), ["b-dm"]);
    assert_eq!(
        file_error(&report, &a.routes_file()),
        Some(&ProjectFileError::Read {
            reason: "permission denied".into()
        })
    );
}

#[test]
fn fifo_route_file_is_refused_without_blocking() {
    let (a, _b, home) = two_projects();
    std::fs::remove_file(a.routes_file()).expect("rm");
    mkfifo(&a.routes_file());
    // The open alone, as when a FIFO is swapped in after the lstat: it must
    // not wait for a writer.
    let path = a.routes_file();
    let opened = within(Duration::from_secs(30), move || {
        open_regular(&path, "routes.toml").map(|_| ())
    });
    let Some(opened) = opened else {
        release_fifo(&a.routes_file());
        panic!("opening a FIFO blocked past 30s");
    };
    assert!(
        matches!(
            opened,
            Err(ReadFault::NotRegular {
                what: "routes.toml",
                kind: "file"
            })
        ),
        "{opened:?}"
    );
    let report = load_effective(&home.request());
    assert_eq!(names(&report), ["b-dm"]);
    assert_eq!(
        file_error(&report, &a.routes_file()),
        Some(&ProjectFileError::NotRegular {
            what: "routes.toml",
            kind: "file"
        })
    );
}

#[test]
fn symlinked_route_file_open_is_refused_as_not_regular() {
    // #8454: the open alone, as when a symlink is swapped in after the
    // lstat: O_NOFOLLOW fails with ELOOP, and that arm names the file.
    let (_tmp, root) = tempdir();
    let target = root.join("real.toml");
    std::fs::write(&target, b"").expect("write target");
    let link = root.join("routes.toml");
    symlink(&target, &link).expect("symlink");
    let opened = open_regular(&link, "routes.toml").map(|_| ());
    assert!(
        matches!(
            opened,
            Err(ReadFault::NotRegular {
                what: "routes.toml",
                kind: "file"
            })
        ),
        "{opened:?}"
    );
}
