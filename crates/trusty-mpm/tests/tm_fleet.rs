//! `tm fleet init|status` through the built binary (#8436).
//!
//! Why: the unit tests in `commands::fleet::tests` never start a session. Only
//! the binary proves that `init` starts `tm-architect` detached with the
//! supervisor stamp, and that `status` exits 1 until it does.
//! What: each test owns a scratch HOME and its own tmux server directory
//! (`TMUX_TMPDIR`, with `TMUX_SOCKET` removed), and puts a fake `claude`
//! first on `PATH`; the server is
//! killed when the test ends. Nothing reaches the operator's home or tmux.
//! Test: `cargo test -p trusty-mpm --test integration tm_fleet::`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::common;

/// A scratch home, a private tmux server directory and a fake `claude`.
struct FleetEnv {
    home: tempfile::TempDir,
    tmux_dir: tempfile::TempDir,
    bin: tempfile::TempDir,
}

impl FleetEnv {
    fn new() -> Self {
        let bin = tempfile::tempdir().expect("fake bin dir");
        let claude = bin.path().join("claude");
        // #8878 ruling A: `tm fleet init` finds its claude by process name, so
        // the fake execs a `sleep` whose name contains `claude`. A symlink, not
        // a copy: macOS kills an unsigned copy of a system binary.
        let sleeper = bin.path().join("claude-sleep");
        std::os::unix::fs::symlink("/bin/sleep", &sleeper).expect("fake claude process");
        std::fs::write(
            &claude,
            format!("#!/bin/sh\nexec {:?} 600\n", sleeper.display().to_string()),
        )
        .expect("fake claude");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            home: tempfile::tempdir().expect("scratch home"),
            // Short path: tmux refuses a socket path over `sun_path` (104 bytes).
            tmux_dir: tempfile::Builder::new()
                .prefix("tmf")
                .tempdir_in("/tmp")
                .expect("tmux dir"),
            bin,
        }
    }

    /// `tm <args>` confined to this environment.
    fn tm(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        common::tm_command_in(self.home.path())
            .args(args)
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            // #8436 P4 fix: the fleet scripts read TMUX_SOCKET, which would
            // take their tmux off the private server above.
            .env_remove("TMUX_SOCKET")
            // #5784: the scratch HOME trips the host-state guard; the private
            // TMUX_TMPDIR above is what keeps tmux off the operator's server.
            .env("TRUSTY_MPM_ALLOW_HOST_STATE", "1")
            .env("PATH", path)
            .output()
            .expect("spawn tm")
    }

    fn dir(&self) -> PathBuf {
        self.home.path().join("arch")
    }
}

impl Drop for FleetEnv {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("kill-server")
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            .env_remove("TMUX")
            .env_remove("TMUX_SOCKET")
            .output();
    }
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(out)))
}

fn dir_arg(dir: &Path) -> &str {
    dir.to_str().expect("utf-8 scratch path")
}

#[test]
fn fleet_status_exits_nonzero_before_init() {
    let env = FleetEnv::new();
    let out = env.tm(&["fleet", "status", "--dir", dir_arg(&env.dir())]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("MISSING"),
        "{}",
        text(&out)
    );

    let out = env.tm(&["fleet", "status", "--json", "--dir", dir_arg(&env.dir())]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert_eq!(json(&out)["complete"], false, "{}", text(&out));
}

#[test]
fn fleet_init_launches_the_architect_and_status_is_complete() {
    let env = FleetEnv::new();
    let dir = env.dir();
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("started tmux session tm-architect"),
        "{}",
        text(&out)
    );
    // #8878 ruling A: the launch recorded the claude it started, and only it.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("bound to claude pid"),
        "{}",
        text(&out)
    );
    let root = env.home.path().join(".trusty-mpm");
    let records: Vec<_> = std::fs::read_dir(root.join("architect-launch"))
        .expect("the launch record directory")
        .map(|entry| entry.expect("entry").path())
        // #8878 R1: the `<pid>.architect-session` name sidecar sits beside the record.
        .filter(|path| path.extension().is_some_and(|ext| ext == "architect"))
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    let pid: u32 = records[0]
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.parse().ok())
        .expect("a `<pid>.architect` record");
    let record = trusty_mpm::core::architect_launch::ARCHITECT_RECORDS
        .read(&root, pid)
        .expect("the record reads")
        .expect("the record exists");
    assert_eq!(record.pid, pid);
    assert!(trusty_mpm::core::process::process_name_is_claude(pid));
    assert_eq!(
        trusty_mpm::core::architect_launch::architect_session_name(&root, pid).as_deref(),
        Ok("tm-architect")
    );

    // #8436 P4: the launch also starts the poller, whose ROOT is the
    // Architect directory: it writes its log under `<dir>/inbox/`.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("started poller session tm-architect-poll"),
        "{}",
        text(&out)
    );
    let log = dir.join("inbox/poll.log");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !log.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(log.exists(), "the poller wrote no {}", log.display());

    let out = env.tm(&["fleet", "status", "--json", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let report = json(&out);
    assert_eq!(report["complete"], true, "{}", text(&out));
    assert_eq!(report["session"], "tm-architect");
    assert_eq!(report["binding"]["ok"], true, "{}", text(&out));

    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Nothing changed"),
        "{}",
        text(&out)
    );
}

/// #8878 R1: `--session` names the Architect's session, its poller
/// (`<name>-poll`) and the launch record, and `status` without the flag
/// finds all three; no `tm-architect` session is started.
#[test]
fn fleet_init_with_a_session_override_names_every_session() {
    let env = FleetEnv::new();
    let dir = env.dir();
    let out = env.tm(&[
        "fleet",
        "init",
        "--session",
        "tm-sup-x",
        "--dir",
        dir_arg(&dir),
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", text(&out));
    for want in [
        "started tmux session tm-sup-x on",
        "bound to claude pid",
        "started poller session tm-sup-x-poll",
        "`[supervisor] session = \"tm-sup-x\"`",
    ] {
        assert!(stdout.contains(want), "{want}: {}", text(&out));
    }
    let has = |name: &str| {
        Command::new("tmux")
            .args(["has-session", "-t", &format!("={name}")])
            .env("TMUX_TMPDIR", env.tmux_dir.path())
            .env_remove("TMUX")
            .env_remove("TMUX_SOCKET")
            .status()
            .expect("tmux")
            .success()
    };
    assert!(has("tm-sup-x") && has("tm-sup-x-poll"));
    assert!(!has("tm-architect") && !has("tm-architect-poll"));

    let root = env.home.path().join(".trusty-mpm");
    let pid: u32 = std::fs::read_dir(root.join("architect-launch"))
        .expect("record dir")
        .filter_map(|e| {
            e.ok()?
                .path()
                .file_name()?
                .to_str()?
                .strip_suffix(".architect")?
                .parse()
                .ok()
        })
        .next()
        .expect("a launch record");
    assert_eq!(
        trusty_mpm::core::architect_launch::architect_session_name(&root, pid).as_deref(),
        Ok("tm-sup-x")
    );

    let out = env.tm(&["fleet", "status", "--json", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let report = json(&out);
    assert_eq!(report["session"], "tm-sup-x", "{}", text(&out));
    assert_eq!(report["binding"]["ok"], true, "{}", text(&out));

    // Item 6: a re-run against the running session in the same directory.
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Nothing changed"),
        "{}",
        text(&out)
    );
}

/// #8878 R1 item 2: an invalid `--session` refuses before any write.
#[test]
fn fleet_init_refuses_an_invalid_session_name_before_writing() {
    let env = FleetEnv::new();
    for bad in ["tm:arch", "tm arch", ""] {
        let out = env.tm(&[
            "fleet",
            "init",
            "--session",
            bad,
            "--dir",
            dir_arg(&env.dir()),
        ]);
        assert!(!out.status.success(), "{bad:?}: {}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("invalid --session"),
            "{}",
            text(&out)
        );
        assert!(!env.dir().exists(), "{bad:?}: {}", text(&out));
        assert!(!env.home.path().join(".trusty-mpm/config.toml").exists());
    }
}

/// #8436 P4, fail closed: a poller that does not start is a FAILED step and
/// exit 1, never an ok report. Two arms: the start script fails, and it exits
/// 0 without starting a session. An edited script is kept, so each arm plants
/// its own.
#[test]
fn fleet_init_fails_closed_when_the_poller_does_not_start() {
    for (script, cause) in [
        (
            "echo 'no poller for you' >&2\nexit 7\n",
            "no poller for you",
        ),
        ("exit 0\n", "tm-architect-poll is not running"),
    ] {
        let env = FleetEnv::new();
        let dir = env.dir();
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/start-fleet-poll.sh"), script).unwrap();
        let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out));
        assert!(
            stdout.contains("FAILED     poller start:"),
            "{}",
            text(&out)
        );
        assert!(stdout.contains(cause), "{}", text(&out));
        assert!(!stdout.contains("Architect set up"), "{}", text(&out));
        assert_eq!(
            std::fs::read_to_string(dir.join("scripts/start-fleet-poll.sh")).unwrap(),
            script,
            "the edited script was overwritten"
        );
    }
}

/// #8436 P4 fix: a poller whose pane is dead (a crashed `python3` under
/// `remain-on-exit on`) is a FAILED step, both right after the start and on a
/// later run that finds the session in this directory.
#[test]
fn fleet_init_fails_when_the_poller_pane_is_dead() {
    let env = FleetEnv::new();
    let dir = env.dir();
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    let script = "tmux new-session -d -s \"$ARCHITECT_POLL_SESSION\" -c \"$PWD\" \
                  'sleep 0.2; exit 3' \\; set-option -w remain-on-exit on\n";
    std::fs::write(dir.join("scripts/start-fleet-poll.sh"), script).unwrap();
    for run in ["first", "second"] {
        let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(1), "{run} run: {}", text(&out));
        assert!(stdout.contains("FAILED"), "{run} run: {}", text(&out));
        assert!(stdout.contains("pane is dead"), "{run} run: {}", text(&out));
        assert!(!stdout.contains("Architect set up"), "{}", text(&out));
    }
}

/// #8436 P4 fix, item 8: an Architect that cannot be bound to its `claude`
/// (#8878 ruling A) is a warning and exit 0, because an npm/node `claude`
/// never binds. The summary names it, instead of only "Architect set up".
/// A file where the launch record directory goes makes the record fail.
#[test]
fn fleet_init_names_an_unbound_architect_in_the_summary() {
    let env = FleetEnv::new();
    let dir = env.dir();
    let root = env.home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("architect-launch"), "not a directory\n").unwrap();
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", text(&out));
    assert!(stdout.contains("NOT bound to its claude"), "{}", text(&out));
    assert!(
        stdout.contains(
            "Architect set up (NOT bound: anchor writes will be denied; see the warning above)"
        ),
        "{}",
        text(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("the Architect process was NOT recorded"),
        "{}",
        text(&out)
    );

    // #8878 R1 critic MEDIUM: a re-run finds the session running unbound
    // and says so, never "Nothing changed".
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        stdout.contains("tmux session tm-architect is running but is not a bound Architect"),
        "{}",
        text(&out)
    );
    assert!(
        stdout.contains("Architect set up (NOT bound"),
        "{}",
        text(&out)
    );
    assert!(!stdout.contains("Nothing changed"), "{}", text(&out));
}

/// #8436: `--dir` goes through the same preflight as the default, so the
/// binary refuses `$HOME` and writes no grant.
#[test]
fn fleet_init_refuses_the_home_directory_before_writing() {
    let env = FleetEnv::new();
    let home = env.home.path();
    let out = env.tm(&["fleet", "init", "--no-launch", "--dir", dir_arg(home)]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("it is your home directory"),
        "{}",
        text(&out)
    );
    assert!(
        !home.join(".trusty-mpm/config.toml").exists(),
        "the grant was written: {}",
        text(&out)
    );
    assert!(!home.join(".trusty-mpm.toml").exists(), "{}", text(&out));
}
