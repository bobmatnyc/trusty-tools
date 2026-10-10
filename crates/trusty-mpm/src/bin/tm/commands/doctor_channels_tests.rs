//! Tests for the `channels_*` doctor rows (#8454 S2c).
//!
//! Why: every error arm must read as Fail or Unknown, never Ok (Fail-Open
//! Check); a test here goes red if any arm is changed to report Ok.
//! What: real loads over a temp home and temp git repos, pure reports built
//! with `merge_for`, and injected loaders that hang, panic or record.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use trusty_channels::policy::{
    Channel, FileState, GateError, LoadReport, LoadRequest, ProjectFileError, ProjectInput,
    load_effective_until, merge_for, parse_host, parse_project_file,
};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

use super::{
    HostProbe, LOAD_TIMEOUT, Loader, Outcome, Unfinished, gate_row, host_row, load_deadline,
    rows_with, view_row,
};

const NAMES: [&str; 4] = [
    "channels_host",
    "channels_routes",
    "channels_gchat",
    "channels_gate",
];

/// A canonical temp home (macOS tempdirs sit under a symlink).
struct Home {
    _tmp: tempfile::TempDir,
    home: PathBuf,
}

impl Home {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = std::fs::canonicalize(tmp.path()).expect("canonical");
        Self { _tmp: tmp, home }
    }

    fn host_path(&self) -> PathBuf {
        trusty_common::crate_config::crate_config_path_at(&self.home, "trusty-mpm")
    }

    fn write_host(&self, yaml: &str) {
        let path = self.host_path();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, yaml).expect("write host");
    }

    fn base(&self) -> LoadRequest {
        LoadRequest {
            host_path: self.host_path(),
            home: Some(self.home.clone()),
            project: None,
            channels: Vec::new(),
        }
    }
}

fn real() -> Loader {
    Arc::new(load_effective_until)
}

async fn rows(home: &Home) -> Vec<DoctorCheck> {
    rows_with(home.base(), real(), Duration::from_secs(30)).await
}

fn row<'a>(rows: &'a [DoctorCheck], name: &str) -> &'a DoctorCheck {
    rows.iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no {name} row in {rows:?}"))
}

/// Run git in `dir` with every `GIT_*` variable cleared and a fixed identity.
fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args([
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd.args(args).output().expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repo on `main` with a root commit, under `home`.
fn repo(home: &Home, name: &str) -> PathBuf {
    let dir = home.home.join(name);
    std::fs::create_dir(&dir).expect("mkdir");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "root"]);
    dir
}

fn commit_routes(dir: &Path, text: &str) {
    std::fs::create_dir_all(dir.join(".trusty-channels")).expect("mkdir");
    std::fs::write(dir.join(".trusty-channels/routes.toml"), text).expect("write");
    git(dir, &["add", ".trusty-channels/routes.toml"]);
    git(dir, &["commit", "-q", "-m", "routes"]);
}

fn slack_route(name: &str, recipient: &str) -> String {
    format!(
        "version = 2\n\n[[slack.routes]]\nname = \"{name}\"\nrecipient = \"{recipient}\"\nkinds = [\"question\"]\n"
    )
}

fn slack_host(dirs: &[&Path]) -> String {
    let list: Vec<String> = dirs
        .iter()
        .map(|d| format!("\"{}\"", d.display()))
        .collect();
    format!(
        "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: {{ bot_ref: slack, \
         app_ref: slack-app }}\n    projects: [{}]\n",
        list.join(", ")
    )
}

/// Every file under `dir` with its length and mtime.
fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read_dir") {
            let path = entry.expect("entry").path();
            let meta = std::fs::symlink_metadata(&path).expect("meta");
            if meta.is_dir() {
                stack.push(path.clone());
            }
            out.push((path, meta.len(), meta.modified().expect("mtime")));
        }
    }
    out.sort();
    out
}

/// A daemon-view report from in-memory inputs.
fn merged(home: &Home, host: &str, inputs: Vec<ProjectInput>) -> LoadReport {
    merge_for(
        parse_host(host, Some(&home.home)),
        inputs,
        &[Channel::Slack, Channel::Telegram],
    )
}

fn input(
    dir: &Path,
    parsed: Result<trusty_channels::policy::ProjectFile, ProjectFileError>,
) -> ProjectInput {
    ProjectInput {
        project_dir: dir.to_path_buf(),
        file: dir.join(".trusty-channels/routes.toml"),
        parsed,
    }
}

#[tokio::test]
async fn not_configured_reads_as_the_deny_state() {
    for host in [None, Some("default_model: sonnet\n")] {
        let home = Home::new();
        if let Some(text) = host {
            home.write_host(text);
        }
        let rows = rows(&home).await;
        for name in NAMES {
            let r = row(&rows, name);
            assert_eq!(r.status, CheckStatus::Ok, "{host:?}: {r:?}");
            assert!(r.message.contains("not configured"), "{host:?}: {r:?}");
        }
    }
}

#[tokio::test]
async fn host_faults_fail_and_never_echo_the_value() {
    let cases = [
        (
            "slack:\n    enabled: true\n    connection: { credential_ref: slack }\n",
            "bot_ref",
            "credential_ref: slack",
        ),
        (
            "slack:\n    enabled: true\n    connection: { bot_ref: xoxb-000000000000-SYNTHETIC-NOTREAL }\n",
            "bot_ref",
            "xoxb-",
        ),
        (
            "slack:\n    enabled: true\n    connection: { bot_ref: slack, app_ref: \"secret://owner/slack-app\" }\n",
            "secret://",
            "owner/slack-app",
        ),
        (
            "telegram:\n    enabled: true\n    connection: { bot_ref: telegram, app_ref: slack-app }\n",
            "app_ref",
            "no-value-to-echo",
        ),
        ("slak:\n    enabled: true\n", "invalid", "no-value-to-echo"),
    ];
    for (body, reason, value) in cases {
        let home = Home::new();
        home.write_host(&format!("channels:\n  version: 1\n  {body}"));
        let rows = rows(&home).await;
        let host = row(&rows, "channels_host");
        assert_eq!(host.status, CheckStatus::Fail, "{body}: {host:?}");
        assert!(host.message.contains(reason), "{body}: {host:?}");
        for name in ["channels_routes", "channels_gchat"] {
            assert_eq!(
                row(&rows, name).status,
                CheckStatus::Fail,
                "{body}: {rows:?}"
            );
        }
        assert_eq!(
            row(&rows, "channels_gate").status,
            CheckStatus::Unknown,
            "{rows:?}"
        );
        for r in &rows {
            assert!(!r.message.contains(value), "{value} echoed: {r:?}");
        }
    }
}

#[tokio::test]
async fn home_unknown_is_unknown() {
    let home = Home::new();
    let mut base = home.base();
    base.home = None;
    let rows = rows_with(base, real(), Duration::from_secs(30)).await;
    for name in NAMES {
        assert_eq!(row(&rows, name).status, CheckStatus::Unknown, "{rows:?}");
    }
}

#[tokio::test]
async fn valid_host_names_refs_only() {
    let home = Home::new();
    home.write_host(&format!(
        "{}  telegram:\n    enabled: false\n    connection: {{ bot_ref: telegram }}\n",
        slack_host(&[])
    ));
    let rows = rows(&home).await;
    let host = row(&rows, "channels_host");
    assert_eq!(host.status, CheckStatus::Ok, "{host:?}");
    for want in ["bot_ref slack", "app_ref slack-app", "bot_ref telegram"] {
        assert!(host.message.contains(want), "{want}: {host:?}");
    }
    let routes = row(&rows, "channels_routes");
    assert_eq!(routes.status, CheckStatus::Ok, "{routes:?}");
    assert!(routes.message.contains("0 route(s)"), "{routes:?}");
}

#[tokio::test]
async fn committed_route_is_ok_and_leaves_repo_untouched() {
    let home = Home::new();
    let dir = repo(&home, "proj");
    commit_routes(&dir, &slack_route("bob-dm", "U0ABCDEF1"));
    home.write_host(&slack_host(&[&dir]));
    let before = snapshot(&home.home);
    let rows = rows(&home).await;
    assert_eq!(snapshot(&home.home), before, "doctor wrote under the home");
    let routes = row(&rows, "channels_routes");
    assert_eq!(routes.status, CheckStatus::Ok, "{routes:?}");
    assert!(routes.message.contains("1 route(s)"), "{routes:?}");
    let gate = row(&rows, "channels_gate");
    assert_eq!(gate.status, CheckStatus::Ok, "{gate:?}");
    assert!(gate.message.contains("1 route file(s)"), "{gate:?}");
}

#[tokio::test]
async fn refused_route_file_warns_and_gate_names_the_fix() {
    let home = Home::new();
    let dir = repo(&home, "proj");
    git(&dir, &["switch", "-q", "-c", "feature"]);
    commit_routes(&dir, &slack_route("bob-dm", "U0ABCDEF1"));
    home.write_host(&slack_host(&[&dir]));
    let rows = rows(&home).await;
    let routes = row(&rows, "channels_routes");
    assert_eq!(routes.status, CheckStatus::Warn, "{routes:?}");
    assert!(routes.message.contains("routes.toml"), "{routes:?}");
    let gate = row(&rows, "channels_gate");
    assert_eq!(gate.status, CheckStatus::Warn, "{gate:?}");
    assert!(gate.message.contains("git switch main"), "{gate:?}");
}

#[test]
fn each_gate_refusal_is_a_warn_with_its_fix() {
    let home = Home::new();
    let dir = home.home.join("proj");
    let host = slack_host(&[&dir]);
    let cases = [
        (
            GateError::NotOnDefaultBranch {
                head: "feature".into(),
                default: "main".into(),
            },
            "git switch main",
        ),
        (GateError::DetachedHead, "check out the default branch"),
        (
            GateError::ContentDiffers { at: "main".into() },
            "reviewed PR",
        ),
        (GateError::NotCommitted { at: "main".into() }, "reviewed PR"),
        (GateError::NotTopLevel, "top level"),
        (GateError::DefaultBranchUnknown, "origin/HEAD"),
        (GateError::GitTimedOut { step: "rev-parse" }, "rev-parse"),
        (
            GateError::GitUnavailable {
                reason: "NotFound".into(),
            },
            "git",
        ),
    ];
    for (error, fix) in cases {
        let report = merged(
            &home,
            &host,
            vec![input(
                &dir,
                Err(ProjectFileError::Gate {
                    error: error.clone(),
                }),
            )],
        );
        let gate = gate_row(&Ok(vec![report]), &Ok(Vec::new()));
        assert_eq!(gate.status, CheckStatus::Warn, "{error:?}: {gate:?}");
        assert!(gate.message.contains(fix), "{error:?}: {gate:?}");
    }
}

#[test]
fn cross_file_overlap_is_fail() {
    let home = Home::new();
    let (a, b) = (home.home.join("a"), home.home.join("b"));
    let file = |name| parse_project_file(&slack_route(name, "U0SHARED1"), Some(&home.home));
    let report = merged(
        &home,
        &slack_host(&[&a, &b]),
        vec![input(&a, file("a-dm")), input(&b, file("b-dm"))],
    );
    assert!(report.denied, "fixture must deny: {report:?}");
    let routes = view_row("channels_routes", "daemon view", &Ok(vec![report]));
    assert_eq!(routes.status, CheckStatus::Fail, "{routes:?}");
}

#[tokio::test]
async fn daemon_view_serves_slack_and_telegram_and_gchat_loads_each_project() {
    let home = Home::new();
    let (a, b) = (home.home.join("a"), home.home.join("b"));
    home.write_host(&format!(
        "channels:\n  version: 1\n  gchat:\n    enabled: true\n    projects: [\"{}\", \"{}\"]\n",
        a.display(),
        b.display()
    ));
    let seen: Arc<Mutex<Vec<LoadRequest>>> = Arc::default();
    let log = Arc::clone(&seen);
    let loader: Loader = Arc::new(move |req: &LoadRequest, deadline: Instant| {
        log.lock().expect("log").push(req.clone());
        load_effective_until(req, deadline)
    });
    rows_with(home.base(), loader, Duration::from_secs(30)).await;
    let seen = seen.lock().expect("log");
    assert!(
        seen.iter()
            .any(|r| r.project.is_none() && r.channels == [Channel::Slack, Channel::Telegram]),
        "no daemon view: {seen:?}"
    );
    for dir in [&a, &b] {
        assert!(
            seen.iter()
                .any(|r| r.project.as_ref() == Some(dir) && r.channels == [Channel::Gchat]),
            "no gchat load for {}: {seen:?}",
            dir.display()
        );
    }
}

#[tokio::test]
async fn a_load_that_hangs_is_unknown_and_doctor_finishes() {
    let home = Home::new();
    home.write_host(&slack_host(&[]));
    // The loader blocks until the test ends and drops the sender.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = Arc::new(Mutex::new(rx));
    let loader: Loader = Arc::new(move |req: &LoadRequest, deadline: Instant| {
        let _ = rx.lock().map(|r| r.recv());
        load_effective_until(req, deadline)
    });
    let started = Instant::now();
    let rows = rows_with(home.base(), loader, Duration::from_millis(100)).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "doctor waited on the load"
    );
    for name in NAMES {
        let r = row(&rows, name);
        assert_eq!(r.status, CheckStatus::Unknown, "{r:?}");
    }
    assert!(
        row(&rows, "channels_host")
            .message
            .contains("did not finish"),
        "{rows:?}"
    );
    drop(tx);
}

#[tokio::test]
async fn a_load_that_panics_is_unknown() {
    let home = Home::new();
    home.write_host(&slack_host(&[]));
    let loader: Loader = Arc::new(|_: &LoadRequest, _: Instant| panic!("synthetic loader panic"));
    let rows = rows_with(home.base(), loader, Duration::from_secs(30)).await;
    for name in NAMES {
        assert_eq!(row(&rows, name).status, CheckStatus::Unknown, "{rows:?}");
    }
}

#[test]
fn unfinished_outcomes_are_unknown() {
    let timed_out: Outcome<Vec<LoadReport>> = Err(Unfinished::TimedOut(Duration::from_secs(30)));
    let stopped: Outcome<Vec<LoadReport>> = Err(Unfinished::Stopped);
    for outcome in [&timed_out, &stopped] {
        let routes = view_row("channels_routes", "daemon view", outcome);
        assert_eq!(routes.status, CheckStatus::Unknown, "{routes:?}");
        let gate = gate_row(outcome, &Ok(Vec::new()));
        assert_eq!(gate.status, CheckStatus::Unknown, "{gate:?}");
        let gate = gate_row(&Ok(Vec::new()), outcome);
        assert_eq!(gate.status, CheckStatus::Unknown, "{gate:?}");
    }
    let host = host_row(&Err(Unfinished::TimedOut(Duration::from_secs(30))));
    assert_eq!(host.status, CheckStatus::Unknown, "{host:?}");
    let none = view_row("channels_gchat", "gchat view", &Ok(Vec::new()));
    assert_eq!(none.status, CheckStatus::Unknown, "{none:?}");
}

#[test]
fn file_faults_and_odd_states_are_not_ok() {
    let home = Home::new();
    let dir = home.home.join("proj");
    let host = slack_host(&[&dir]);
    let parsed = || parse_project_file(&slack_route("bob-dm", "U0ABCDEF1"), Some(&home.home));
    for state in [FileState::Stale, FileState::Withheld] {
        let mut report = merged(&home, &host, vec![input(&dir, parsed())]);
        assert!(
            matches!(report.per_file[0].state, FileState::Effective { .. }),
            "{report:?}"
        );
        report.per_file[0].state = state;
        let routes = view_row("channels_routes", "daemon view", &Ok(vec![report]));
        assert_eq!(routes.status, CheckStatus::Warn, "{state:?}: {routes:?}");
    }
    let broken = Err(ProjectFileError::Parse {
        reason: "expected a table".into(),
    });
    let report = merged(&home, &host, vec![input(&dir, broken)]);
    let routes = view_row("channels_routes", "daemon view", &Ok(vec![report]));
    assert_eq!(routes.status, CheckStatus::Warn, "{routes:?}");
    assert!(routes.message.contains("expected a table"), "{routes:?}");
}

#[test]
fn a_denied_load_without_a_host_finding_is_fail() {
    let home = Home::new();
    let mut report = merged(&home, &slack_host(&[]), Vec::new());
    assert!(!report.denied, "fixture must load: {report:?}");
    report.denied = true;
    let routes = view_row("channels_routes", "daemon view", &Ok(vec![report.clone()]));
    assert_eq!(routes.status, CheckStatus::Fail, "{routes:?}");
    let host = host_row(&Ok(HostProbe {
        path: home.host_path(),
        report,
        ceiling: None,
    }));
    assert_eq!(host.status, CheckStatus::Fail, "{host:?}");
}

#[test]
fn a_load_deadline_falls_before_its_wait_ends() {
    let start = Instant::now();
    assert_eq!(
        load_deadline(start, LOAD_TIMEOUT),
        start + Duration::from_secs(28)
    );
    assert_eq!(
        load_deadline(start, Duration::from_millis(100)),
        start + Duration::from_millis(50)
    );
}

/// Names the fixture home of `doctor_leaves_no_git_running_at_its_budget`'s
/// child run; set only on that child.
const CHILD_HOME: &str = "TM_8454_DOCTOR_CHILD_HOME";
/// The child run's doctor budget: far under the gate's 10 s step timeout.
const CHILD_LIMIT: Duration = Duration::from_secs(4);

/// The first `git` on this process's PATH.
fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    std::env::split_paths(&path)
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH")
}

/// A `git` wrapper in `bin` that, in a directory holding `.block-git`, starts
/// a grandchild sleep, records both pids in `pids`, then sleeps; elsewhere it
/// runs the real git.
fn blocking_git(bin: &Path, pids: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let script = format!(
        "#!/bin/sh\nif [ -e .block-git ]; then\n  /bin/sleep 300 &\n  \
         echo \"$$ $!\" >> '{}'\n  exec /bin/sleep 300\nfi\nexec '{}' \"$@\"\n",
        pids.display(),
        real_git().display()
    );
    let wrapper = bin.join("git");
    std::fs::write(&wrapper, script).expect("write git wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // A sibling test's fork can briefly hold the script's write fd (ETXTBSY).
    let runs = (0..50).any(|_| {
        let ok = Command::new(&wrapper)
            .arg("--version")
            .current_dir(bin)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            std::thread::sleep(Duration::from_millis(20));
        }
        ok
    });
    assert!(runs, "the git wrapper never ran");
}

/// The pids in `pids` still alive after up to `grace`; each is then killed.
fn survivors(pids: &Path, grace: Duration) -> Vec<libc::pid_t> {
    let text = std::fs::read_to_string(pids).unwrap_or_default();
    let mut alive: Vec<libc::pid_t> = text
        .split_whitespace()
        .map(|p| p.parse().expect("pid"))
        .collect();
    let until = Instant::now() + grace;
    loop {
        // SAFETY: kill(2) with signal 0 only checks the pid.
        alive.retain(|&pid| unsafe { libc::kill(pid, 0) } == 0);
        if alive.is_empty() || Instant::now() >= until {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for &pid in &alive {
        // SAFETY: as above; frees a sleep this test started.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    alive
}

/// Why: #8454: doctor's load thread outlived its wait, so a blocked git (its
/// own process group) was orphaned, still running, when tm exited.
/// What: re-runs itself as a child process, as `tm doctor` runs: a `git`
/// wrapper on the child's PATH blocks in the one project the daemon and
/// gchat views both load, and the child returns from [`rows_with`] and
/// exits. No wrapper process may be alive afterwards. PATH is set on the
/// child only; this target never writes its own environment.
#[tokio::test]
async fn doctor_leaves_no_git_running_at_its_budget() {
    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let home = PathBuf::from(home);
        let base = LoadRequest {
            host_path: trusty_common::crate_config::crate_config_path_at(&home, "trusty-mpm"),
            home: Some(home),
            project: None,
            channels: Vec::new(),
        };
        let rows = rows_with(base, real(), CHILD_LIMIT).await;
        println!("child rows: {rows:?}");
        return;
    }
    let home = Home::new();
    let dir = repo(&home, "proj");
    commit_routes(&dir, &slack_route("bob-dm", "U0ABCDEF1"));
    std::fs::write(dir.join(".block-git"), b"").expect("marker");
    home.write_host(&format!(
        "{}  gchat:\n    enabled: true\n    projects: [\"{}\"]\n",
        slack_host(&[&dir]),
        dir.display()
    ));
    let bin = home.home.join("bin");
    std::fs::create_dir(&bin).expect("mkdir bin");
    let pids = bin.join("pids");
    blocking_git(&bin, &pids);
    let rest = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&rest)))
            .expect("join PATH");
    // libtest names tests relative to the crate root, without the crate name.
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    let name = format!("{module}::doctor_leaves_no_git_running_at_its_budget");
    let started = Instant::now();
    let out = Command::new(std::env::current_exe().expect("test binary path"))
        .args([
            name.as_str(),
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_HOME, &home.home)
        .env("PATH", &path)
        .output()
        .expect("re-run this test as a child process");
    let took = started.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed") && stdout.contains("child rows"),
        "the child must have run doctor; stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::read_to_string(&pids).is_ok_and(|t| !t.trim().is_empty()),
        "the wrapper never blocked; stdout: {stdout}"
    );
    let left = survivors(&pids, Duration::from_millis(500));
    assert!(
        left.is_empty(),
        "git wrapper processes {left:?} outlived a doctor run of {took:?} with a {CHILD_LIMIT:?} budget"
    );
}

#[test]
fn host_loaded_but_unreadable_details_is_unknown() {
    let home = Home::new();
    let report = merged(&home, &slack_host(&[]), Vec::new());
    let host = host_row(&Ok(HostProbe {
        path: home.host_path(),
        report,
        ceiling: None,
    }));
    assert_eq!(host.status, CheckStatus::Unknown, "{host:?}");
}
