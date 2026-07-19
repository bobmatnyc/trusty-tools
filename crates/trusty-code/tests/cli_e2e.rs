//! Black-box CLI end-to-end test for #2060: the M1 cut-line "replay via
//! thin CLI" verb.
//!
//! Why: #2053-#2058 proved the daemon + JSON-RPC surface works over its own
//! wire (`tests/session_e2e.rs`, `tests/task_e2e.rs`), driven either
//! directly against `tcode serve` or via the small test-only `StdioSession`
//! helper. This ticket's own deliverable — the `tcode` BINARY's subcommands
//! — needs its OWN black-box proof: spawn the compiled `tcode` binary
//! (`env!("CARGO_BIN_EXE_tcode")`) exactly as a user's shell would, let it
//! do whatever it does internally (which, per #2060, is spawn its OWN
//! nested `tcode serve --stdio` child and speak JSON-RPC to it — this test
//! never touches that nested layer directly), and assert only on the
//! OUTER process's stdout/stderr/exit code. `TCODE_MOCK_LLM=echo` keeps
//! `run-task` deterministic and offline (env propagates from this outer
//! process to the CLI's own spawned nested daemon child, since it inherits
//! the environment by default).
//! What: [`run_task_streams_live_events_and_reports_final_status`] is the
//! REQUIRED case — `tcode run-task python-engineer "<task>" --project <tmp>`
//! must print live tool events and a final `status=finished` line, exit 0.
//! `run_task_json_mode_prints_only_the_final_session` proves `--json`
//! suppresses the per-event lines and emits one parseable JSON document.
//! `run_task_mode_precedence_over_the_wire` (#2059) proves the three-tier
//! `HarnessMode` precedence — env var > `--mode` flag > `.claude/settings.json`
//! > default — end to end via the REAL CLI binary and its `--json` response.
//!
//! `session_list_on_fresh_project_reports_no_sessions`,
//! `transcript_unknown_session_errors_cleanly`,
//! `attach_unknown_session_errors_cleanly`, and
//! `cancel_unknown_session_errors_cleanly` prove the remaining subcommands
//! are wired and that daemon errors surface as clear CLI errors and a
//! nonzero exit (per the ticket's explicit requirement) — see those
//! subcommands' own module docs (`crate::cli::session`/`attach`/`cancel`/
//! `transcript` in `src/cli/`) for why a MEANINGFUL cross-invocation
//! inspection test (attach to a session a DIFFERENT process created) isn't
//! possible yet: M1 sessions are in-memory, one per ephemeral spawned
//! daemon, and this ticket's required path is spawn-stdio, not a shared
//! daemon.
//!
//! `workstream_list_on_fresh_project_reports_no_workstreams` through
//! `workstream_close_hides_from_default_list_but_include_closed_shows_it`
//! (#3296, DOC-48 §5.4) prove the `tcode workstream`/`tcode ws` family end
//! to end. UNLIKE sessions, workstreams are persisted to a flat file keyed
//! on `--project` (`crate::workstreams::path`), so — unlike every
//! session/attach/cancel/transcript case above — these tests spawn the CLI
//! TWICE against the same `--project` and prove the SECOND invocation's
//! ephemeral daemon sees what the FIRST one persisted (create, then a
//! separate list; close, then a separate list). Each test pins `HOME` to
//! its own tempdir (`crate::workstreams::path::default_data_dir` resolves
//! `~/.trusty-code`) so the store file never touches the real developer's
//! home directory — the same sandboxing `support::home_with_user_level_agents`
//! already uses for agent resolution.
//!
//! Test: this file IS the test; see `support` for the shared
//! `project_with_agents` fixture.

mod support;

use std::process::Command;

use support::project_with_agents;

/// `tcode run-task <agent> "<task>" --project <tmp>` (`TCODE_MOCK_LLM=echo`)
/// must stream the PM's `delegate_to_agent` and the engineer's `bash` tool
/// events to stdout, then report `status=finished`, exiting 0.
#[test]
fn run_task_streams_live_events_and_reports_final_status() {
    let project = project_with_agents();
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "run-task",
            "pm",
            "say hi",
            "--project",
            &project.path().display().to_string(),
        ])
        .env("TCODE_MOCK_LLM", "echo")
        .output()
        .expect("spawn tcode run-task");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "tcode run-task must exit 0, got {:?}\nstdout: {stdout}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("tool_started") && stdout.contains("delegate_to_agent"),
        "expected the PM's delegate_to_agent dispatch in stdout: {stdout}"
    );
    assert!(
        stdout.contains("tool_started") && stdout.contains("bash"),
        "expected the engineer's bash dispatch in stdout: {stdout}"
    );
    assert!(
        stdout.contains("tool_finished"),
        "expected at least one tool_finished line in stdout: {stdout}"
    );
    assert!(
        stdout.contains("status=finished"),
        "expected the final status line to report finished: {stdout}"
    );
}

/// `tcode run-task ... --json` must suppress the live per-event lines and
/// print exactly one parseable JSON document with `status: "finished"`.
#[test]
fn run_task_json_mode_prints_only_the_final_session() {
    let project = project_with_agents();
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "run-task",
            "pm",
            "say hi",
            "--project",
            &project.path().display().to_string(),
            "--json",
        ])
        .env("TCODE_MOCK_LLM", "echo")
        .output()
        .expect("spawn tcode run-task --json");

    assert!(
        output.status.success(),
        "tcode run-task --json must exit 0, got {:?}",
        output.status.code()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("tool_started"),
        "--json must suppress live event lines: {stdout}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}: {stdout}"));
    assert_eq!(parsed["status"], "finished");
}

/// Run `tcode run-task ... --json`, optionally with `--mode`, a
/// `.claude/settings.json`, and/or `TRUSTY_CODE_MODE`, returning the
/// resolved `mode` string from the parsed JSON response.
fn run_task_resolved_mode(
    project: &std::path::Path,
    cli_mode: Option<&str>,
    env_mode: Option<&str>,
) -> String {
    let mut args = vec![
        "run-task".to_string(),
        "pm".to_string(),
        "say hi".to_string(),
        "--project".to_string(),
        project.display().to_string(),
        "--json".to_string(),
    ];
    if let Some(m) = cli_mode {
        args.push("--mode".to_string());
        args.push(m.to_string());
    }

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tcode"));
    cmd.args(&args).env("TCODE_MOCK_LLM", "echo");
    if let Some(m) = env_mode {
        cmd.env("TRUSTY_CODE_MODE", m);
    } else {
        cmd.env_remove("TRUSTY_CODE_MODE");
    }
    let output = cmd.output().expect("spawn tcode run-task");
    assert!(
        output.status.success(),
        "tcode run-task must exit 0: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}: {stdout}"));
    parsed["mode"]
        .as_str()
        .unwrap_or_else(|| panic!("response must carry a mode string: {parsed}"))
        .to_string()
}

/// #2059's three-tier `HarnessMode` precedence, proven over the REAL wire
/// via the `tcode` CLI (not just `crate::mode::resolve_mode`'s own offline
/// unit tests, and not just `task::protocol::tests`' direct-handler-call
/// integration test): `TRUSTY_CODE_MODE` env var > `task.run`'s `mode` param
/// (here, the CLI's `--mode` flag) > `.claude/settings.json`'s
/// `code_harness.mode` > default `daily-driver`.
#[test]
fn run_task_mode_precedence_over_the_wire() {
    // 1. Nothing set anywhere -> default.
    let project = project_with_agents();
    assert_eq!(
        run_task_resolved_mode(project.path(), None, None),
        "daily-driver"
    );

    // 2. `.claude/settings.json` alone sets parity.
    std::fs::write(
        project.path().join(".claude").join("settings.json"),
        r#"{"code_harness": {"mode": "parity"}}"#,
    )
    .expect("write settings.json");
    assert_eq!(run_task_resolved_mode(project.path(), None, None), "parity");

    // 3. The CLI's `--mode` flag (task.run's `mode` param) overrides
    //    settings.json.
    assert_eq!(
        run_task_resolved_mode(project.path(), Some("daily-driver"), None),
        "daily-driver"
    );

    // 4. `TRUSTY_CODE_MODE` overrides EVERYTHING, including `--mode`.
    assert_eq!(
        run_task_resolved_mode(project.path(), Some("daily-driver"), Some("parity")),
        "parity"
    );
}

/// `tcode session list` on a project with no prior activity must report no
/// sessions and exit 0 — proving the `session.list` wiring even though this
/// ephemeral-spawn CLI invocation's daemon has an empty registry.
#[test]
fn session_list_on_fresh_project_reports_no_sessions() {
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "session",
            "list",
            "--project",
            &project.path().display().to_string(),
        ])
        .output()
        .expect("spawn tcode session list");

    assert!(output.status.success(), "must exit 0: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no sessions"),
        "expected an empty-list message: {stdout}"
    );
}

/// `tcode transcript <unknown-id>` must surface the daemon's
/// `session_not_found` error as a clean CLI error on stderr and exit
/// nonzero.
#[test]
fn transcript_unknown_session_errors_cleanly() {
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "transcript",
            "nonexistent-id",
            "--project",
            &project.path().display().to_string(),
        ])
        .output()
        .expect("spawn tcode transcript");

    assert!(
        !output.status.success(),
        "must exit nonzero on session_not_found"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session not found") || stderr.contains("-32007"),
        "expected a clear session_not_found error on stderr: {stderr}"
    );
}

/// `tcode attach <unknown-id>` must likewise surface a clean error and exit
/// nonzero rather than hanging.
#[test]
fn attach_unknown_session_errors_cleanly() {
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "attach",
            "nonexistent-id",
            "--project",
            &project.path().display().to_string(),
        ])
        .output()
        .expect("spawn tcode attach");

    assert!(!output.status.success(), "must exit nonzero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session not found") || stderr.contains("-32007"),
        "expected a clear session_not_found error on stderr: {stderr}"
    );
}

/// `tcode cancel <unknown-id>` must likewise surface a clean error and exit
/// nonzero.
#[test]
fn cancel_unknown_session_errors_cleanly() {
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "cancel",
            "nonexistent-id",
            "--project",
            &project.path().display().to_string(),
        ])
        .output()
        .expect("spawn tcode cancel");

    assert!(!output.status.success(), "must exit nonzero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session not found") || stderr.contains("-32007"),
        "expected a clear session_not_found error on stderr: {stderr}"
    );
}

/// `tcode --version` must print the semver AND the git provenance (#2823).
///
/// Why: This is #2823's headline acceptance criterion, and the unit tests can
/// only assert on constants — they cannot prove the constants are actually
/// WIRED into clap's `--version`. Before this ticket the binary printed a bare
/// `tcode 0.2.0`, which is exactly what a `version` (no `= …`) clap attribute
/// still produces, so a regression here would be invisible to
/// `build_info`'s tests. Only spawning the real binary catches it.
/// What: Spawn the compiled binary with `--version`; assert exit 0 and that
/// stdout carries the semver plus a parenthesised `(<commit> <date>)` stamp
/// matching the compiled-in constants.
#[test]
fn version_flag_reports_build_provenance() {
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .arg("--version")
        .output()
        .expect("spawn tcode --version");

    assert!(output.status.success(), "--version must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "expected the semver in --version output: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "({} {})",
            trusty_code::build_info::GIT_HASH,
            trusty_code::build_info::COMMIT_DATE
        )),
        "expected a `(<commit> <date>)` provenance stamp: {stdout}"
    );
    // Guards the pre-#2823 regression shape: a bare `tcode <semver>` line.
    assert!(
        stdout.trim() != format!("tcode {}", env!("CARGO_PKG_VERSION")),
        "--version regressed to a bare semver with no provenance: {stdout}"
    );
}

/// `tcode workstream create --name <NAME> --project <project_arg>`, run
/// against `home`, returning the newly-minted id parsed out of its
/// `created workstream <id>` confirmation line.
///
/// Why: every workstream e2e test below needs at least one persisted
/// workstream to act on; factored out so each test's setup is one line
/// instead of a repeated Command/parse block.
fn create_workstream(home: &std::path::Path, project_arg: &str, name: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "workstream",
            "create",
            "--name",
            name,
            "--project",
            project_arg,
        ])
        .env("HOME", home)
        .output()
        .expect("spawn tcode workstream create");
    assert!(
        output.status.success(),
        "workstream create must exit 0: {output:?}"
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .strip_prefix("created workstream ")
        .expect("expected the `created workstream <id>` confirmation shape")
        .to_string()
}

/// `tcode workstream list` on a project with no prior workstreams must
/// report none and exit 0 (DOC-48 AC-5.1).
#[test]
fn workstream_list_on_fresh_project_reports_no_workstreams() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "workstream",
            "list",
            "--project",
            &project.path().display().to_string(),
        ])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode workstream list");

    assert!(output.status.success(), "must exit 0: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no workstreams"),
        "expected an empty-list message: {stdout}"
    );
}

/// `tcode workstream create` persists to a flat file keyed on `--project`
/// (DOC-48 §3.1) — a SEPARATE `tcode ws list` invocation against the SAME
/// project must see what a prior, already-exited invocation created. This
/// also proves the `ws` alias (DOC-48 AC-5.3).
#[test]
fn workstream_create_persists_and_ws_alias_lists_it() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let project_arg = project.path().display().to_string();

    create_workstream(home.path(), &project_arg, "Token rotation hardening");

    let list_output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args(["ws", "list", "--project", &project_arg])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode ws list");
    assert!(
        list_output.status.success(),
        "ws list must exit 0: {list_output:?}"
    );
    let list_stdout = String::from_utf8_lossy(&list_output.stdout);
    assert!(
        list_stdout.contains("Token rotation hardening"),
        "a SEPARATE invocation against the same --project must see the persisted workstream: {list_stdout}"
    );
    assert!(
        list_stdout.contains("idle"),
        "a freshly-created workstream must show idle: {list_stdout}"
    );
}

/// `tcode workstream get <unknown-uuid>` must surface `not_found` cleanly
/// and exit nonzero.
#[test]
fn workstream_get_unknown_id_errors_cleanly() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "workstream",
            "get",
            "00000000-0000-0000-0000-000000000000",
            "--project",
            &project.path().display().to_string(),
        ])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode workstream get");

    assert!(!output.status.success(), "must exit nonzero on not_found");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("-32002"),
        "expected a clear not_found error on stderr: {stderr}"
    );
}

/// `tcode workstream activate` without `--force` while a DIFFERENT
/// workstream is active must name the active workstream and suggest
/// `--force`, exiting nonzero (DOC-48 §5.2/§6.1's `ActiveConflict`
/// scenario, the ONE piece of business logic this CLI ticket's scope
/// explicitly calls out).
#[test]
fn workstream_activate_conflict_names_active_and_suggests_force() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let project_arg = project.path().display().to_string();

    let id_a = create_workstream(home.path(), &project_arg, "A");
    let id_b = create_workstream(home.path(), &project_arg, "B");

    let activate_a = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args(["workstream", "activate", &id_a, "--project", &project_arg])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode workstream activate a");
    assert!(
        activate_a.status.success(),
        "activating a with no prior active must succeed: {activate_a:?}"
    );

    let activate_b = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args(["workstream", "activate", &id_b, "--project", &project_arg])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode workstream activate b");
    assert!(
        !activate_b.status.success(),
        "activating b without --force while a is active must fail"
    );
    let stderr = String::from_utf8_lossy(&activate_b.stderr);
    assert!(
        stderr.contains(&id_a),
        "must name the currently-active workstream: {stderr}"
    );
    assert!(stderr.contains("--force"), "must suggest --force: {stderr}");
}

/// `tcode workstream deactivate` with no active workstream is a documented
/// no-op (DOC-48 §4.3) — must print a clear message and exit 0, never an
/// error.
#[test]
fn workstream_deactivate_with_none_active_is_a_clean_noop() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "workstream",
            "deactivate",
            "--project",
            &project.path().display().to_string(),
        ])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode workstream deactivate");

    assert!(output.status.success(), "must exit 0: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no workstream is active"),
        "expected a clear no-op message: {stdout}"
    );
}

/// `tcode workstream close` then `tcode workstream list` (default) must hide
/// the closed workstream; `--include-closed` must still show it (DOC-48
/// §4.4).
#[test]
fn workstream_close_hides_from_default_list_but_include_closed_shows_it() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let project_arg = project.path().display().to_string();

    let id = create_workstream(home.path(), &project_arg, "Old spike");

    let close_output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args(["workstream", "close", &id, "--project", &project_arg])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode workstream close");
    assert!(
        close_output.status.success(),
        "close must exit 0: {close_output:?}"
    );

    let default_list = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args(["workstream", "list", "--project", &project_arg])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode workstream list");
    let default_stdout = String::from_utf8_lossy(&default_list.stdout);
    assert!(
        !default_stdout.contains("Old spike"),
        "closed workstream must be hidden from the default list: {default_stdout}"
    );

    let include_closed_list = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "workstream",
            "list",
            "--include-closed",
            "--project",
            &project_arg,
        ])
        .env("HOME", home.path())
        .output()
        .expect("spawn tcode workstream list --include-closed");
    let include_closed_stdout = String::from_utf8_lossy(&include_closed_list.stdout);
    assert!(
        include_closed_stdout.contains("Old spike") && include_closed_stdout.contains("closed"),
        "include-closed list must show the closed workstream: {include_closed_stdout}"
    );
}
