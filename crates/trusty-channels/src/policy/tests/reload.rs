//! Reload (#8454 S2b §3, Architect Q3): an unreviewed edit never widens the
//! policy, a narrowing applies at once, the fingerprint is content plus
//! branch, and the rate-limit logs survive.

use std::fs::File;
use std::path::Path;

use super::repo::{host_yaml, slack_routes, Home, Repo};
use super::{names, TestClock};
use crate::policy::{
    BucketDecision, FileState, Finding, GateError, LoadReport, PolicyLoader, ProjectFileError,
    RateLimit, RateLimiter,
};

fn state(report: &LoadReport, dir: &Path) -> FileState {
    report
        .per_file
        .iter()
        .find(|s| s.project_dir == dir)
        .map(|s| s.state)
        .expect("a status per listed project")
}

fn assert_stale(loader: &PolicyLoader, repo: &Repo, what: &str) {
    let report = loader.report();
    assert_eq!(names(report), ["bob-dm"], "{what}: policy changed");
    assert_eq!(state(report, repo.dir()), FileState::Stale, "{what}");
    assert!(
        report.findings.contains(&Finding::Stale {
            file: repo.routes_file()
        }),
        "{what}: {:#?}",
        report.findings
    );
    assert!(
        report.findings.iter().any(|f| matches!(
            f,
            Finding::FileRefused {
                error: ProjectFileError::Gate { .. },
                ..
            }
        )),
        "{what}: the refusal is not shown: {:#?}",
        report.findings
    );
}

fn bob() -> String {
    slack_routes("bob-dm", "U0ABCDEF1")
}

/// bob's file plus a second route, the widening an edit would add.
fn bob_and_eve() -> String {
    bob() + "\n[[slack.routes]]\nname = \"eve-dm\"\nrecipient = \"U0ABCDEF2\"\nkinds = [\"question\"]\n"
}

fn loaded() -> (Repo, Home, PolicyLoader) {
    let repo = Repo::init("main");
    repo.commit_routes(&bob());
    let home = Home::listing(&[repo.dir()]);
    let loader = PolicyLoader::new(home.request());
    assert_eq!(names(loader.report()), ["bob-dm"]);
    (repo, home, loader)
}

#[test]
fn routes_edit_outside_the_reviewed_source_has_no_effect() {
    let cases: [(&str, &[&[&str]]); 4] = [
        ("modified", &[]),
        ("staged", &[&["add", ".trusty-channels/routes.toml"]]),
        (
            "untracked",
            &[&["rm", "-q", "--cached", ".trusty-channels/routes.toml"]],
        ),
        (
            "assume-unchanged",
            &[&[
                "update-index",
                "--assume-unchanged",
                ".trusty-channels/routes.toml",
            ]],
        ),
    ];
    for (what, steps) in cases {
        let (repo, _home, mut loader) = loaded();
        if what == "assume-unchanged" {
            repo.git(steps[0]);
            repo.write_routes(&bob_and_eve());
            assert_eq!(repo.git(&["status", "--porcelain"]), "", "edit not hidden");
        } else {
            repo.write_routes(&bob_and_eve());
            for step in steps {
                repo.git(step);
            }
        }
        assert!(loader.refresh(), "{what}: the edit was not seen");
        assert_stale(&loader, &repo, what);
    }
    // Never committed: no last-good, so the file is refused, not stale.
    let repo = Repo::init("main");
    repo.write_routes(&bob());
    let home = Home::listing(&[repo.dir()]);
    let loader = PolicyLoader::new(home.request());
    assert!(loader.policy().is_empty());
    assert_eq!(state(loader.report(), repo.dir()), FileState::Refused);
}

#[test]
fn reviewed_change_takes_effect_on_refresh() {
    let (repo, _home, mut loader) = loaded();
    assert!(!loader.refresh(), "nothing changed");
    repo.commit_routes(&bob_and_eve());
    assert!(loader.refresh());
    assert_eq!(names(loader.report()), ["bob-dm", "eve-dm"]);
    // A later fault falls back to this load, the newest good one.
    repo.write_routes(&bob());
    assert!(loader.refresh());
    assert_eq!(names(loader.report()), ["bob-dm", "eve-dm"]);
    assert_eq!(state(loader.report(), repo.dir()), FileState::Stale);
}

#[test]
fn same_size_same_mtime_edit_is_seen_on_reload() {
    // An edit that keeps the length and the mtime, and moves no git ref:
    // only the content tells it apart.
    let rewrite = |path: &Path, text: &str| {
        let meta = std::fs::metadata(path).expect("meta");
        let mtime = meta.modified().expect("mtime");
        assert_eq!(meta.len(), text.len() as u64, "the edit changes the size");
        std::fs::write(path, text).expect("write");
        File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(mtime))
            .expect("set mtime");
    };
    let repo = Repo::init("main");
    repo.commit_routes(&bob());
    let home = Home::listing(&[repo.dir()]);
    // A trailing space keeps `enabled: true ` as long as `enabled: false`.
    let on = host_yaml(&[repo.dir()]).replace(
        "slack:\n    enabled: true\n",
        "slack:\n    enabled: true \n",
    );
    home.write_host(&on);
    let mut loader = PolicyLoader::new(home.request());
    assert_eq!(names(loader.report()), ["bob-dm"]);
    // The project file: an unreviewed edit is seen and shown Stale.
    rewrite(&repo.routes_file(), &slack_routes("bob-dm", "U0ABCDEF9"));
    assert!(loader.refresh(), "a same-size project edit was missed");
    assert_eq!(state(loader.report(), repo.dir()), FileState::Stale);
    // The host file: a narrowing applies at once.
    let off = on.replace("enabled: true \n", "enabled: false\n");
    rewrite(&home.host_path(), &off);
    assert!(loader.refresh(), "a same-size host edit was missed");
    assert!(
        loader.policy().is_empty(),
        "the disabled channel kept its route"
    );
}

#[test]
fn branch_switch_with_identical_bytes_regates() {
    let (repo, _home, mut loader) = loaded();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    assert!(
        loader.refresh(),
        "a branch switch with the same bytes did not re-gate"
    );
    assert_eq!(state(loader.report(), repo.dir()), FileState::Stale);
    assert!(loader.report().findings.contains(&Finding::FileRefused {
        file: repo.routes_file(),
        error: ProjectFileError::Gate {
            error: GateError::NotOnDefaultBranch {
                head: "feature".into(),
                default: "main".into(),
            },
        },
    }));
    // The default branch moving under an unchanged file also re-gates.
    repo.git(&["checkout", "-q", "main"]);
    assert!(loader.refresh());
    assert_eq!(
        state(loader.report(), repo.dir()),
        FileState::Effective { routes: 1 }
    );
    repo.git(&["commit", "-q", "--allow-empty", "-m", "unrelated"]);
    assert!(
        loader.refresh(),
        "a new default-branch commit did not re-gate"
    );
}

#[test]
fn reload_keeps_rate_limit_logs() {
    let repo = Repo::init("main");
    let limited = |extra: &str| {
        slack_routes("bob-dm", "U0ABCDEF1")
            + "rate_limit = { limit = 2, window_secs = 60 }\n"
            + extra
    };
    repo.commit_routes(&limited(""));
    let home = Home::listing(&[repo.dir()]);
    let mut loader = PolicyLoader::new(home.request());
    let clock = TestClock::default();
    let mut limiter = RateLimiter::new(RateLimit::DEFAULT, clock.clone());
    let route = loader.policy().routes()[0].clone();
    assert_eq!(limiter.take_route(&route), BucketDecision::Admit);
    assert_eq!(limiter.take_route(&route), BucketDecision::Admit);
    assert_eq!(limiter.take_route(&route), BucketDecision::Exhausted);
    repo.commit_routes(&limited("\n[[slack.routes]]\nname = \"eve-dm\"\nrecipient = \"U0ABCDEF2\"\nkinds = [\"question\"]\n"));
    assert!(loader.refresh(), "the reviewed change did not reload");
    assert_eq!(names(loader.report()), ["bob-dm", "eve-dm"]);
    let route = loader.policy().routes()[0].clone();
    clock.set_ms(1_000);
    assert_eq!(
        limiter.take_route(&route),
        BucketDecision::Exhausted,
        "the reload reset the flood's window"
    );
}

#[test]
fn adding_a_project_needs_no_code_change() {
    let (a, home, mut loader) = loaded();
    let b = Repo::init("main");
    b.commit_routes(&slack_routes("b-dm", "U0ABCDEF5"));
    home.write_host(&host_yaml(&[a.dir(), b.dir()]));
    assert!(loader.refresh());
    assert_eq!(names(loader.report()), ["bob-dm", "b-dm"]);
}

#[test]
fn host_fault_on_reload_denies_all_at_once() {
    let (repo, home, mut loader) = loaded();
    home.write_host("channels:\n  version: 1\n  surprise: true\n");
    assert!(loader.refresh());
    assert!(loader.report().denied);
    assert!(loader.policy().is_empty(), "a host fault kept routes");
    // The host fault cleared last-good: a file fault now refuses outright.
    home.write_host(&host_yaml(&[repo.dir()]));
    repo.write_routes(&bob_and_eve());
    assert!(loader.refresh());
    assert!(loader.policy().is_empty());
    assert_eq!(state(loader.report(), repo.dir()), FileState::Refused);
}

#[test]
fn valid_narrowing_reload_applies_at_once() {
    let (repo, home, mut loader) = loaded();
    // The host turns Slack off: the route goes now, no Stale.
    home.write_host(
        &host_yaml(&[repo.dir()])
            .replace("slack:\n    enabled: true", "slack:\n    enabled: false"),
    );
    assert!(loader.refresh());
    assert!(
        loader.policy().is_empty(),
        "a disabled channel kept its route"
    );
    home.write_host(&host_yaml(&[repo.dir()]));
    assert!(loader.refresh());
    assert_eq!(names(loader.report()), ["bob-dm"]);
    // A reviewed commit removing the route applies at once.
    repo.commit_routes("version = 2\n");
    assert!(loader.refresh());
    assert!(loader.policy().is_empty());
    assert_eq!(
        state(loader.report(), repo.dir()),
        FileState::Effective { routes: 0 }
    );
}
