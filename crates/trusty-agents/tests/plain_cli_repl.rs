//! Piped-stdin integration test for the persistent plain-line CLI mode
//! (`--plain` / `TAGENT_NO_TUI=1`, #3052).
//!
//! Why: `--plain` exists so testers can drive `tagent` over SSH from a
//! narrow terminal (Terminus/iPhone) without the ratatui full-screen TUI,
//! which reflows badly and has a modal keystroke-swallow bug. The only way
//! to prove the persistent line loop actually stays resident, dispatches
//! slash commands, and exits cleanly is to drive the REAL compiled binary
//! with piped stdin — an in-process unit test can't observe the stdin/stdout
//! loop in `ctrl::run_plain_cli`. Mirrors the `CARGO_BIN_EXE_tagent` pattern
//! already used by `tests/config_mount.rs`.
//! What: Spawns `tagent --plain` (and separately with `TAGENT_NO_TUI=1` and
//! no flag) with `TAGENT_NONINTERACTIVE=1` so the first-run profile
//! interview is skipped deterministically, feeds `"/help\n/quit\n"` on
//! stdin, and asserts the process printed the `you>` prompt + help text and
//! exited 0 within a bounded wait. No free-text chat line is sent, so the
//! test needs no LLM credential and never depends on network access —
//! satisfying the "don't make CI depend on a key" requirement.
//! Test: this file IS the test.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Absolute path to the freshly built `tagent` binary (Cargo sets this for
/// integration tests that share a package with a `[[bin]]` target).
const BIN: &str = env!("CARGO_BIN_EXE_tagent");

/// Bound on how long we'll wait for the child to exit. `/quit` after `/help`
/// itself returns in well under a second, but `tagent`'s startup path spawns
/// background MCP plugin handshakes and a tmux-backed session monitor shared
/// with every other interactive mode (TUI included) — under a fully loaded
/// `cargo test -p trusty-agents` run (thousands of unit tests plus several
/// other subprocess-spawning integration tests competing for CPU) that
/// startup can legitimately take tens of seconds. 120s leaves headroom for
/// that contention while still failing (rather than hanging the suite
/// forever) if a real regression makes the loop stop reading stdin or exit.
const WAIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Spawn `tagent` with `extra_args`/`extra_env`, write `stdin_input`, close
/// stdin (EOF), and return `(exit_status, stdout, stderr)` once the process
/// exits or the timeout elapses (whichever comes first — panics on timeout).
///
/// Runs with `current_dir` pinned to a fresh, isolated tempdir. Why: `tagent`
/// unconditionally bumps a persistent `.trusty-agents/state/build.json`
/// counter at startup (`runtime::startup::run_startup_init`), resolved via
/// `ctrl::detect_self_project()`'s cwd walk-up. Without isolation, every
/// spawned process in this file (and every OTHER integration test binary
/// that also spawns the real binary, all running concurrently under `cargo
/// test`) would race to rename the SAME `build.json.tmp` in the shared crate
/// root, which is an unrelated pre-existing non-atomicity this test doesn't
/// need to depend on. A cwd with no `.trusty-agents/agents/pm.toml` (and no
/// such ancestor) makes `detect_self_project()` return `None`, so startup
/// falls back to `std::env::current_dir()` — the private tempdir — for its
/// state directory instead.
fn run_piped(
    extra_args: &[&str],
    extra_env: &[(&str, &str)],
    stdin_input: &str,
) -> (bool, String, String) {
    run_piped_with_setup(extra_args, extra_env, stdin_input, |_| {})
}

/// Same as [`run_piped`], but calls `setup(tempdir_path)` after creating the
/// isolated cwd and before spawning — lets a test seed fixture files (e.g. a
/// minimal `assistant.toml`) into the isolated project without touching the
/// real crate-root `.trusty-agents/`.
fn run_piped_with_setup(
    extra_args: &[&str],
    extra_env: &[(&str, &str)],
    stdin_input: &str,
    setup: impl FnOnce(&Path),
) -> (bool, String, String) {
    let isolated_cwd = tempfile::tempdir().expect("create isolated tempdir for tagent cwd");
    setup(isolated_cwd.path());

    let mut cmd = Command::new(BIN);
    cmd.args(extra_args)
        .current_dir(isolated_cwd.path())
        // Deterministic: skip the interactive first-run profile interview,
        // which would otherwise consume lines meant for the REPL loop.
        .env("TAGENT_NONINTERACTIVE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().expect("spawn tagent");
    {
        let mut stdin = child.stdin.take().expect("child stdin was piped");
        stdin
            .write_all(stdin_input.as_bytes())
            .expect("write stdin");
        // `stdin` drops here, closing the pipe (EOF) if the loop doesn't
        // exit via `/quit` first.
    }

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = child.wait_with_output();
        let _ = tx.send(out);
    });
    let out = rx
        .recv_timeout(WAIT_TIMEOUT)
        .unwrap_or_else(|_| {
            panic!("tagent did not exit within {WAIT_TIMEOUT:?} (hang regression?)")
        })
        .expect("wait_with_output failed");

    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn plain_flag_stays_resident_and_exits_cleanly_on_quit() {
    let (success, stdout, stderr) = run_piped(&["--plain"], &[], "/help\n/quit\n");
    assert!(
        success,
        "tagent --plain should exit 0 on /quit; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("you> "),
        "plain CLI should print the `you> ` prompt each turn; got:\n{stdout}"
    );
    assert!(
        stdout.contains("trusty-agents REPL — slash commands"),
        "/help should print the shared slash-command reference; got:\n{stdout}"
    );
    assert!(
        stdout.contains("/model") && stdout.contains("/provider"),
        "/help output should advertise /model and /provider; got:\n{stdout}"
    );
    assert!(
        stdout.contains("resolved agent endpoint"),
        "startup banner should show the resolved agent endpoint; got:\n{stdout}"
    );
    // No ratatui alt-screen escape sequence should ever appear in plain mode.
    assert!(
        !stdout.contains("\x1b[?1049h"),
        "plain CLI must never enter the alt-screen; got:\n{stdout}"
    );
}

#[test]
fn no_tui_env_forces_plain_mode_without_the_flag() {
    // Same behavior via TAGENT_NO_TUI=1 with no --plain flag — proves the
    // env var alone is sufficient to bypass the TUI.
    let (success, stdout, stderr) = run_piped(&[], &[("TAGENT_NO_TUI", "1")], "/quit\n");
    assert!(
        success,
        "TAGENT_NO_TUI=1 tagent should exit 0 on /quit; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("you> "),
        "TAGENT_NO_TUI=1 should route into the plain line loop; got:\n{stdout}"
    );
}

#[test]
fn quit_command_stops_the_loop_immediately() {
    // A single `/quit` (no `/help` first) should be enough to exit — proves
    // the loop doesn't require a specific command sequence to terminate.
    let (success, stdout, stderr) = run_piped(&["--plain"], &[], "/quit\n");
    assert!(success, "a bare /quit should exit 0; stderr:\n{stderr}");
    assert!(stdout.contains("you> "), "got:\n{stdout}");
}

/// Seed an isolated project's `.trusty-agents/agents/assistant.toml` with a
/// minimal, deterministic fixture — a distinctive `model` id so the
/// assertion doesn't depend on (or drift with) the real bundled
/// `assistant/agent.toml`'s actual model.
fn seed_fixture_assistant_agent(project_root: &Path) {
    let agents_dir = project_root.join(".trusty-agents").join("agents");
    std::fs::create_dir_all(&agents_dir).expect("create fixture agents dir");
    std::fs::write(
        agents_dir.join("assistant.toml"),
        "[agent]\n\
         name = \"assistant\"\n\
         role = \"assistant\"\n\
         model = \"test-vendor/test-opus-fixture\"\n\
         description = \"fixture assistant persona for plain-CLI default test\"\n\
         \n\
         [llm]\n\
         temperature = 0.7\n\
         max_tokens = 4096\n\
         \n\
         [system_prompt]\n\
         content = \"You are a fixture assistant persona used only by an automated test.\"\n",
    )
    .expect("write fixture assistant.toml");
}

#[test]
fn plain_cli_defaults_active_agent_to_assistant_not_ctrl() {
    // Owner decision (#3406 follow-up): the plain CLI's out-of-the-box
    // conversational agent must be `assistant` (what testers actually
    // exercise), not `ctrl` (the local-ollama machine-coordination
    // persona) — even though `TrustyAgentsRepl::new` itself still
    // constructs with `project_name = "ctrl"` / `active_persona = None`.
    let (success, stdout, stderr) =
        run_piped_with_setup(&["--plain"], &[], "/quit\n", seed_fixture_assistant_agent);
    assert!(success, "should exit 0 on /quit; stderr:\n{stderr}");
    assert!(
        stdout.contains("Switched to: assistant"),
        "startup should default-switch to the assistant persona; got:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "resolved agent endpoint: agent=assistant model=test-vendor/test-opus-fixture"
        ),
        "resolved agent endpoint banner should report agent=assistant with the assistant's \
         actual model (not ctrl's); got:\n{stdout}"
    );
    assert!(
        !stdout.contains("agent=ctrl"),
        "default agent must not be ctrl; got:\n{stdout}"
    );
}

#[test]
fn switch_ctrl_still_reaches_the_ctrl_coordinator() {
    // `ctrl` must remain fully reachable via `/switch ctrl` — defaulting to
    // `assistant` changes the DEFAULT, not ctrl's availability. `/switch
    // ctrl` doesn't need an agent-config lookup (it's special-cased to clear
    // `active_persona` and reset `project_name` to "ctrl"), so no fixture
    // seeding is needed here. `/connect` (TM-session coordination, not the
    // controller-socket path) is exercised separately by the live manual
    // transcript in the PR description — it doesn't need a real project
    // fixture to prove the slash routes correctly here.
    let (success, stdout, stderr) = run_piped(&["--plain"], &[], "/switch ctrl\n/connect\n/quit\n");
    assert!(success, "should exit 0 on /quit; stderr:\n{stderr}");
    assert!(
        stdout.contains("Switched to: ctrl"),
        "/switch ctrl should report switching back to ctrl; got:\n{stdout}"
    );
    // `/connect` with no args prints its usage line — proves the
    // machine-coordination slash command is still routed/reachable (not
    // shadowed or broken by the default-persona change), independent of
    // whether a real project path is supplied.
    assert!(
        stdout.contains("usage: /connect"),
        "/connect should still be reachable as a slash command; got:\n{stdout}"
    );
}
