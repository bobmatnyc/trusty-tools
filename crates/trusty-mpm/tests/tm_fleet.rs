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
        // #8981: each start appends its argv, one line per launch. While the
        // RESUME_FAILS marker exists, a `--resume` start dies after 1 s, as
        // a `claude` that cannot load its conversation does.
        let sleeper = sleeper.display().to_string();
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\necho \"$*\" >> {:?}\ncase \" $* \" in *\" --resume \"*) \
                 [ -e {:?} ] && exec {sleeper:?} 1;; esac\nexec {sleeper:?} 600\n",
                bin.path().join(ARGV_LOG).display().to_string(),
                bin.path().join(RESUME_FAILS).display().to_string(),
            ),
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
        self.tm_with_socket(args, None)
    }

    /// `tm <args>`, dialling the daemon at `socket` when one is given.
    fn tm_with_socket(&self, args: &[&str], socket: Option<&Path>) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = common::tm_command_in(self.home.path());
        if let Some(socket) = socket {
            cmd.env("TRUSTY_MPM_SOCKET", socket);
        }
        cmd.args(args)
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

    /// The fake `claude`'s argv, one entry per Architect launch (a `--model`
    /// line; tm's `claude --version` probes are skipped), waiting up to 10 s
    /// for launch `n` (1-based) to appear.
    fn launches(&self, n: usize) -> Vec<Vec<String>> {
        let log = self.bin.path().join(ARGV_LOG);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let lines: Vec<Vec<String>> = std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .map(|line| line.split_whitespace().map(str::to_owned).collect())
                .filter(|argv: &Vec<String>| argv.iter().any(|a| a == "--model"))
                .collect();
            if lines.len() >= n || std::time::Instant::now() >= deadline {
                return lines;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// Claude Code's transcript for the conversation `argv` started with
    /// `--session-id`, under the Architect's config dir, in the folder named
    /// for the directory it ran in. Returns the id.
    fn write_transcript(&self, argv: &[String]) -> Option<String> {
        let id = flag_value(argv, "--session-id")?;
        let canonical = std::fs::canonicalize(self.dir()).expect("architect dir");
        let folder: String = canonical
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let projects =
            trusty_mpm::core::trusty_tools_config::managed_claude_config_dir_at(self.home.path())
                .join("projects")
                .join(folder);
        std::fs::create_dir_all(&projects).expect("project folder");
        std::fs::write(projects.join(format!("{id}.jsonl")), "{}\n").expect("transcript");
        Some(id.to_owned())
    }

    /// The Architect's conversation record under the scratch home.
    fn conversation_record(&self) -> PathBuf {
        self.home
            .path()
            .join(".trusty-mpm/architect-launch/last.architect-conversation")
    }

    /// Whether tmux session `name` runs on this environment's private server.
    fn has_session(&self, name: &str) -> bool {
        Command::new("tmux")
            .args([
                "has-session",
                "-t",
                &trusty_common::tmux::exact_session_target(name),
            ])
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            .env_remove("TMUX")
            .env_remove("TMUX_SOCKET")
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Put a `tmux` first on `tm`'s `PATH` that runs the real one, except
    /// that it fails a `#{pane_pid}` read while [`PANE_UNREADABLE`] exists and
    /// a `kill-session` while [`KILL_FAILS`] exists (#8981 round 2).
    fn wrap_tmux(&self) {
        let real = trusty_common::bin_resolve::resolve_binary("tmux").expect("tmux on PATH");
        let marker = |name: &str| self.bin.path().join(name).display().to_string();
        let script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    \
             '#{{pane_pid}}') [ -e {pane:?} ] && {{ echo 'pane unreadable' >&2; exit 1; }};;\n    \
             kill-session) [ -e {kill:?} ] && {{ echo 'kill refused' >&2; exit 1; }};;\n  \
             esac\ndone\nexec {real:?} \"$@\"\n",
            pane = marker(PANE_UNREADABLE),
            kill = marker(KILL_FAILS),
            real = real.display().to_string(),
        );
        let tmux = self.bin.path().join("tmux");
        std::fs::write(&tmux, script).expect("tmux wrapper");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Create the marker `name` in the fake `bin` dir.
    fn mark(&self, name: &str) {
        std::fs::write(self.bin.path().join(name), "").expect("marker");
    }

    /// `tmux kill-session` on this environment's private server.
    fn kill_session(&self, name: &str) {
        let status = Command::new("tmux")
            .args([
                "kill-session",
                "-t",
                &trusty_common::tmux::exact_session_target(name),
            ])
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            .env_remove("TMUX")
            .env_remove("TMUX_SOCKET")
            .status()
            .expect("tmux");
        assert!(status.success(), "kill-session {name}");
    }
}

/// The file, in the fake `bin` dir, where the fake `claude` logs its argv.
const ARGV_LOG: &str = "claude-argv.log";

/// The marker, in the fake `bin` dir, that makes a `--resume` start fail.
const RESUME_FAILS: &str = "resume-fails";

/// The marker that makes [`FleetEnv::wrap_tmux`]'s tmux fail a pane pid read.
const PANE_UNREADABLE: &str = "pane-unreadable";

/// The marker that makes [`FleetEnv::wrap_tmux`]'s tmux fail a kill-session.
const KILL_FAILS: &str = "kill-fails";

/// The value after `flag` in one launch's argv.
fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].as_str())
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
    // #8942: the scratch HOME has no daemon socket, so the bound Architect is
    // not registered — a warning, and init still exits 0.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("NOT registered with the daemon"),
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

/// #8981 regression: a relaunch resumes the prior Architect conversation
/// with no daemon session record — the scratch HOME runs no daemon, which is
/// the state after record a8a726e2 was deleted — and every launch carries
/// `--remote-control`. Then a corrupt conversation record starts fresh and
/// the summary says why.
#[test]
fn fleet_init_resumes_the_prior_conversation_after_the_record_is_gone() {
    let env = FleetEnv::new();
    let dir = env.dir();
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let first = env.launches(1).first().cloned().unwrap_or_default();
    env.write_transcript(&first);

    env.kill_session("tm-architect");
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let launches = env.launches(2);
    let second = launches.get(1).cloned().unwrap_or_default();
    let resumed = flag_value(&second, "--resume");
    assert!(
        resumed.is_some(),
        "the relaunch started a fresh conversation: {second:?}\n{}",
        text(&out)
    );
    assert_eq!(
        resumed,
        flag_value(&first, "--session-id"),
        "the relaunch resumed another conversation than the first launch started"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("resuming conversation"),
        "{}",
        text(&out)
    );
    for argv in [&first, &second] {
        assert_eq!(
            argv.last().map(String::as_str),
            Some("--remote-control"),
            "{argv:?}"
        );
    }

    // Error arm: a corrupt record is set aside, said on stdout and stderr.
    std::fs::write(env.conversation_record(), "{not json").expect("corrupt record");
    env.kill_session("tm-architect");
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let third = env.launches(3).get(2).cloned().unwrap_or_default();
    assert_eq!(flag_value(&third, "--resume"), None, "{third:?}");
    let fresh = flag_value(&third, "--session-id").expect("a new conversation id");
    assert_ne!(Some(fresh), resumed, "{third:?}");
    for stream in [&out.stdout, &out.stderr] {
        let stream = String::from_utf8_lossy(stream);
        assert!(
            stream.contains("the prior one was not resumed"),
            "{}",
            text(&out)
        );
        assert!(stream.contains("is corrupt"), "{}", text(&out));
    }
}

/// #8981 critic HIGH regression: a resume whose `claude` dies at once fails
/// the run, kills the session it made and clears the conversation record, so
/// the next `tm fleet init` starts a new conversation instead of retrying
/// the dead one on every run.
#[test]
fn fleet_init_clears_the_conversation_record_when_the_resume_fails() {
    let env = FleetEnv::new();
    let dir = env.dir();
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let first = env.launches(1).first().cloned().unwrap_or_default();
    let id = env.write_transcript(&first).expect("a fresh --session-id");
    assert!(
        env.conversation_record().is_file(),
        "the fresh id is recorded"
    );

    env.kill_session("tm-architect");
    std::fs::write(env.bin.path().join(RESUME_FAILS), "").expect("marker");
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    let second = env.launches(2).get(1).cloned().unwrap_or_default();
    assert_eq!(
        flag_value(&second, "--resume"),
        Some(id.as_str()),
        "{second:?}"
    );
    assert!(
        !out.status.success(),
        "a failed resume is a failed run: {}",
        text(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!(
            "resuming the Architect's conversation {id} failed"
        )),
        "{}",
        text(&out)
    );
    let record = env.conversation_record();
    assert!(
        stderr.contains(&format!("cleared {}", record.display())),
        "the failure names the cleared record: {}",
        text(&out)
    );
    assert!(!record.exists(), "the dead conversation is still recorded");
    assert!(
        !env.has_session("tm-architect"),
        "the failed session was left"
    );

    // The next run starts fresh, even with the resume still failing.
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let third = env.launches(3).get(2).cloned().unwrap_or_default();
    assert_eq!(flag_value(&third, "--resume"), None, "{third:?}");
    let fresh = flag_value(&third, "--session-id").expect("a new conversation id");
    assert_ne!(fresh, id, "{third:?}");
}

/// #8981 round 2, critic MEDIUM: when tmux cannot say which process runs in
/// the resumed Architect's pane, nothing proves the resume failed, so the
/// session and the conversation record are kept, with a warning naming both.
#[test]
fn fleet_init_keeps_the_resumed_architect_when_its_pane_cannot_be_read() {
    let env = FleetEnv::new();
    env.wrap_tmux();
    let dir = env.dir();
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let first = env.launches(1).first().cloned().unwrap_or_default();
    let id = env.write_transcript(&first).expect("a fresh --session-id");

    env.kill_session("tm-architect");
    env.mark(PANE_UNREADABLE);
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    let second = env.launches(2).get(1).cloned().unwrap_or_default();
    assert_eq!(
        flag_value(&second, "--resume"),
        Some(id.as_str()),
        "{second:?}"
    );
    assert!(
        out.status.success(),
        "an unreadable pane is not a failed resume: {}",
        text(&out)
    );
    let record = env.conversation_record();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not tell whether the resumed `claude`")
            && stderr.contains("tmux session tm-architect")
            && stderr.contains(&record.display().to_string()),
        "the warning names the session and the record: {}",
        text(&out)
    );
    assert!(record.is_file(), "the conversation record was cleared");
    assert!(env.has_session("tm-architect"), "the session was killed");
}

/// #8981 round 2, critic LOW: a failed resume whose session tmux will not
/// kill says so, with the command to run, instead of "killed".
#[test]
fn fleet_init_says_so_when_the_failed_resume_session_cannot_be_killed() {
    let env = FleetEnv::new();
    env.wrap_tmux();
    let dir = env.dir();
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let first = env.launches(1).first().cloned().unwrap_or_default();
    env.write_transcript(&first).expect("a fresh --session-id");

    env.kill_session("tm-architect");
    env.mark(RESUME_FAILS);
    env.mark(KILL_FAILS);
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(
        !out.status.success(),
        "a failed resume is a failed run: {}",
        text(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "could NOT kill tmux session tm-architect; run `tmux kill-session -t =tm-architect`"
        ),
        "{}",
        text(&out)
    );
    assert!(!stderr.contains("killed tmux session"), "{}", text(&out));
    assert!(
        !env.conversation_record().exists(),
        "the dead conversation is still recorded"
    );
}

/// A stand-in daemon on a unix socket that records each Architect
/// registration and answers it as the daemon does for a bound Architect.
struct FakeDaemon {
    socket: PathBuf,
    requests: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The fake daemon's one method, `mpm.managed.register_supervisor`.
struct Registrar(std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>);

#[async_trait::async_trait]
impl trusty_common::uds::server::RpcFallback for Registrar {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, trusty_common::uds::server::RpcError> {
        use trusty_mpm::session_manager::{
            RegisteredSession, RegistrationReport, SessionKind, SupervisorRegistration,
        };
        const METHOD: &str = "mpm.managed.register_supervisor";
        if method != METHOD {
            return Err(trusty_common::uds::server::RpcError::method_not_found(
                method,
                &[METHOD],
            ));
        }
        self.0.lock().expect("requests").push(params.clone());
        let reg: SupervisorRegistration = serde_json::from_value(params)
            .map_err(|e| trusty_common::uds::server::RpcError::invalid_params(e.to_string()))?;
        let row = |name: &str, kind| RegisteredSession {
            id: format!("id-{name}"),
            tmux_name: name.to_owned(),
            kind,
        };
        let mut registered = vec![row(&reg.session, SessionKind::Supervisor)];
        for helper in [&reg.poll_session, &reg.collector_session]
            .into_iter()
            .flatten()
        {
            registered.push(row(helper, SessionKind::SupervisorAux));
        }
        let report = RegistrationReport {
            registered,
            ..Default::default()
        };
        serde_json::to_value(report)
            .map_err(|e| trusty_common::uds::server::RpcError::internal(e.to_string()))
    }
}

impl FakeDaemon {
    /// Serve at `socket` on a runtime of its own, until dropped.
    fn start(socket: PathBuf) -> Self {
        let requests = std::sync::Arc::default();
        let registrar = Registrar(std::sync::Arc::clone(&requests));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let bind_at = socket.clone();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async move {
                let listener = trusty_common::uds::bind_hardened(&bind_at).expect("bind");
                ready_tx.send(()).expect("ready");
                let router = std::sync::Arc::new(
                    trusty_common::uds::server::RpcRouter::new().fallback(registrar),
                );
                trusty_common::uds::server::serve_until(
                    &listener,
                    router,
                    trusty_common::uds::server::RpcServeOptions::default(),
                    async {
                        let _ = stopped.await;
                    },
                )
                .await;
            });
        });
        ready.recv().expect("the fake daemon bound its socket");
        Self {
            socket,
            requests,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().expect("requests").clone()
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// #8981 / #8942 live FAIL: a relaunch registers the Architect and both of
/// its helpers, so `tm ls` lists the `architect` row and the
/// `architect-helper` rows again (the daemon side and the tags are pinned by
/// `a_relaunch_registers_the_architect_and_both_helpers` and
/// `session_table_pins_and_tags_the_architect_row`).
#[test]
fn fleet_init_registers_the_architect_on_a_relaunch() {
    let env = FleetEnv::new();
    let dir = env.dir();
    let daemon = FakeDaemon::start(env.tmux_dir.path().join("d.sock"));
    let out = env.tm_with_socket(
        &["fleet", "init", "--dir", dir_arg(&dir)],
        Some(&daemon.socket),
    );
    assert!(out.status.success(), "{}", text(&out));
    let first = env.launches(1).first().cloned().unwrap_or_default();
    env.write_transcript(&first);

    env.kill_session("tm-architect");
    let out = env.tm_with_socket(
        &["fleet", "init", "--dir", dir_arg(&dir)],
        Some(&daemon.socket),
    );
    assert!(out.status.success(), "{}", text(&out));
    let second = env.launches(2).get(1).cloned().unwrap_or_default();
    assert!(
        flag_value(&second, "--resume").is_some(),
        "not a relaunch: {second:?}"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(
            "registered tm-architect with the daemon as the Architect (record id-tm-architect; \
             helpers: tm-architect-poll, tm-architect-collector)"
        ),
        "{}",
        text(&out)
    );
    let requests = daemon.requests();
    assert_eq!(
        requests.len(),
        2,
        "one registration per launch: {requests:?}"
    );
    let canonical = std::fs::canonicalize(&dir).expect("architect dir");
    let relaunch = &requests[1];
    assert_eq!(relaunch["session"], "tm-architect", "{relaunch}");
    assert_eq!(relaunch["poll_session"], "tm-architect-poll", "{relaunch}");
    assert_eq!(
        relaunch["collector_session"], "tm-architect-collector",
        "{relaunch}"
    );
    let sent = relaunch["dir"].as_str().map(std::fs::canonicalize);
    assert!(
        sent.is_some_and(|d| d.ok() == Some(canonical.clone())),
        "{relaunch}"
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
            .args([
                "has-session",
                "-t",
                &trusty_common::tmux::exact_session_target(name),
            ])
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
