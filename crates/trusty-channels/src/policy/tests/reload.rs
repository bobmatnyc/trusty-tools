//! Reload (#8454 S2b §3, Architect Q3): an unreviewed edit never widens the
//! policy, a narrowing applies at once, the fingerprint is content plus
//! branch, and the rate-limit logs survive. #9808: the fingerprint a load
//! keeps describes the bytes that load read, or matches nothing.

use std::fs::File;
use std::os::unix::fs::symlink;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use super::repo::{host_yaml, slack_routes, tempdir, Home, Repo};
use super::{names, TestClock};
use crate::policy::load::{prepare, Prepared};
use crate::policy::{
    load_effective, BucketDecision, Channel, FileState, Finding, GateError, HostError, LoadReport,
    LoadRequest, PolicyLoader, ProjectFileError, RateLimit, RateLimiter,
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
fn trusty_tools_becoming_a_symlink_denies_all_on_refresh() {
    // #8454 Architect ruling: the host file's bytes do not change, so only
    // the canonical-path check in the fingerprint can see the swap.
    let (_repo, home, mut loader) = loaded();
    let (_out, outside) = tempdir();
    let tools = home.home.join(".trusty-tools");
    let moved = outside.join("trusty-tools");
    std::fs::rename(&tools, &moved).expect("move");
    symlink(&moved, &tools).expect("symlink .trusty-tools");
    assert!(loader.refresh(), "the swap to a symlink was not seen");
    let report = loader.report();
    assert!(report.denied, "{:#?}", report.findings);
    assert!(loader.policy().is_empty(), "a host fault kept routes");
    assert!(
        report.findings.iter().any(|f| matches!(
            f,
            Finding::HostRefused {
                error: HostError::NotUnderHome { reason },
            } if reason.contains("symlinked component")
        )),
        "{:#?}",
        report.findings
    );
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

#[test]
fn host_fault_on_an_unserved_entry_denies_all_on_refresh() {
    // #8454 Q3: a `projects` entry that fails the canonical check denies
    // all, even on a channel the consumer does not serve or behind the
    // project filter; refresh must reach the fresh load's verdict.
    let (repo, home, mut daemon) = loaded();
    let (_tmp, root) = tempdir();
    let g = root.join("g");
    std::fs::create_dir(&g).expect("mkdir");
    home.write_host(&format!(
        "channels:\n  version: 1\n  gchat:\n    enabled: true\n    projects: [\"{g}\", \"{r}\"]\n  \
         slack:\n    enabled: true\n    projects: [\"{r}\"]\n",
        g = g.display(),
        r = repo.dir().display(),
    ));
    assert!(daemon.refresh());
    assert_eq!(names(daemon.report()), ["bob-dm"]);
    // gchat-mcp's shape: gchat only, filtered to the repo, so `g` is out.
    let mut req = home.request();
    req.channels = vec![Channel::Gchat];
    req.project = Some(repo.dir().to_path_buf());
    let mut filtered = PolicyLoader::new(req);
    assert!(
        !filtered.report().denied,
        "{:#?}",
        filtered.report().findings
    );
    // `g` becomes a symlink; the host file's text does not change.
    let moved = root.join("g-real");
    std::fs::rename(&g, &moved).expect("move");
    symlink(&moved, &g).expect("symlink");
    assert!(
        load_effective(&home.request()).denied,
        "a fresh load denies"
    );
    assert!(daemon.refresh(), "the daemon missed the host fault");
    assert!(daemon.report().denied);
    assert!(daemon.policy().is_empty(), "a host fault kept routes");
    assert!(
        filtered.refresh(),
        "the filtered loader missed the host fault"
    );
    assert!(filtered.report().denied);
}

/// 🔴 #8454 VC (R1): a loader stamps a nonzero digest of its source
/// fingerprint; a no-op refresh keeps it and a reload moves it, so a cache
/// keyed on it sees every reload.
#[test]
fn loader_report_carries_a_fingerprint_that_follows_its_sources() {
    let (repo, home, mut loader) = loaded();
    let first = loader.report().fingerprint;
    assert_ne!(first, 0, "a loader report must carry a fingerprint");
    assert!(!loader.refresh(), "nothing changed");
    assert_eq!(
        loader.report().fingerprint,
        first,
        "a no-op refresh moved it"
    );
    repo.commit_routes(&bob_and_eve());
    assert!(loader.refresh());
    let second = loader.report().fingerprint;
    assert_ne!(second, 0);
    assert_ne!(second, first, "a reviewed change kept the old fingerprint");
    // A host fault is a reload too: the denied report is stamped as well.
    home.write_host("channels:\n  version: 1\n  surprise: true\n");
    assert!(loader.refresh());
    assert!(loader.report().denied);
    assert_ne!(loader.report().fingerprint, 0);
    assert_ne!(loader.report().fingerprint, second);
}

/// 🔴 #8454 VC (R1): a one-shot load has no loader fingerprint, so it is 0,
/// the value a cache must never key on.
#[test]
fn one_shot_load_has_no_fingerprint() {
    let repo = Repo::init("main");
    repo.commit_routes(&bob());
    let home = Home::listing(&[repo.dir()]);
    let report = load_effective(&home.request());
    assert_eq!(names(&report), ["bob-dm"]);
    assert_eq!(report.fingerprint, 0);
}

// ── #9808: the fingerprint describes the bytes the load read ──

/// A read that sees a change the sources drop again before the next
/// refresh: `change` runs, the real [`prepare`] reads, `revert` runs.
fn flip_during_load<'a>(
    change: impl FnOnce() + 'a,
    revert: impl FnOnce() + 'a,
) -> impl FnOnce(&LoadRequest) -> Prepared + 'a {
    move |req| {
        change();
        let read = prepare(req);
        revert();
        read
    }
}

/// Commit eve's route, then `git reset --hard` back to the same `HEAD`.
fn eve_then_back(repo: &Repo) -> impl FnOnce(&LoadRequest) -> Prepared + '_ {
    flip_during_load(
        || repo.commit_routes(&bob_and_eve()),
        || {
            repo.git(&["reset", "--hard", "-q", "HEAD~1"]);
        },
    )
}

/// The reverted change is reloaded on the next refresh.
fn assert_reverted_reloads(loader: &mut PolicyLoader) {
    assert_eq!(
        names(loader.report()),
        ["bob-dm", "eve-dm"],
        "the load did not read the change"
    );
    assert!(
        loader.refresh(),
        "#9808: the change reverted after the read was never reloaded"
    );
    assert_eq!(names(loader.report()), ["bob-dm"]);
}

/// A home listing `repo` on Slack and dir `g` on gchat only: an entry the
/// daemon does not serve, whose canonical check still denies all.
struct Unserved {
    _tmp: tempfile::TempDir,
    g: PathBuf,
    real: PathBuf,
}

impl Unserved {
    fn listed(home: &Home, repo: &Repo) -> Self {
        let (tmp, root) = tempdir();
        let g = root.join("g");
        std::fs::create_dir(&g).expect("mkdir");
        home.write_host(&format!(
            "channels:\n  version: 1\n  gchat:\n    enabled: true\n    projects: [\"{g}\"]\n  \
             slack:\n    enabled: true\n    projects: [\"{r}\"]\n",
            g = g.display(),
            r = repo.dir().display(),
        ));
        let real = root.join("g-real");
        Self { _tmp: tmp, g, real }
    }

    /// `g` becomes a symlink: the canonical check fails.
    fn symlink(&self) {
        std::fs::rename(&self.g, &self.real).expect("move");
        symlink(&self.real, &self.g).expect("symlink");
    }

    /// `g` is a real directory again.
    fn restore(&self) {
        std::fs::remove_file(&self.g).expect("unlink");
        std::fs::rename(&self.real, &self.g).expect("move back");
    }
}

#[test]
fn refresh_after_change_and_revert_during_load_reloads() {
    let (repo, _home, mut loader) = loaded();
    // A new commit the refresh must load; the load then sees eve's route.
    repo.git(&["commit", "-q", "--allow-empty", "-m", "unrelated"]);
    assert!(loader.refresh_with(eve_then_back(&repo)));
    assert_reverted_reloads(&mut loader);
}

#[test]
fn new_after_change_and_revert_during_load_reloads() {
    let repo = Repo::init("main");
    repo.commit_routes(&bob());
    let home = Home::listing(&[repo.dir()]);
    let mut loader = PolicyLoader::new_with(home.request(), eve_then_back(&repo));
    assert_reverted_reloads(&mut loader);
}

#[test]
fn reload_after_change_and_revert_during_load_reloads() {
    let (repo, _home, mut loader) = loaded();
    loader.reload_with(eve_then_back(&repo));
    assert_reverted_reloads(&mut loader);
}

#[test]
fn host_file_change_and_revert_during_load_reloads() {
    let repo = Repo::init("main");
    repo.commit_routes(&bob());
    let home = Home::listing(&[repo.dir()]);
    let on = host_yaml(&[repo.dir()]);
    let off = on.replace("slack:\n    enabled: true", "slack:\n    enabled: false");
    home.write_host(&off);
    let mut loader = PolicyLoader::new(home.request());
    assert!(loader.policy().is_empty(), "Slack is off");
    // Only the host file moves; the project bytes read match the post sample.
    loader.reload_with(flip_during_load(
        || home.write_host(&on),
        || home.write_host(&off),
    ));
    assert_eq!(names(loader.report()), ["bob-dm"], "the load read Slack on");
    assert!(
        loader.refresh(),
        "#9808: the host file reverted after the read was never reloaded"
    );
    assert!(
        loader.policy().is_empty(),
        "the disabled channel kept its route"
    );
}

#[test]
fn canonical_fault_cleared_and_restored_during_load_reloads() {
    let (repo, home, mut loader) = loaded();
    let g = Unserved::listed(&home, &repo);
    g.symlink();
    assert!(loader.refresh());
    assert!(
        loader.report().denied,
        "the non-canonical entry did not deny"
    );
    // The host text and the project bytes stay the same; only the
    // canonical verdict the load saw differs from the post sample.
    loader.reload_with(flip_during_load(|| g.restore(), || g.symlink()));
    assert_eq!(names(loader.report()), ["bob-dm"], "the load saw no fault");
    assert!(
        loader.refresh(),
        "#9808: the fault restored after the check was never reloaded"
    );
    assert!(loader.report().denied);
    assert!(loader.policy().is_empty(), "a host fault kept routes");
}

#[test]
fn project_not_read_by_the_load_never_settles() {
    // The shape of a load whose deadline passed before a project's read.
    let (_repo, _home, mut loader) = loaded();
    loader.reload_with(|req| {
        let mut read = prepare(req);
        read.projects.clear();
        read
    });
    assert!(
        loader.policy().is_empty(),
        "the skipped project kept routes"
    );
    assert!(
        loader.refresh(),
        "#9808: a load that skipped a listed project settled"
    );
    assert_eq!(names(loader.report()), ["bob-dm"]);
}

#[test]
fn panic_during_load_leaves_a_loader_that_reloads() {
    let (repo, _home, loader) = loaded();
    repo.commit_routes(&bob_and_eve());
    let mut kept = loader.clone();
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        kept.refresh_with(|_: &LoadRequest| -> Prepared { panic!("load fault") })
    }));
    assert!(panicked.is_err(), "the load did not panic");
    assert_eq!(names(kept.report()), ["bob-dm"], "the report moved");
    assert!(
        kept.refresh(),
        "#9808: the change behind the panicked load was never reloaded"
    );
    assert_eq!(names(kept.report()), ["bob-dm", "eve-dm"]);
}

#[test]
fn stable_load_settles_and_next_refresh_is_false() {
    let (repo, home, mut loader) = loaded();
    assert!(!loader.refresh(), "new did not settle");
    repo.commit_routes(&bob_and_eve());
    assert!(loader.refresh());
    assert!(!loader.refresh(), "refresh did not settle");
    loader.reload();
    assert!(!loader.refresh(), "reload did not settle");
    // A stable host fault settles too: its load reads no project file.
    let g = Unserved::listed(&home, &repo);
    g.symlink();
    assert!(loader.refresh());
    assert!(loader.report().denied);
    assert!(!loader.refresh(), "a stable host fault did not settle");
}

#[test]
fn persistent_change_during_load_reloads_once_then_settles() {
    let (repo, _home, mut loader) = loaded();
    repo.git(&["commit", "-q", "--allow-empty", "-m", "unrelated"]);
    assert!(loader.refresh_with(|req: &LoadRequest| {
        repo.commit_routes(&bob_and_eve());
        prepare(req)
    }));
    assert_eq!(names(loader.report()), ["bob-dm", "eve-dm"]);
    assert!(
        loader.refresh(),
        "#9808: a change during the load did not reload"
    );
    assert_eq!(names(loader.report()), ["bob-dm", "eve-dm"]);
    assert!(!loader.refresh(), "the kept change never settled");
}

/// 🔴 #8454 VC: a load that did not settle reports fingerprint 0, the value
/// the egress verdict route never serves, on both apply paths; a settled
/// load reports the digest of the fingerprint it kept.
#[test]
fn unsettled_load_reports_fingerprint_zero() {
    let (repo, home, mut loader) = loaded();
    let settled = loader.report().fingerprint;
    assert_ne!(settled, 0, "a settled load must carry a fingerprint");
    assert_eq!(loader.kept_digest(), Some(settled), "not the kept digest");
    // The project path: eve's route appears and is reverted inside the read.
    repo.git(&["commit", "-q", "--allow-empty", "-m", "unrelated"]);
    assert!(loader.refresh_with(eve_then_back(&repo)));
    assert_eq!(loader.kept_digest(), None, "the flipped load settled");
    assert_eq!(
        loader.report().fingerprint,
        0,
        "#8454 VC: an unsettled load was stamped"
    );
    assert!(loader.refresh());
    let resettled = loader.report().fingerprint;
    assert_ne!(resettled, 0);
    assert_eq!(loader.kept_digest(), Some(resettled));
    // The host-fault path: the load reads a faulty host file, then it is fixed.
    let good = host_yaml(&[repo.dir()]);
    loader.reload_with(flip_during_load(
        || home.write_host("channels:\n  version: 1\n  surprise: true\n"),
        || home.write_host(&good),
    ));
    assert!(loader.report().denied, "the load did not read the fault");
    assert_eq!(loader.kept_digest(), None, "the flipped host load settled");
    assert_eq!(
        loader.report().fingerprint,
        0,
        "#8454 VC: an unsettled denied load was stamped"
    );
}
