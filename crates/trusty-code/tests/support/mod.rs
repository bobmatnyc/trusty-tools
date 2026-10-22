//! Process/protocol plumbing shared by `tests/session_e2e.rs`, (#2056)
//! `tests/task_e2e.rs`, (#2060) `tests/cli_e2e.rs`, and (#2062)
//! `tests/m1_cutline_e2e.rs`.
//!
//! Why: driving the REAL `tcode` binary over its real stdio/socket surface
//! needs a bit of scaffolding (spawn, NDJSON line I/O with timeouts, parse
//! helpers for responses vs. server-initiated notifications, stream reading)
//! that has nothing to do with the session-lifecycle assertions
//! themselves. Keeping it in `tests/support/mod.rs` (not a top-level
//! `tests/*.rs` file, so cargo does not treat it as its own test binary)
//! keeps every `*_e2e.rs` file readable as a pure black-box script.
//! What: [`StdioSession`] (spawn + NDJSON request/response/notification
//! I/O over real pipes — [`StdioSession::spawn_with_mock_llm`] additionally
//! roots the daemon at a given project dir and sets `TCODE_MOCK_LLM=echo`
//! for #2056's offline task-execution coverage), [`SocketDaemon`]/
//! [`spawn_socket_daemon`]/[`spawn_socket_daemon_with_env`] (#2062, #6637:
//! the persistent daemon on its Unix socket, with the SAME mock-LLM
//! determinism STDIO has), [`find_response`]/[`find_session_event`]
//! (classify a raw NDJSON line), [`read_stream_until`] (read one
//! `session.events` stream in stages), and [`assert_envelopes_contiguous`]
//! (#2055: the shared seq/field-presence check both the STDIO and socket e2e
//! scenarios run against the SAME `SessionEventEnvelope` shape).
//!
//! `#![allow(dead_code)]`: each `tests/*.rs` file compiles `mod support;` as
//! its OWN independent crate (cargo's per-integration-test-binary model), so
//! any helper one file doesn't use (e.g. `task_e2e.rs` never touches the
//! socket helpers `session_e2e.rs` needs) is flagged dead in THAT binary —
//! a false positive from splitting one shared file across two consumers,
//! not a real unused-code issue.
#![allow(dead_code)]

/// A stand-in trusty-memory daemon on a temp Unix socket (#6286).
pub mod uds_mock;

// Only `recall_content_e2e` needs these, and every other `tests/*.rs` binary
// compiles this file too — the same false positive `#![allow(dead_code)]`
// above exists for.
#[allow(unused_imports)]
pub use uds_mock::{MockMemoryDaemon, spawn as spawn_mock_memory_daemon};

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

/// Per-read/response timeout used throughout — generous enough for a debug
/// build under CI load, tight enough that a genuine protocol bug fails the
/// test in seconds rather than hanging the suite.
const WIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// The ONLY way an integration test may name the `tcode` binary — a
/// `Command` for the freshly-built executable with ambient-daemon isolation
/// already installed (#3036, #3195).
///
/// Why: `trusty_common::search_index` refuses daemon writes when
/// `running_under_test_harness()` holds (#4255), but that reads the RUNNING
/// process's own path. `env!("CARGO_BIN_EXE_tcode")` is
/// `target/<profile>/tcode` — outside `deps/` — so a spawned child is
/// indistinguishable from a user's real invocation and warms its
/// `--project` into whatever trusty-search daemon it discovers. On a
/// developer machine that is the OPERATOR's live daemon, which registers the
/// test's `$TMPDIR/.tmpXXXXXX` fixture and writes `.gitignore` plus
/// `.trusty-search/{index.redb,hnsw.usearch,…}` back INSIDE the sandbox.
/// Those files land at an unpredictable moment — the warm-up is a detached
/// thread and the daemon walks the tree asynchronously — so they corrupt
/// whichever `run_task` before/after diff assertion happens to be open,
/// which is why a different test failed each run and why `--test-threads=1`
/// looked green. `spawn_inner`/`spawn_socket_daemon_with_env` already set this
/// on the two children they own; the 28 call sites across 24 test functions
/// that built their own `Command` did not, and that residue is #3036/#3195.
/// What: a [`TcodeCommand`] for the binary whose environment is cleared and
/// rebuilt by [`ChildIsolation`] (#9139): `TRUSTY_TEST_HARNESS=1` so every
/// `search_index` write is refused at the source, plus a private `HOME`, data
/// root and memory socket. Pinning `HOME` alone is NOT a substitute: macOS
/// resolves the data dir through `NSFileManager`, which ignores `$HOME`.
/// Test: `no_test_spawns_the_tcode_binary_unguarded` (in the lib suite)
/// fails if any test file names the binary directly instead of calling this;
/// `hermetic_palace_e2e::turn_recording_children_never_reach_the_ambient_memory_daemon`.
pub fn tcode_command() -> TcodeCommand {
    let isolation = ChildIsolation::new();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_tcode"));
    cmd.env_clear().envs(isolation.env());
    TcodeCommand {
        cmd,
        _isolation: isolation,
    }
}

/// A `tcode` [`std::process::Command`] that owns its [`ChildIsolation`].
///
/// Why: the isolation tree must outlive the child. Dereferencing to the
/// `Command` keeps every `support::tcode_command().args(..).output()` call
/// site unchanged; the temporary lives to the end of that statement. A caller
/// that `spawn()`s must keep the `TcodeCommand` alive while the child runs.
pub struct TcodeCommand {
    cmd: std::process::Command,
    _isolation: ChildIsolation,
}

impl std::ops::Deref for TcodeCommand {
    type Target = std::process::Command;
    fn deref(&self) -> &Self::Target {
        &self.cmd
    }
}

impl std::ops::DerefMut for TcodeCommand {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cmd
    }
}

/// Inherited variables a spawned `tcode` may see — nothing that can name the
/// operator's daemons, data root or palace (#9139).
const INHERITED_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "USER",
    "LOGNAME",
    "RUST_BACKTRACE",
    "RUST_LOG",
];

/// A private `HOME`, data root and memory socket for one spawned `tcode`.
///
/// Why (#9139): every spawn inherited the whole parent environment. A
/// trusty-mpm managed shell exports `TRUSTY_MEMORY_PALACE=trusty-tools`, and
/// the default memory socket is the live daemon's, so each child's turn
/// recorder wrote its fixture turns into the live palace: 2,308 drawers.
/// What: a fresh `TempDir` with `home/` and `data/`, and the env a child gets:
/// [`INHERITED_ENV_ALLOWLIST`] from the parent, then `TRUSTY_TEST_HARNESS=1`,
/// `HOME`, `TRUSTY_DATA_DIR_OVERRIDE` and `TRUSTY_MEMORY_SOCKET` pointed into
/// the tree. Nothing binds that socket, so the recorder's dial is refused.
/// Construction panics when the tree cannot be made — fail closed, never a
/// fall back to the inherited environment.
/// Test: `hermetic_palace_e2e::turn_recording_children_never_reach_the_ambient_memory_daemon`.
pub struct ChildIsolation {
    root: tempfile::TempDir,
}

impl ChildIsolation {
    /// Create the isolation tree, panicking (failing the test) on any error.
    pub fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("tcode-child-")
            .tempdir()
            .expect("#9139: cannot create the child isolation root — refusing to spawn");
        for dir in ["home", "data"] {
            std::fs::create_dir(root.path().join(dir))
                .expect("#9139: cannot create the child isolation tree — refusing to spawn");
        }
        Self { root }
    }

    /// The child's `HOME`.
    pub fn home(&self) -> std::path::PathBuf {
        self.root.path().join("home")
    }

    /// The child's `TRUSTY_DATA_DIR_OVERRIDE`.
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.root.path().join("data")
    }

    /// The child's `TRUSTY_MEMORY_SOCKET` — a path nothing serves.
    pub fn memory_socket(&self) -> std::path::PathBuf {
        self.root.path().join("no-memory-daemon.sock")
    }

    /// The complete environment for the child, applied after `env_clear()`.
    pub fn env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = INHERITED_ENV_ALLOWLIST
            .iter()
            .filter_map(|key| std::env::var_os(key).map(|value| ((*key).into(), value)))
            .collect();
        env.push((trusty_common::test_harness::FORCE_ENV.into(), "1".into()));
        env.push(("HOME".into(), self.home().into()));
        env.push((
            trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV.into(),
            self.data_dir().into(),
        ));
        env.push((
            trusty_common::memory_rpc::TRUSTY_MEMORY_SOCKET_ENV.into(),
            self.memory_socket().into(),
        ));
        env
    }
}

/// Provision a throwaway project with `.claude/agents/{pm,python-engineer}.md`.
///
/// Why: shared by `tests/task_e2e.rs` (drives the daemon directly) and
/// (#2060) `tests/cli_e2e.rs` (drives the `tcode` CLI, which spawns its own
/// nested daemon) — both need the SAME minimal two-agent fixture for
/// `TCODE_MOCK_LLM=echo`'s scripted PM -> engineer delegation to resolve.
/// `.md` (not `.toml`) since #2897 Slice D retired the TOML agent loader —
/// `agents::discover_agents` only globs `*.md`.
pub fn project_with_agents() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("project tempdir");
    let agents = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir agents");
    std::fs::write(
        agents.join("pm.md"),
        "---\nname: pm\nmodel: openai/gpt-4o-mini\n---\n\nYou are the PM. Delegate work to python-engineer.\n",
    )
    .expect("write pm.md");
    std::fs::write(
        agents.join("python-engineer.md"),
        "---\nname: python-engineer\nmodel: deepseek/deepseek-chat\n---\n\nYou are a Python engineer.\n",
    )
    .expect("write python-engineer.md");
    tmp
}

/// A fake `$HOME` containing `~/.claude/agents/{pm,python-engineer}.md`.
///
/// Why: a PROJECTLESS daemon has no project root to resolve `.claude/agents`
/// from, so `ProjectBinding::agents_dir` falls back to the USER-level
/// `~/.claude/agents`. Pointing the child's `$HOME` at a fixture lets the
/// projectless e2e resolve real agent configs hermetically — and exercises that
/// user-level fallback itself, rather than depending on whatever happens to be
/// in the developer's real home directory.
/// What: same two agent configs `project_with_agents` writes (`.md`, #2897
/// Slice D), rooted at `<tmp>/.claude/agents` and intended to be passed as
/// the child's `HOME`.
/// Test: used by `task_e2e::projectless_task_runs_end_to_end`.
pub fn home_with_user_level_agents() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("home tempdir");
    let agents = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir user-level agents");
    std::fs::write(
        agents.join("pm.md"),
        "---\nname: pm\nmodel: openai/gpt-4o-mini\n---\n\nYou are the PM. Delegate work to python-engineer.\n",
    )
    .expect("write pm.md");
    std::fs::write(
        agents.join("python-engineer.md"),
        "---\nname: python-engineer\nmodel: deepseek/deepseek-chat\n---\n\nYou are a Python engineer.\n",
    )
    .expect("write python-engineer.md");
    tmp
}

/// A running `tcode serve --stdio` subprocess with line-buffered stdin/stdout.
///
/// Why: the STDIO half of the API-driven e2e coverage — every call in
/// `session_lifecycle_over_stdio` goes through this, writing/reading actual
/// NDJSON bytes over the child's real pipes.
pub struct StdioSession {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    /// Kept alive so a session that owns its project root (see [`Self::spawn`])
    /// removes that root on drop. `None` when the caller supplied the root.
    _project: Option<tempfile::TempDir>,
    /// #9139: the child's private `HOME`, data root and memory socket.
    _isolation: ChildIsolation,
}

impl StdioSession {
    /// Spawn the real `tcode` binary in `--stdio` mode.
    ///
    /// Why: `env!("CARGO_BIN_EXE_tcode")` resolves to the freshly-built
    /// binary for this exact test run — no separate install step needed.
    /// What: a throwaway tempdir as `--project` (any existing directory works;
    /// `session.*` does not require a configuration root), stdin/stdout piped,
    /// stderr discarded (logs aren't asserted on here), `kill_on_drop` so a
    /// panicking test still reaps the child instead of leaking a process. The
    /// tempdir is owned by the returned session and removed with it.
    ///
    /// #2074: this used to pass `--project .`, which is the CRATE directory
    /// when a test binary runs. `tcode serve` now materializes the agent roster
    /// into its project's `.trusty-code/agents/`, so `.` wrote 33 agent files
    /// and a manifest into the repository on every run.
    pub fn spawn() -> Self {
        let project = tempfile::tempdir().expect("throwaway project root");
        let mut session = Self::spawn_inner(Some(project.path()), None, None, &[], &[]);
        session._project = Some(project);
        session
    }

    /// Spawn `tcode serve --stdio` rooted at `project`, with
    /// `TCODE_MOCK_LLM=echo` set in the child's environment so `task.run`
    /// drives #2056's deterministic offline `EchoLlmClient` instead of
    /// requiring a live `OPENROUTER_API_KEY` — the mandatory offline
    /// black-box path for `tests/task_e2e.rs`.
    pub fn spawn_with_mock_llm(project: &std::path::Path) -> Self {
        Self::spawn_inner(
            Some(project),
            Some(trusty_code::task::mock_llm::MOCK_LLM_ECHO),
            None,
            &[],
            &[],
        )
    }

    /// Spawn `tcode serve --stdio` rooted at `project`, with `TCODE_MOCK_LLM`
    /// set to `mock_llm_value` (DOC-39 AC-13).
    ///
    /// Why: [`Self::spawn_with_mock_llm`] hardcodes
    /// `trusty_code::task::mock_llm::MOCK_LLM_ECHO`'s fixed single-delegation
    /// script, which cannot exercise a fan-out to two SAME-named delegated
    /// agents — the acceptance proof `tests/agent_id_e2e.rs` needs. This is
    /// the general form that lets a caller select any registered mock-LLM
    /// variant (e.g. `trusty_code::task::mock_llm::MOCK_LLM_ECHO_FANOUT`)
    /// while keeping `spawn_with_mock_llm` itself unchanged for every other
    /// e2e file.
    /// Test: `agent_id_e2e::fanned_out_same_named_delegations_get_distinct_agent_ids_over_the_wire`.
    pub fn spawn_with_mock_llm_variant(project: &std::path::Path, mock_llm_value: &str) -> Self {
        Self::spawn_inner(Some(project), Some(mock_llm_value), None, &[], &[])
    }

    /// Like [`Self::spawn_with_mock_llm_variant`], additionally setting
    /// `envs` in the child's environment (DOC-39 Slice C).
    ///
    /// Why: `tests/recall_content_e2e.rs` needs to point the daemon's
    /// `recall_session` tool at a MOCK trusty-memory backend (via
    /// `TRUSTY_MEMORY_SOCKET`,
    /// `trusty_common::memory_rpc::TRUSTY_MEMORY_SOCKET_ENV`)
    /// rather than a real daemon — no prior e2e scenario in this crate needed
    /// to override an env var beyond the mock-LLM selector, so this is the
    /// general form that adds that capability without touching any other
    /// `spawn_*` caller.
    /// What: identical to `spawn_with_mock_llm_variant`, plus every
    /// `(key, value)` pair in `envs` set on the child.
    /// Test: `recall_content_e2e::held_back_recall_result_carries_its_text_and_run_id_over_the_wire`.
    pub fn spawn_with_mock_llm_variant_and_env(
        project: &std::path::Path,
        mock_llm_value: &str,
        envs: &[(&str, &str)],
    ) -> Self {
        Self::spawn_inner(Some(project), Some(mock_llm_value), None, envs, &[])
    }

    /// Like [`Self::spawn_with_mock_llm_variant`] but also sets `extra_envs`
    /// in the child's environment (DOC-39 Slice B).
    ///
    /// Why: `tests/search_hits_e2e.rs` needs the spawned `tcode` daemon to
    /// resolve `trusty-search` (the binary `tools::trusty_search` spawns) to
    /// a FAKE MCP server script instead of a real trusty-search install —
    /// the only seam for that is prepending a fixture directory to the
    /// child's `PATH`. No other e2e scenario needs extra env vars, so this
    /// stays a separate opt-in form rather than growing every existing call
    /// site's arity.
    /// Test: `search_hits_e2e::search_code_hit_paths_and_scores_reach_the_search_performed_event`.
    pub fn spawn_with_mock_llm_variant_and_envs(
        project: &std::path::Path,
        mock_llm_value: &str,
        extra_envs: &[(&str, &str)],
    ) -> Self {
        Self::spawn_inner(Some(project), Some(mock_llm_value), None, &[], extra_envs)
    }

    /// Spawn `tcode serve --stdio` with NO `--project` — a PROJECTLESS daemon.
    ///
    /// Why: this is the state screen 7a renders and the one that was previously
    /// impossible to even start: `--project` was a required `PathBuf`. That the
    /// daemon accepts this invocation at all is half of what the projectless
    /// e2e proves.
    /// What: omits `--project` entirely and points the child's `HOME` at
    /// `home` so the user-level `~/.claude/agents` fallback resolves the
    /// fixture's agent configs hermetically. `TCODE_MOCK_LLM=echo` as usual.
    /// Test: `task_e2e::projectless_task_runs_end_to_end`.
    pub fn spawn_projectless_with_mock_llm(home: &std::path::Path) -> Self {
        Self::spawn_inner(
            None,
            Some(trusty_code::task::mock_llm::MOCK_LLM_ECHO),
            Some(home),
            &[],
            &[],
        )
    }

    fn spawn_inner(
        project: Option<&std::path::Path>,
        mock_llm_value: Option<&str>,
        home: Option<&std::path::Path>,
        envs: &[(&str, &str)],
        extra_envs: &[(&str, &str)],
    ) -> Self {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_tcode"));
        cmd.arg("serve");
        // #4255, #9139: the child is `target/<profile>/tcode`, not a `deps/`
        // test binary, so it cannot detect the harness from its own path, and
        // it inherited the operator's daemons and palace. `ChildIsolation`
        // clears the environment and sets `TRUSTY_TEST_HARNESS=1` plus a
        // private `HOME`, data root and memory socket.
        let isolation = ChildIsolation::new();
        cmd.env_clear().envs(isolation.env());
        for (key, value) in extra_envs {
            cmd.env(key, value);
        }
        if let Some(project) = project {
            cmd.arg("--project").arg(project);
        }
        cmd.arg("--stdio");
        if let Some(home) = home {
            cmd.env("HOME", home);
        }
        if let Some(value) = mock_llm_value {
            cmd.env(trusty_code::task::mock_llm::MOCK_LLM_ENV, value);
        }
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `tcode serve --stdio`");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let lines = BufReader::new(stdout).lines();
        Self {
            child,
            stdin,
            lines,
            _project: None,
            _isolation: isolation,
        }
    }

    /// Write one JSON-RPC 2.0 request line; does not read a response.
    ///
    /// Why: `session.send`'s response and its streamed notification can
    /// arrive in either order on the wire (see `session_e2e.rs`), so that
    /// call site needs to write first and read separately via
    /// `read_lines`, rather than going through [`Self::call`].
    pub async fn write_request(&mut self, id: i64, method: &str, params: Value) {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_string(&req).expect("serialise request");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("write request line");
        self.stdin.flush().await.expect("flush stdin");
    }

    /// Write a request, then read lines (skipping any interleaved
    /// notifications) until the matching response arrives.
    ///
    /// Why: the common case — one request, one response, no observable
    /// side-stream traffic to correlate.
    /// What: panics after [`WIRE_TIMEOUT`] if no matching response shows up.
    pub async fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.write_request(id, method, params).await;
        timeout(WIRE_TIMEOUT, async {
            loop {
                let line = self.next_line().await;
                if let Some(resp) = find_response(&line, id) {
                    return resp;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for a response to id={id} ({method})"))
    }

    /// Read raw NDJSON lines until `max_lines` have been collected or
    /// [`WIRE_TIMEOUT`] elapses, whichever comes first.
    ///
    /// Why: `session.send`'s test needs to observe BOTH its response and a
    /// streamed notification, which may arrive in either order — reading a
    /// bounded batch and classifying each line afterward (via
    /// [`find_response`]/[`find_session_event`]) is simpler and more robust
    /// than trying to predict the interleaving.
    pub async fn read_lines(&mut self, max_lines: usize) -> Vec<String> {
        let mut collected = Vec::new();
        let _ = timeout(WIRE_TIMEOUT, async {
            while collected.len() < max_lines {
                collected.push(self.next_line().await);
            }
        })
        .await;
        collected
    }

    /// Read exactly one NDJSON line, panicking on EOF or an I/O error.
    async fn next_line(&mut self) -> String {
        self.lines
            .next_line()
            .await
            .expect("read stdout line")
            .expect("tcode serve --stdio closed stdout unexpectedly")
    }

    /// Close stdin (EOF) and assert the daemon exits cleanly.
    ///
    /// Why: proves the STDIO transport's documented EOF-triggers-shutdown
    /// behaviour end-to-end, not just via the offline `transport::tests`.
    pub async fn shutdown_via_eof_and_assert_clean_exit(mut self) {
        drop(self.stdin);
        let status = timeout(WIRE_TIMEOUT, self.child.wait())
            .await
            .expect("timed out waiting for the daemon to exit after stdin EOF")
            .expect("wait() failed");
        assert!(
            status.success(),
            "daemon must exit 0 on stdin EOF, got {status:?}"
        );
    }
}

/// A running `tcode serve` subprocess — the persistent daemon on its Unix
/// socket (#6637 slice 2c replaced the `tcode serve --http` `HttpDaemon`).
pub struct SocketDaemon {
    child: Child,
    /// The socket the daemon binds: `<data>/trusty-code/trusty-code.sock`
    /// inside this daemon's own isolated data directory.
    pub socket: std::path::PathBuf,
    /// Kept alive so a daemon that owns its project root (see
    /// [`spawn_socket_daemon`]) removes that root on drop. `None` when the
    /// caller supplied the root.
    _project: Option<tempfile::TempDir>,
    /// #9139: the child's private `HOME`, data root and memory socket.
    isolation: ChildIsolation,
}

/// How long a spawned daemon may take to answer on its socket.
const SOCKET_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-frame budget for reading a `session.events` stream — a live event can
/// trail its trigger by a `task.run`'s whole mock execution.
const STREAM_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawn `tcode serve` rooted at a throwaway tempdir the returned daemon
/// owns, and wait until its socket answers.
///
/// #2074: rooted at a tempdir, never `.` — `tcode serve` materializes the
/// agent roster into its project's `.trusty-code/agents/`.
pub async fn spawn_socket_daemon() -> SocketDaemon {
    let project = tempfile::tempdir().expect("throwaway project root");
    let mut daemon = spawn_socket_daemon_with_env(project.path(), &[]).await;
    daemon._project = Some(project);
    daemon
}

/// Spawn `tcode serve --project <project>` with `envs` set in the child's
/// environment, and wait until its socket answers.
///
/// Why: (#2062) the M1 milestone suite's socket scenario needs the SAME
/// `TCODE_MOCK_LLM=echo` offline determinism the STDIO scenario gets from
/// `StdioSession::spawn_with_mock_llm`.
/// What: the child runs with [`ChildIsolation`]'s environment, so its socket
/// lands in its own data directory; stderr goes to `<home>/daemon.stderr`,
/// quoted if the daemon exits before answering. `kill_on_drop` stops it if a
/// test panics before [`SocketDaemon::shutdown_via_sigterm_and_assert_clean_exit`].
pub async fn spawn_socket_daemon_with_env(
    project: &std::path::Path,
    envs: &[(&str, &str)],
) -> SocketDaemon {
    let isolation = ChildIsolation::new();
    let socket = isolation
        .data_dir()
        .join("trusty-code")
        .join("trusty-code.sock");
    let log = isolation.home().join("daemon.stderr");
    let stderr = std::fs::File::create(&log).expect("daemon stderr log");
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_tcode"));
    cmd.arg("serve").arg("--project").arg(project);
    // #4255, #9139: same reason as `spawn_inner` — the spawned `tcode` cannot
    // detect the test harness from its own path, and inherited the operator's
    // daemons and palace.
    cmd.env_clear().envs(isolation.env());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()
        .expect("spawn `tcode serve`");

    let started = std::time::Instant::now();
    while !trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(200)).await {
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!(
                "`tcode serve` exited ({status}) before answering on {}\nstderr:\n{}",
                socket.display(),
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        assert!(
            started.elapsed() < SOCKET_READY_TIMEOUT,
            "`tcode serve` never answered on {}\nstderr:\n{}",
            socket.display(),
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    SocketDaemon {
        child,
        socket,
        _project: None,
        isolation,
    }
}

impl SocketDaemon {
    /// One JSON-RPC call over the socket, returning the whole response frame
    /// (`{"jsonrpc","id","result"|"error"}`) as JSON — the shape the HTTP
    /// `POST /rpc` body had, so `resp["error"].is_null()` reads the same.
    pub async fn call(&self, id: i64, method: &str, params: Value) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        trusty_common::uds::send_framed_request::<Value, Value>(
            &self.socket,
            &request,
            STREAM_FRAME_TIMEOUT,
        )
        .await
        .unwrap_or_else(|e| panic!("{method} must answer over the socket: {e}"))
    }

    /// Open one `session.events` stream: the session's ring-buffer replay,
    /// then its live events, on ONE connection (#2055, #6637).
    pub async fn open_events(&self, session_id: &str) -> trusty_common::uds::FramedStream<Value> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "session.events",
            "params": {"session_id": session_id},
            "stream": true,
        });
        trusty_common::uds::send_framed_stream_request(&self.socket, &request, STREAM_FRAME_TIMEOUT)
            .await
            .unwrap_or_else(|e| panic!("session.events must open over the socket: {e}"))
    }

    /// The daemon's own stderr so far, for a failure message.
    pub fn stderr(&self) -> String {
        std::fs::read_to_string(self.isolation.home().join("daemon.stderr")).unwrap_or_default()
    }

    /// Send SIGTERM and assert the daemon exits cleanly and unlinks its
    /// socket (issue #534 connection-safe restart, against the real process).
    pub async fn shutdown_via_sigterm_and_assert_clean_exit(mut self) {
        let pid = self.child.id().expect("child has a pid") as i32;
        // Safety: `kill(2)` with a valid pid and the standard termination
        // signal; no memory is touched, only a signal is delivered.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let status = timeout(Duration::from_secs(20), self.child.wait())
            .await
            .expect("timed out waiting for the daemon to exit after SIGTERM")
            .expect("wait() failed");
        assert!(
            status.success(),
            "daemon must exit 0 after graceful SIGTERM shutdown, got {status:?}\nstderr:\n{}",
            self.stderr()
        );
        assert!(
            !self.socket.exists(),
            "a clean stop must unlink {}",
            self.socket.display()
        );
    }
}

/// Read frames from `stream` into `frames` until one satisfies `until`.
///
/// Why: the #2055 replay -> live continuity check reads ONE stream in stages
/// (the replay burst, then live events published while still connected), so
/// each stage appends to the same `frames` and the contiguity check runs over
/// the whole of it. A frame already read that satisfies `until` returns at
/// once.
/// What: panics on a stream error, a stream that ends first, or a frame
/// timeout ([`STREAM_FRAME_TIMEOUT`] per frame, set when the stream opened).
pub async fn read_stream_until(
    stream: &mut trusty_common::uds::FramedStream<Value>,
    frames: &mut Vec<Value>,
    until: impl Fn(&Value) -> bool,
) {
    if frames.iter().any(&until) {
        return;
    }
    loop {
        match stream.next_frame().await {
            Some(Ok(frame)) => {
                let done = until(&frame);
                frames.push(frame);
                if done {
                    return;
                }
            }
            Some(Err(e)) => panic!("session.events failed after {frames:?}: {e}"),
            None => panic!("session.events ended before the awaited frame: {frames:?}"),
        }
    }
}

/// A predicate for [`read_stream_until`]: the envelope's `kind` is `kind`.
pub fn kind_is(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |frame| frame["kind"] == kind
}

/// Parse `line` as JSON and return it if it's a response whose `id` matches.
///
/// Why: `session.send`'s response and a streamed notification can arrive
/// interleaved with each other (and, in principle, with earlier
/// still-in-flight lines); this is the "is this line the response I'm
/// waiting for" classifier.
pub fn find_response(line: &str, id: i64) -> Option<Value> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("id").and_then(|i| i.as_i64()) == Some(id) {
        Some(v)
    } else {
        None
    }
}

/// Parse `line` as JSON and return its `params` if it's a `session.event`
/// notification for `session_id`.
///
/// Why: the classifier for the OTHER half of an interleaved
/// response/notification pair — a server-initiated notification has no
/// `id`, a `method` field, and this crate's wire shape nests the actual
/// `crate::events::Event` under `params.event`.
pub fn find_session_event(line: &str, session_id: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("method").and_then(|m| m.as_str()) == Some("session.event")
        && v["params"]["session_id"].as_str() == Some(session_id)
    {
        Some(v["params"].clone())
    } else {
        None
    }
}

/// Drive `task.run` -> `session.attach` -> read-until-`session_done` over an
/// already-spawned [`StdioSession`], returning the session id once the run
/// has reached a terminal state.
///
/// Why: (#2058) both `tests/task_e2e.rs`'s live-tool-event assertions and its
/// `session.get_transcript` assertions need "run a task to completion" as a
/// precondition — `get_transcript` in particular must not be called before
/// `task::executor::run_and_record` has stored the outcome via
/// `SessionRegistry::set_run_outcome`, or the transcript would still be
/// empty. Factored out so both call sites share the exact same
/// read-until-terminal logic rather than copy-pasting the polling loop.
/// What: calls `task.run` (id 1) and `session.attach` (id 2) — these two ids
/// are reserved by this helper, so callers must start their OWN subsequent
/// request ids at 3 — then reads NDJSON lines, classifying each as a
/// `session.event` for this session, until `session_done` is observed (or
/// panics after 20 read rounds, mirroring the bounded wait every other e2e
/// polling loop in this crate uses). Returns the session id.
pub async fn run_task_to_completion(daemon: &mut StdioSession, task_description: &str) -> String {
    let run_resp = daemon
        .call(1, "task.run", json!({"task_description": task_description}))
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let mut kinds: Vec<String> = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array")
        .iter()
        .map(|e| {
            e["kind"]
                .as_str()
                .expect("kind must be a string")
                .to_string()
        })
        .collect();

    let mut iterations = 0;
    while !kinds.iter().any(|k| k == "session_done") {
        iterations += 1;
        assert!(
            iterations < 20,
            "gave up waiting for session_done after {iterations} read rounds; kinds so far: {kinds:?}"
        );
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "timed out waiting for more events; kinds so far: {kinds:?}"
        );
        for line in &lines {
            if let Some(envelope) = find_session_event(line, &session_id) {
                kinds.push(
                    envelope["kind"]
                        .as_str()
                        .expect("kind must be a string")
                        .to_string(),
                );
            }
        }
    }

    session_id
}

/// Assert that `envelopes` (in receipt order) each carry every
/// `SessionEventEnvelope` field and form ONE gap-free, strictly increasing
/// `seq` sequence; returns `(first_seq, last_seq)`.
///
/// Why: the #2055 correctness property this whole e2e suite exists to
/// prove — factored out so the STDIO and socket scenarios run the exact
/// same check against their respective wire representations of the same
/// envelope shape.
/// What: panics with the offending envelope on the first missing field or
/// any `seq` that isn't exactly `previous + 1`. Panics on empty input too —
/// every call site here always has at least the replay burst to check.
pub fn assert_envelopes_contiguous(envelopes: &[Value]) -> (u64, u64) {
    assert!(
        !envelopes.is_empty(),
        "expected at least one envelope to check"
    );
    let mut previous: Option<u64> = None;
    for envelope in envelopes {
        for field in ["session_id", "seq", "at", "kind", "event"] {
            assert!(
                envelope.get(field).is_some(),
                "envelope missing required field {field:?}: {envelope}"
            );
        }
        let seq = envelope["seq"].as_u64().expect("seq must be a u64");
        if let Some(prev) = previous {
            assert_eq!(
                seq,
                prev + 1,
                "seq must increase by exactly 1 with no gaps or duplicates: {envelope}"
            );
        }
        previous = Some(seq);
    }
    let first = envelopes[0]["seq"].as_u64().unwrap();
    let last = envelopes[envelopes.len() - 1]["seq"].as_u64().unwrap();
    (first, last)
}
