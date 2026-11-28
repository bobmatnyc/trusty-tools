//! An opted-in `tm connect` and in-place `tm session start` start `claude`
//! with the role's sandbox policy (#8928 step 6, #10102 criterion 3).
//!
//! Why: #10102 criterion 3 asks, per launch path, for a test that the policy
//! is applied and verified before the session starts. Both paths had only a
//! negative test. Since #12077 the policy is opt-in (`TM_CLAUDE_SANDBOX=1`)
//! and rides inline as `--settings '<json>'` in the launch spec the pane's
//! start line names; tm writes the spec and reads it back before it types
//! that line. The switch is a process env var, and setting it inside the
//! `tm` bin test process would change the result of parallel tests there
//! (`session_start_posts_the_same_wire_shape_bare_tm_guided_default_sends`
//! asserts the exact POST body), so these tests set it on a child `tm` only.
//! What: each case runs the built `tm` under a scratch `$HOME` against the
//! #10459 stub daemon, with a fake `claude` and a logging fake `tmux` first on
//! `PATH`; the fake tmux answers "no such session" to `has-session` and
//! accepts every other call. The test asserts the child exits 0, registered
//! once, typed one start line, and that the spec file the line names carries
//! exactly one `--settings` whose JSON equals the PM role's
//! `inline_sandbox_settings` for that workspace and home.
//! Test: `an_opted_in_connect_starts_claude_with_the_pm_sandbox_8928`,
//! `an_opted_in_session_start_starts_claude_with_the_pm_sandbox_8928`.

use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_mpm::core::claude_sandbox::{SANDBOX_OPT_IN_ENV, inline_sandbox_settings};
use trusty_mpm::core::session_launch::{SandboxRole, launch_sandbox_inputs};
use trusty_mpm::runtime::launch_spec::LaunchSpec;

use crate::architect_version_refusal_10459::StubDaemon;
use crate::common::tm_command_in;

/// Env vars that would move a path, a tmux server or the switch away from
/// what this test sets.
const SCRUBBED: [&str; 7] = [
    "TRUSTY_MPM_CONFIG_DIR",
    "TRUSTY_MPM_WORKSPACE_ROOT",
    "TRUSTY_MPM_REPOS_ROOT",
    "TRUSTY_SANDBOX",
    "CLAUDE_CONFIG_DIR",
    "TM_WORKSPACE_TMUX",
    SANDBOX_OPT_IN_ENV,
];

/// A canonical scratch root under `/tmp` with `home/`, a remote-less git
/// `project/` and `bin/` holding the fakes; `tmux.log` gets each tmux argv.
struct Scratch {
    _guard: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let guard = tempfile::Builder::new()
            .prefix("tm-test-8928-")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = guard.path().canonicalize().expect("canonical");
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(&home).expect("mkdir home");
        std::fs::create_dir_all(&project).expect("mkdir project");
        let status = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&project)
            .status()
            .expect("git init");
        assert!(status.success(), "git init");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        // See #10248
        trusty_common::exec_stub::write_executable(
            &bin.join("claude"),
            "#!/bin/sh\n[ \"$1\" = --version ] && { echo '2.1.400 (Claude Code)'; exit 0; }\nexit 9\n",
        )
        .expect("fake claude");
        let tmux = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in *has-session*) exit 1;; esac\nexit 0\n",
            root.join("tmux.log").display()
        );
        trusty_common::exec_stub::write_executable(&bin.join("tmux"), tmux).expect("fake tmux");
        Self {
            _guard: guard,
            root,
            home,
            project,
        }
    }

    /// Run `tm --url <stub> <args>` with `TM_CLAUDE_SANDBOX=1`; returns the
    /// stub, the exit success and stderr.
    fn tm_opted_in(&self, args: &[&str]) -> (StubDaemon, bool, String) {
        let sock = self.root.join("sock");
        std::fs::create_dir_all(&sock).expect("sock dir");
        let daemon = StubDaemon::start(&sock);
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = tm_command_in(&self.home);
        for key in SCRUBBED {
            cmd.env_remove(key);
        }
        let out = cmd
            .arg("--url")
            .arg(&daemon.url)
            .args(args)
            .env("TRUSTY_MPM_SOCKET", &daemon.socket)
            .env("PATH", path)
            .env("TM_DISABLE_SPAWN_DISCLAIM", "1")
            // #5784: the scratch-home tmux gate; every tmux call hits the fake.
            .env("TRUSTY_MPM_ALLOW_HOST_STATE", "1")
            .env(SANDBOX_OPT_IN_ENV, "1")
            .output()
            .expect("spawn tm");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        (daemon, out.status.success(), stderr)
    }

    fn project_arg(&self) -> String {
        self.project.to_string_lossy().into_owned()
    }
}

/// The spec file the one typed start line names, read back.
fn started_spec(s: &Scratch, stderr: &str) -> LaunchSpec {
    let log = std::fs::read_to_string(s.root.join("tmux.log"))
        .unwrap_or_else(|e| panic!("tmux was never called ({e}); stderr: {stderr}"));
    let lines: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("send-keys") && l.contains("--launch-spec"))
        .collect();
    assert_eq!(lines.len(), 1, "one start line: {log}");
    let (_, rest) = lines[0]
        .split_once("--launch-spec '")
        .unwrap_or_else(|| panic!("the start line names a spec: {}", lines[0]));
    let file = rest.split('\'').next().expect("quoted spec path");
    LaunchSpec::verify(Path::new(file)).expect("the named spec reads back")
}

/// Assert `s`'s launch registered once and started `claude` with the PM
/// role's sandbox policy for its project and home, inline.
fn assert_started_with_the_pm_sandbox(s: &Scratch, daemon: &StubDaemon, ok: bool, stderr: &str) {
    assert!(ok, "an opted-in launch must succeed; stderr: {stderr}");
    // `tm session start` registers with `mpm.sessions.register`, `tm connect`
    // with `mpm.sessions.connect` (#6288: the socket only).
    let registered = daemon
        .calls()
        .into_iter()
        .filter(|c| c == "mpm.sessions.register" || c == "mpm.sessions.connect")
        .count();
    assert_eq!(registered, 1, "{:?}; stderr: {stderr}", daemon.calls());
    let spec = started_spec(s, stderr);
    let values: Vec<&String> = spec
        .args
        .windows(2)
        .filter(|w| w[0] == "--settings")
        .map(|w| &w[1])
        .collect();
    assert_eq!(values.len(), 1, "one --settings: {:?}", spec.args);
    let inline: serde_json::Value = serde_json::from_str(values[0]).expect("inline JSON");
    // The child resolves its daemon sockets from its own env: the stub's
    // `TRUSTY_MPM_SOCKET`, and the others under the scratch `$HOME`.
    let mut inputs = launch_sandbox_inputs(&s.project, &s.home);
    inputs.mpm_socket = Some(daemon.socket.clone());
    if let Some(real_home) = std::env::var_os("HOME").map(PathBuf::from) {
        for socket in &mut inputs.daemon_sockets {
            if let Ok(rel) = socket.strip_prefix(&real_home) {
                *socket = s.home.join(rel);
            }
        }
    }
    let want = inline_sandbox_settings(SandboxRole::Pm, &inputs);
    assert_eq!(inline, want, "the PM sandbox policy, inline");
    assert_eq!(
        inline.pointer("/sandbox/enabled"),
        Some(&serde_json::json!(true)),
        "{inline}"
    );
    let switch = (SANDBOX_OPT_IN_ENV.to_owned(), "1".to_owned());
    assert!(spec.env_set.contains(&switch), "{:?}", spec.env_set);
}

/// #10102 criterion 3, `tm connect`: an opted-in connect starts `claude` with
/// the PM sandbox policy, carried by the spec its start line names.
#[test]
fn an_opted_in_connect_starts_claude_with_the_pm_sandbox_8928() {
    let s = Scratch::new();
    let (daemon, ok, stderr) = s.tm_opted_in(&["connect", &s.project_arg()]);
    assert_started_with_the_pm_sandbox(&s, &daemon, ok, &stderr);
}

/// #10102 criterion 3, in-place `tm session start`: an opted-in start starts
/// `claude` with the PM sandbox policy, carried by the spec its start line
/// names.
#[test]
fn an_opted_in_session_start_starts_claude_with_the_pm_sandbox_8928() {
    let s = Scratch::new();
    // No remote: the in-place path, not the managed route.
    let (daemon, ok, stderr) = s.tm_opted_in(&["session", "start", "--dir", &s.project_arg()]);
    assert_started_with_the_pm_sandbox(&s, &daemon, ok, &stderr);
}
