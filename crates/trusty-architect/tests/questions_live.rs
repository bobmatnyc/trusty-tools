//! #12029 P2f: the questions ports' live tmux rule, their unrecordable
//! launch, and their fail-closed branches.
//!
//! Every tmux call here goes to `tests/fixtures/questions/fake_server.sh`,
//! copied into a temp dir and run on a private `-S` socket in that dir; no
//! test reaches the fleet's tmux server or starts, prompts or kills a real
//! session (Architect ruling 2026-11-28, C1 to C4).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_architect::FakeClock;
use trusty_architect::questions::{LIVE_TMUX, QuestionsError, Socket, TmuxCli, rc, start};
use trusty_architect::spec_drift::Outcome;

/// The recordings' instant, 2026-11-27T13:00:00Z.
const AT: f64 = 1_795_784_400.0;

/// A temp dir holding the fake server, its answers and its call log.
struct Server {
    dir: tempfile::TempDir,
}

impl Server {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let src =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/questions/fake_server.sh");
        let prog = dir.path().join("tmux");
        std::fs::copy(src, &prog).expect("copy the fake server");
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// `verb` answers `out` with exit `code`.
    fn answer(&self, verb: &str, out: &str, code: i32) {
        std::fs::write(self.path(&format!("{verb}.out")), out).expect("write answer");
        std::fs::write(self.path(&format!("{verb}.exit")), code.to_string()).expect("write exit");
    }

    fn socket(&self) -> PathBuf {
        self.path("private.sock")
    }

    fn tmux(&self) -> TmuxCli {
        TmuxCli::private(self.path("tmux"), Socket::Path(self.socket()))
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn env_of(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
}

fn rc_options<'a>(
    root: &Path,
    tmux: TmuxCli,
    env: &'a dyn Fn(&str) -> Option<String>,
) -> rc::Options<'a> {
    rc::Options {
        root: root.to_path_buf(),
        scripts: root.join("scripts"),
        store_root: root.join("local/owner-queue"),
        schema_dir: root.join("schemas"),
        cwd: root.to_path_buf(),
        env,
        tmux,
        clock: Arc::new(FakeClock::new(AT)),
    }
}

fn no_rc() -> Outcome {
    Outcome {
        stdout: "in-process rc\n".into(),
        stderr: String::new(),
        code: 0,
    }
}

fn start_options<'a>(
    root: &Path,
    tmux: TmuxCli,
    env: &'a dyn Fn(&str) -> Option<String>,
    rc: &'a dyn Fn() -> Outcome,
) -> start::Options<'a> {
    start::Options {
        prog: "scripts/start-architect-questions.sh".into(),
        root: root.to_path_buf(),
        env,
        tmux,
        clock: Arc::new(FakeClock::new(AT)),
        rc,
    }
}

/// C4: with a directory holding an executable `tmux` first on `PATH`, the
/// live options still resolve tmux from [`LIVE_TMUX`] and never run it.
#[test]
fn live_options_never_resolve_tmux_through_path() {
    let server = Server::new();
    let root = tempfile::tempdir().expect("temp dir");
    let path = format!("{}:/usr/bin:/bin", server.dir.path().display());
    let env = env_of(vec![("PATH", path)]);
    let clock = Arc::new(FakeClock::new(AT));
    match rc::Options::live(root.path(), &env, clock) {
        Ok(o) => {
            let program = o.tmux.program();
            assert!(
                LIVE_TMUX.iter().any(|c| Path::new(c) == program),
                "{program:?} is not one of {LIVE_TMUX:?}"
            );
            assert!(!program.starts_with(server.dir.path()), "{program:?}");
            assert!(o.tmux.socket().is_none(), "live runs on tmux's own server");
        }
        Err(QuestionsError::NoTmux(_)) => {
            assert!(LIVE_TMUX.iter().all(|c| !Path::new(c).is_file()));
        }
        Err(e) => panic!("{e}"),
    }
    assert!(server.calls().is_empty(), "the PATH tmux ran");
}

/// The launch the recordings cannot hold (its prompt names the default
/// session): every call on the private socket, the frozen script's argv, and
/// one `relaunch.log` line once the pane shows its URL.
#[test]
fn start_launch_runs_only_on_the_private_socket_and_logs_the_url() {
    let server = Server::new();
    let root = tempfile::tempdir().expect("temp dir");
    let r = root.path();
    std::fs::create_dir(r.join("cfg")).expect("cfg");
    server.answer("has-session", "", 1);
    server.answer(
        "capture-pane",
        "x\nRemote Control https://claude.ai/code/session_AB12\n",
        0,
    );
    let env = env_of(vec![
        ("HOME", r.display().to_string()),
        ("CLAUDE_CONFIG_DIR", r.join("cfg").display().to_string()),
        ("ARCH_Q_RC_SLEEP", "0".into()),
    ]);
    let o = start::run(
        &["D01".into()],
        &start_options(r, server.tmux(), &env, &no_rc),
    );
    let url = "https://claude.ai/code/session_AB12";
    assert_eq!(
        (o.stdout.as_str(), o.stderr.as_str(), o.code),
        (
            &*format!("queued: D01\nstarted: tm-architect-questions\nrc: {url}\n"),
            "",
            0
        )
    );
    let sock = server.socket().display().to_string();
    let q = format!("{}/inbox/interactive-queue.txt", r.display());
    let prompt = format!(
        "You are tm-architect-questions, not the Architect. Read \
         templates/architect-questions-brief.md now and follow it exactly. The queue file is {q}."
    );
    assert_eq!(
        server.calls(),
        [
            format!("-S {sock} has-session -t =tm-architect-questions"),
            format!(
                "-S {sock} new-session -d -s tm-architect-questions -c {} -e CLAUDE_CONFIG_DIR={}/cfg \
                 claude --remote-control 'tm-architect-questions' '{prompt}'",
                r.display(),
                r.display()
            ),
            format!("-S {sock} capture-pane -t =tm-architect-questions: -p -J -S -200"),
        ]
    );
    let log = std::fs::read_to_string(r.join("inbox/relaunch.log")).expect("relaunch.log");
    assert_eq!(log, format!("13:00:00Z tm-architect-questions rc: {url}\n"));
    assert_eq!(std::fs::read_to_string(&q).expect("queue"), "D01\n");
}

/// With `ARCH_Q_RC` unset the running-session branch runs `questions_rc`
/// in process; set, it runs the named script instead.
#[test]
fn start_runs_questions_rc_in_process_unless_arch_q_rc_names_a_script() {
    let server = Server::new();
    let root = tempfile::tempdir().expect("temp dir");
    let r = root.path();
    server.answer("has-session", "", 0);
    let missing = r.join("missing.py").display().to_string();
    let base = vec![("ARCH_Q_INPUT_STATE", missing.clone())];
    let env = env_of(base.clone());
    let o = start::run(
        &["D01".into()],
        &start_options(r, server.tmux(), &env, &no_rc),
    );
    assert!(o.stdout.contains("in-process rc\n"), "{}", o.stdout);
    assert!(
        o.stdout.contains("input=unknown; no prompt sent"),
        "{}",
        o.stdout
    );
    let mut with = base;
    with.push(("ARCH_Q_RC", missing));
    let env = env_of(with);
    let o = start::run(
        &["D01".into()],
        &start_options(r, server.tmux(), &env, &no_rc),
    );
    assert!(!o.stdout.contains("in-process rc"), "{}", o.stdout);
    assert_eq!(o.code, 0);
}

/// A temp root for the `questions_rc` failure tests: an `old` state file, an
/// empty queue, an empty store and no `scripts/` helpers.
fn rc_root() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("temp dir");
    let r = root.path();
    std::fs::create_dir_all(r.join("inbox")).expect("inbox");
    std::fs::create_dir_all(r.join("scripts")).expect("scripts");
    std::fs::create_dir_all(r.join("local/owner-queue/items")).expect("store");
    std::fs::write(r.join("inbox/questions-rc.json"), "old").expect("old state");
    std::fs::write(r.join("inbox/interactive-queue.txt"), "").expect("queue");
    let state = r.join("inbox/questions-rc.json");
    (root, state)
}

fn state_is_old(state: &Path) -> bool {
    std::fs::read_to_string(state).expect("state") == "old"
}

fn rc_env() -> impl Fn(&str) -> Option<String> {
    env_of(vec![("ARCH_Q_SESSION", "qna-alpha".into())])
}

/// Requirement 4: a tmux that cannot start is exit 1 and leaves the state
/// file as it was.
#[test]
fn rc_exits_1_and_keeps_the_state_file_when_tmux_cannot_start() {
    let (root, state) = rc_root();
    let r = root.path();
    let env = rc_env();
    let gone = TmuxCli::private(r.join("no-such-tmux"), Socket::Name("p".into()));
    let o = rc::run(&[], &rc_options(r, gone, &env));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr.starts_with("questions_rc: tmux has-session:"),
        "{}",
        o.stderr
    );
    assert!(state_is_old(&state));
}

/// Why: divergence D1 — the frozen script reads a non-zero `capture-pane` as
/// an empty pane and carries on; the port stops instead.
/// What: `questions_rc` with a running session whose `capture-pane` exits 1
/// prints nothing, exits 1 and leaves the state file as it was.
/// Test: this test.
#[test]
fn rc_exits_1_and_keeps_the_state_file_on_a_failed_capture_pane() {
    let (root, state) = rc_root();
    let r = root.path();
    let env = rc_env();
    let server = Server::new();
    server.answer("has-session", "", 0);
    server.answer("capture-pane", "", 1);
    let o = rc::run(&[], &rc_options(r, server.tmux(), &env));
    assert_eq!((o.code, o.stdout.as_str()), (1, ""), "{}", o.stderr);
    assert!(
        o.stderr.contains("tmux capture-pane: exit 1"),
        "{}",
        o.stderr
    );
    assert!(state_is_old(&state));
    assert!(
        server.calls().iter().any(|c| c.contains("capture-pane")),
        "{:?}",
        server.calls()
    );
}

/// Requirement 4: an action script that cannot start (none in `scripts/`)
/// is exit 1 and leaves the state file as it was.
#[test]
fn rc_exits_1_and_keeps_the_state_file_when_the_action_script_cannot_start() {
    let (root, state) = rc_root();
    let r = root.path();
    let env = rc_env();
    let item = r#"{"kind": "question", "status": "interactive", "version": 1}"#;
    std::fs::write(r.join("local/owner-queue/items/D01.json"), item).expect("item");
    let server = Server::new();
    server.answer("has-session", "", 1);
    let o = rc::run(&[], &rc_options(r, server.tmux(), &env));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(
        o.stderr.contains("start-architect-questions.sh"),
        "{}",
        o.stderr
    );
    assert!(state_is_old(&state));
}

/// Requirement 4: a failed state write (`<state>.tmp` is a directory) is
/// exit 1 after the count line, and the state file stays as it was.
#[test]
fn rc_exits_1_and_keeps_the_state_file_when_the_state_write_fails() {
    let (root, state) = rc_root();
    let r = root.path();
    let env = rc_env();
    std::fs::create_dir(r.join("inbox/questions-rc.json.tmp")).expect("tmp dir");
    let server = Server::new();
    server.answer("has-session", "", 1);
    let o = rc::run(&[], &rc_options(r, server.tmux(), &env));
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(o.stderr.contains("cannot write"), "{}", o.stderr);
    assert!(
        o.stdout.starts_with("questions_rc: count=0"),
        "{}",
        o.stdout
    );
    assert!(state_is_old(&state));
}

/// Requirement 4: a queue that is not UTF-8 raises, as the script's read
/// does: exit 1, the state file as it was.
#[test]
fn rc_exits_1_and_keeps_the_state_file_on_a_queue_that_is_not_utf8() {
    let (root, state) = rc_root();
    let r = root.path();
    let env = rc_env();
    std::fs::write(r.join("inbox/interactive-queue.txt"), b"D01\n\xff\n").expect("queue");
    let server = Server::new();
    server.answer("has-session", "", 1);
    let o = rc::run(&[], &rc_options(r, server.tmux(), &env));
    assert_eq!((o.code, o.stdout.as_str()), (1, ""), "{}", o.stderr);
    assert!(o.stderr.starts_with("questions_rc: "), "{}", o.stderr);
    assert!(state_is_old(&state));
}

/// A temp root for the launcher failure tests: a Claude config dir, and a
/// plain file `blocker` that no directory can be made under.
fn start_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(root.path().join("cfg")).expect("cfg");
    std::fs::write(root.path().join("blocker"), "a file").expect("blocker");
    root
}

/// Runs the launcher for `D01` with zero pauses, two tries and `extra` vars.
fn run_start(r: &Path, server: &Server, extra: Vec<(&'static str, String)>) -> Outcome {
    let mut pairs = vec![
        ("ARCH_Q_SESSION", "qna-alpha".into()),
        ("CLAUDE_CONFIG_DIR", r.join("cfg").display().to_string()),
        ("ARCH_Q_RC_SLEEP", "0".into()),
        ("ARCH_Q_RC_TRIES", "2".into()),
    ];
    pairs.extend(extra);
    let env = env_of(pairs);
    start::run(
        &["D01".into()],
        &start_options(r, server.tmux(), &env, &no_rc),
    )
}

/// Why: divergence D2 — the frozen script carries on after a failed
/// `mkdir -p` of the queue's directory; the port stops instead.
/// What: `ARCH_Q_QUEUE` under a plain file is a write error: exit 1, nothing
/// on stdout, no tmux call.
/// Test: this test.
#[test]
fn start_exits_1_with_no_tmux_call_when_the_queue_dir_cannot_be_made() {
    let root = start_root();
    let r = root.path();
    let server = Server::new();
    let blocked = r.join("blocker/q.txt").display().to_string();
    let o = run_start(r, &server, vec![("ARCH_Q_QUEUE", blocked)]);
    assert_eq!((o.code, o.stdout.as_str()), (1, ""), "{}", o.stderr);
    assert!(o.stderr.contains("cannot write"), "{}", o.stderr);
    assert!(server.calls().is_empty(), "{:?}", server.calls());
}

/// Why: divergence D2 — the frozen script carries on after a failed
/// `echo "$id" >> "$Q"`; the port stops instead.
/// What: `ARCH_Q_QUEUE` naming a directory makes the append fail: exit 1,
/// nothing on stdout, no tmux call.
/// Test: this test.
#[test]
fn start_exits_1_with_no_tmux_call_when_the_queue_append_fails() {
    let root = start_root();
    let r = root.path();
    let server = Server::new();
    let queue = r.join("queue-is-a-dir");
    std::fs::create_dir(&queue).expect("queue dir");
    let q = queue.display().to_string();
    let o = run_start(r, &server, vec![("ARCH_Q_QUEUE", q.clone())]);
    assert_eq!((o.code, o.stdout.as_str()), (1, ""), "{}", o.stderr);
    assert!(
        o.stderr.contains(&format!("cannot write {q}")),
        "{}",
        o.stderr
    );
    assert!(server.calls().is_empty(), "{:?}", server.calls());
}

/// Why: divergence D3 — the frozen script prints `started:` and waits for a
/// URL even when `tmux new-session` fails; the port stops instead.
/// What: a failed `new-session` is exit 1 after `queued:` only, with no
/// `started:` line, no `capture-pane` and no `relaunch.log`.
/// Test: this test.
#[test]
fn start_exits_1_before_started_with_no_relaunch_log_on_a_failed_new_session() {
    let root = start_root();
    let r = root.path();
    let server = Server::new();
    server.answer("has-session", "", 1);
    server.answer("new-session", "", 1);
    let o = run_start(r, &server, vec![]);
    assert_eq!(
        (o.code, o.stdout.as_str()),
        (1, "queued: D01\n"),
        "{}",
        o.stderr
    );
    assert!(o.stderr.contains("tmux new-session"), "{}", o.stderr);
    assert!(!r.join("inbox/relaunch.log").exists());
    assert!(
        !server.calls().iter().any(|c| c.contains("capture-pane")),
        "{:?}",
        server.calls()
    );
}

/// Why: divergence D1 on the launcher — the frozen script reads a non-zero
/// `capture-pane` as an empty pane and keeps polling; the port stops.
/// What: after `started:`, a failed `capture-pane` is exit 1 on the first
/// try, with no `rc:` line and no `relaunch.log`.
/// Test: this test.
#[test]
fn start_exits_1_with_no_rc_line_on_a_failed_capture_pane() {
    let root = start_root();
    let r = root.path();
    let server = Server::new();
    server.answer("has-session", "", 1);
    server.answer("new-session", "", 0);
    server.answer("capture-pane", "", 1);
    let o = run_start(r, &server, vec![]);
    assert_eq!(
        (o.code, o.stdout.as_str()),
        (1, "queued: D01\nstarted: qna-alpha\n"),
        "{}",
        o.stderr
    );
    assert!(
        o.stderr.contains("tmux capture-pane: exit 1"),
        "{}",
        o.stderr
    );
    assert!(!r.join("inbox/relaunch.log").exists());
    let captures = server
        .calls()
        .iter()
        .filter(|c| c.contains("capture-pane"))
        .count();
    assert_eq!(captures, 1, "{:?}", server.calls());
}

/// Why: divergence D4 — the frozen script prints `rc: <url>` even when its
/// `relaunch.log` append fails; the port stops instead.
/// What: `RC_URL_LOG` in a missing directory is exit 1 after `started:`,
/// with no `rc:` line and the write error on stderr; every tmux call stays
/// on the private socket.
/// Test: this test.
#[test]
fn start_exits_1_with_no_rc_line_when_the_relaunch_log_append_fails() {
    let root = start_root();
    let r = root.path();
    let server = Server::new();
    server.answer("has-session", "", 1);
    server.answer("new-session", "", 0);
    server.answer("capture-pane", "https://claude.ai/code/session_Z9\n", 0);
    let log = r.join("no-dir/relaunch.log").display().to_string();
    let o = run_start(r, &server, vec![("RC_URL_LOG", log.clone())]);
    assert_eq!(
        (o.code, o.stdout.as_str()),
        (1, "queued: D01\nstarted: qna-alpha\n"),
        "{}",
        o.stderr
    );
    assert!(
        o.stderr.contains(&format!("cannot write {log}")),
        "{}",
        o.stderr
    );
    for call in server.calls() {
        assert!(call.starts_with("-S "), "{call}");
    }
}

/// Why: `Duration::from_secs_f64` panics on NaN, and `ARCH_Q_RC_SLEEP=nan`
/// parses as NaN (#12029); the frozen script's `sleep nan` fails and its loop
/// runs on with no pause.
/// What: the launch with `ARCH_Q_RC_SLEEP=nan` pauses 0 s per try, logs the
/// URL and exits 0.
/// Test: this test.
#[test]
fn start_treats_a_nan_rc_sleep_as_no_pause() {
    let root = start_root();
    let r = root.path();
    let server = Server::new();
    server.answer("has-session", "", 1);
    server.answer("new-session", "", 0);
    server.answer("capture-pane", "https://claude.ai/code/session_Z9\n", 0);
    let env = env_of(vec![
        ("ARCH_Q_SESSION", "qna-alpha".into()),
        ("CLAUDE_CONFIG_DIR", r.join("cfg").display().to_string()),
        ("ARCH_Q_RC_SLEEP", "nan".into()),
        ("ARCH_Q_RC_TRIES", "1".into()),
    ]);
    let clock = Arc::new(FakeClock::new(AT));
    let mut opts = start_options(r, server.tmux(), &env, &no_rc);
    opts.clock = clock.clone();
    let o = start::run(&["D01".into()], &opts);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stdout
            .ends_with("rc: https://claude.ai/code/session_Z9\n"),
        "{}",
        o.stdout
    );
    assert_eq!(clock.sleeps(), [std::time::Duration::ZERO]);
}

/// C1: the port's sources hold no tmux stand-in; the one the tests use lives
/// under `tests/`.
#[test]
fn questions_sources_carry_no_tmux_stub() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest.join("src/questions");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("src/questions") {
        let path = entry.expect("entry").path();
        let text = std::fs::read_to_string(&path).expect("source");
        let lower = text.to_lowercase();
        for needle in ["#!/", "stub", "fake_server", "tests/fixtures/questions"] {
            assert!(
                !lower.contains(needle),
                "{} holds {needle:?}",
                path.display()
            );
        }
        seen += 1;
    }
    assert!(seen >= 4, "only {seen} files under {}", dir.display());
    assert!(
        manifest
            .join("tests/fixtures/questions/fake_server.sh")
            .is_file()
    );
}
