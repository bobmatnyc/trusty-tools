//! `tm internal-spawn-disclaimed` — the pane's macOS TCC disclaim shim (#2997).
//!
//! Why: managed sessions launch `claude` by typing a command into a tmux pane;
//! the pane shell is forked by the shared tmux server, so a disclaim attribute
//! the daemon sets can never reach that `claude` and tccd blames the server.
//! [`crate::cli::Command::InternalSpawnDisclaimed`] (emitted into the pane
//! command by [`trusty_mpm::core::spawn_disclaim::disclaim_pane_command`]) is
//! the tm-owned process the pane routes `claude` through so it can be
//! `posix_spawn`ed WITH the disclaim attribute set.
//! What: [`run`] spawns the given program+args via the shared #3037
//! [`trusty_mpm::core::spawn_disclaim::disclaimed_status`] seam (inherited
//! stdio so `claude` stays interactive in the pane), waits, and exits with the
//! child's code. On macOS it first sets SIGINT/SIGQUIT to SIG_IGN so a pane
//! Ctrl-C reaches only `claude` (which the disclaimed spawn resets to SIG_DFL)
//! and the shim survives to reap and propagate the child's real exit status —
//! the standard wrapper convention. Non-macOS is a plain pass-through spawn (no
//! TCC there, and the shim is never emitted into a pane off macOS).
//! #9092: a launch spec naming the supervisor session records the Architect
//! launch record for the `claude` it spawns, or does not start it
//! ([`launch_from_spec`]).
//! Test: `run_rejects_empty_argv`; the disclaim/spawn behaviour is covered by
//! `trusty_mpm::core::spawn_disclaim`'s `disclaimed_status_*` tests, and the
//! wrapped-pane PID discovery + signal topology by
//! `core::process::claude_pid_resolves_through_disclaim_wrapper` (`#[ignore]`)
//! plus the manual macOS pre-merge checks noted on the PR.

use anyhow::Context as _;

/// Whether the raw process argv shows this shim was invoked by its EXACT,
/// full subcommand name — not an abbreviated prefix clap's
/// `infer_subcommands` (#4398) resolved to it.
///
/// Why (#4431 critic review, HIGH): `infer_subcommands` makes clap resolve
/// any unambiguous prefix of a subcommand name to that command, and it walks
/// HIDDEN subcommands too — `#[command(hide = true)]` on
/// `Command::InternalSpawnDisclaimed` only suppresses the name from
/// `--help`, it does not exempt it from prefix matching. Since [`run`]
/// unconditionally `posix_spawn`s `argv[0]` with macOS TCC responsibility
/// disclaimed (#2997), letting an abbreviation (`tm int <program>`, `tm
/// inte`, `tm internal`) reach it would spawn an arbitrary process through
/// this internal, hidden escape hatch. `main` calls this BEFORE dispatching
/// to [`run`], using the raw `std::env::args()` captured before clap ever
/// ran inference — `argv[1]` is the first token the operator actually typed,
/// at the position clap resolved to this variant.
/// What: `true` only when `argv.get(1)` is exactly
/// [`trusty_mpm::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND`].
/// `disclaim_pane_command`'s own self-invocation always emits the full
/// spelling, so this keeps that literal path working unchanged.
/// Test: `invoked_literally_accepts_exact_name`,
/// `invoked_literally_rejects_abbreviated_prefixes`,
/// `invoked_literally_rejects_missing_token`.
pub(crate) fn invoked_literally(argv: &[String]) -> bool {
    argv.get(1).map(String::as_str)
        == Some(trusty_mpm::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND)
}

/// Spawn `argv[0]` with `argv[1..]` disclaimed and exit with its status code.
///
/// Why: this is the leaf of the #2997 fix — the one process that actually sets
/// the `posix_spawn` disclaim attribute on the pane's `claude`, so `claude`
/// becomes its own TCC responsible process instead of the shared tmux server.
/// What: builds a `std::process::Command` from `argv` (stdio left as the
/// default inherit, matching the pane's interactive tty), spawns it through
/// [`trusty_mpm::core::spawn_disclaim::disclaimed_status`] — which disclaims on
/// macOS and is a plain `Command::status()` elsewhere — then
/// `std::process::exit`s with the child's code (127 when it was killed by a
/// signal, mirroring a shell). Never returns `Ok`: it either exits the process
/// or returns the spawn error (an empty `argv`, or a spawn failure such as a
/// missing binary).
/// Test: `run_rejects_empty_argv`.
pub(crate) fn run(argv: Vec<String>) -> anyhow::Result<()> {
    let mut it = argv.into_iter();
    let program = it
        .next()
        .context("internal-spawn-disclaimed requires a program to launch")?;
    let rest: Vec<String> = it.collect();
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&rest);
    let status = spawn_and_wait(&mut cmd, &program, |_| Ok(()))?.0;
    // Mirror a shell: propagate the child's exit code; use 127 when it was
    // terminated by a signal (no exit code available).
    std::process::exit(status);
}

/// Run a managed launch from its spec file (#8233).
///
/// Why: the pane can only type a bounded line, so the launch's cwd, argv and
/// environment travel in a mode-0600 file instead. This is the process that
/// reads it — the SAME process the #2997 disclaim contract already put between
/// the pane shell and `claude`, so the parameterisation costs no extra hop and
/// `claude` is still `posix_spawn`ed disclaimed.
/// What: consumes (reads AND deletes) the spec, writes the
/// `<session>.started` sentinel that tells the daemon the pane's SHELL really
/// ran the typed line (#8233 review — tmux accepting keystrokes never proved
/// that), builds the spec's [`std::process::Command`], spawns it disclaimed, and
/// on exit prints the three-way launch report
/// [`trusty_mpm::runtime::launch_report::report_exit`] produces — the behaviour
/// #6766 used to express as an `if`/`elif` shell fragment appended to the typed
/// line. Exits with the child's code.
///
/// The sentinel is written BEFORE the spawn deliberately: it answers "did the
/// shell consume the command", which is true whether or not `claude` then
/// starts, and the two failures need different remedies.
///
/// # Errors
///
/// A spec that is missing, unreadable or corrupt returns an error rather than
/// launching anything, and so does a `program` the OS refuses to spawn. `main`
/// prints it to the pane and exits non-zero, so the pane says what happened and
/// no `claude` starts with half a configuration; the daemon's post-send launch
/// check then marks the record errored.
/// Test: `run_launch_spec_rejects_a_missing_spec`,
/// `run_launch_spec_rejects_a_corrupt_spec`,
/// `run_launch_spec_reports_a_program_that_cannot_be_spawned`,
/// `run_launch_spec_marks_started_before_it_spawns`.
pub(crate) fn run_launch_spec(path: &std::path::Path) -> anyhow::Result<()> {
    // #9092: the root the daemon's session store and the launch records share.
    let root = dirs::home_dir()
        .context("managed launch aborted: the home directory cannot be resolved")?
        .join(".trusty-mpm");
    let started = std::time::Instant::now();
    let (status, code) = launch_from_spec(path, &root, |name| std::env::var_os(name))?;
    println!(
        "{}",
        trusty_mpm::runtime::launch_report::report_exit(code, started.elapsed())
    );
    std::process::exit(status);
}

/// [`run_launch_spec`] up to the child's exit, under the `~/.trusty-mpm`
/// `root` and the environment `env`.
///
/// Why: #9092 — a launch of the supervisor session must write the Architect
/// launch record for the `claude` it spawns, and a test must drive that
/// against a scratch root without `process::exit`.
/// What: consumes the spec and marks it started (#8233). #9092: when
/// [`binding_for`](trusty_mpm::session_manager::architect_spawn::binding_for)
/// names the supervisor session, it refuses under a Claude Code session
/// marker in `env`, runs the record-dir preflight, and records the spawned
/// PID before waiting; any of those failing is `Err`, with no `claude`
/// started or the spawned one killed. Returns `(exit status, raw code)` as
/// [`spawn_and_wait`] does.
/// Test: `a_supervisor_launch_records_the_claude_it_spawned_9092`,
/// `a_supervisor_launch_with_an_unwritable_record_dir_spawns_nothing_9092`,
/// `an_ordinary_launch_writes_no_launch_record_9092`,
/// `a_supervisor_launch_inside_claude_code_spawns_nothing_9092`.
pub(crate) fn launch_from_spec(
    path: &std::path::Path,
    root: &std::path::Path,
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> anyhow::Result<(i32, Option<i32>)> {
    let spec = trusty_mpm::runtime::launch_spec::LaunchSpec::consume(path)
        .with_context(|| "managed launch aborted: its launch spec could not be used")?;
    // #8233: the pane's shell parsed and ran the launch line — record that before
    // anything else can fail, so a stuck parser is distinguishable from a launch
    // that started and then broke.
    // #8233 review round 2: keyed on the LAUNCH, so a marker from an earlier
    // launch of this same session can never satisfy this one.
    trusty_mpm::runtime::launch_spec::LaunchSpec::mark_started(path, &spec.launch_id);
    // #9092: a supervisor launch records its claude or does not start it.
    let binding = trusty_mpm::session_manager::architect_spawn::binding_for(
        root,
        &spec.session_id,
        &spec.cwd,
    )
    .map_err(|why| {
        anyhow::anyhow!("managed launch aborted: cannot tell whether it is the Architect's: {why}")
    })?;
    if let Some(binding) = &binding {
        refuse_inside_claude_code(binding.session(), &env)?;
        binding.preflight().map_err(|why| {
            anyhow::anyhow!(
                "managed launch of supervisor session {} aborted before claude started: the \
                 Architect launch record cannot be written: {why}",
                binding.session()
            )
        })?;
    }
    let mut cmd = spec.to_command();
    spawn_and_wait(&mut cmd, &spec.program, |pid| match &binding {
        Some(binding) => binding.record(pid).map(drop),
        None => Ok(()),
    })
}

/// `Err` when `env` carries a Claude Code session marker (#9092).
///
/// Why: a supervisor launch writes an Architect launch record; a hooked
/// Claude session running this shim by hand must not mint one, as
/// `tm fleet relaunch` refuses there (#10033).
fn refuse_inside_claude_code(
    session: &str,
    env: &impl Fn(&str) -> Option<std::ffi::OsString>,
) -> anyhow::Result<()> {
    let present = super::fleet::inside_claude::present_markers(env);
    if present.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = present.iter().map(|(name, _)| *name).collect();
    anyhow::bail!(
        "managed launch of supervisor session {session} refused: it runs inside a Claude Code \
         session ({} set), and only an operator pane may launch the Architect",
        names.join(", ")
    )
}

/// Spawn `cmd` disclaimed, wait, and report its status two ways.
///
/// Why: both shim forms need the same SIGINT/SIGQUIT discipline and the same
/// disclaimed spawn; only what they do with the result differs.
/// What: returns `(exit_code_for_process_exit, raw_exit_code)` — the first is
/// 127 for a signal death (mirroring a shell), the second is `None` there so
/// [`trusty_mpm::runtime::launch_report::report_exit`] can tell the cases apart.
/// #9092: `then` runs on the child's PID before the wait; its `Err` kills the
/// child and fails the call.
/// Test: exercised by both `run` forms; the disclaim behaviour by
/// `trusty_mpm::core::spawn_disclaim`'s `disclaimed_status_*` tests.
fn spawn_and_wait(
    cmd: &mut std::process::Command,
    program: &str,
    then: impl FnOnce(u32) -> Result<(), String>,
) -> anyhow::Result<(i32, Option<i32>)> {
    // #2997 review: ignore SIGINT/SIGQUIT in the shim so a pane Ctrl-C (or
    // Ctrl-\) reaches only `claude` — which the disclaimed spawn resets to
    // SIG_DFL so it installs its own handlers (see
    // `spawn_disclaim::macos::status`) — and the shim SURVIVES to reap the
    // child and propagate its real exit status. Without this the shim and
    // `claude` share the foreground process group, so the signal would kill
    // the shim (default disposition) out from under `claude`, orphaning it.
    // The standard wrapper convention (à la `nohup`/`system(3)`). macOS-only
    // to match where the child-side SIG_DFL reset actually happens (the shim
    // is only ever emitted into a pane on macOS).
    #[cfg(target_os = "macos")]
    // SAFETY: setting SIG_IGN for two async-signal-safe signals before any
    // child exists; no handler state is shared.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
    }

    let status = trusty_mpm::core::spawn_disclaim::disclaimed_status_then(cmd, then)
        .with_context(|| format!("failed to spawn `{program}` (disclaimed)"))?;

    Ok((status.code().unwrap_or(127), status.code()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This launch's id — #8233 keys the sentinel on the LAUNCH, not the
    /// session, so a marker from an earlier launch cannot satisfy a later one.
    const TEST_LAUNCH_ID: &str = "aaaabbbbccccddddeeeeffff00001111";

    /// [`launch_from_spec`] under a scratch `~/.trusty-mpm` in `home` and an
    /// operator environment, so no test reads the real session store (#9092).
    fn launch_in(
        home: &std::path::Path,
        spec: &std::path::Path,
    ) -> anyhow::Result<(i32, Option<i32>)> {
        launch_from_spec(spec, &home.join(".trusty-mpm"), |_| None)
    }

    /// Write a spec naming `program` into `dir`, and return its path.
    ///
    /// Why: both cases below need a spec that decodes cleanly, so the only
    /// thing under test is what happens AFTER it is consumed.
    /// Test: `run_launch_spec_reports_a_program_that_cannot_be_spawned`,
    /// `run_launch_spec_marks_started_before_it_spawns`.
    fn spec_naming(dir: &std::path::Path, program: &str) -> std::path::PathBuf {
        let spec = trusty_mpm::runtime::launch_spec::LaunchSpec {
            session_id: "11111111-2222-3333-4444-555555555555".to_owned(),
            launch_id: TEST_LAUNCH_ID.to_owned(),
            cwd: dir.to_path_buf(),
            program: program.to_owned(),
            args: Vec::new(),
            env_unset: Vec::new(),
            env_set: Vec::new(),
        };
        spec.write_in(dir).expect("write the spec")
    }

    /// #8233 review (MEDIUM): the "program cannot be spawned" arm had no test.
    /// It is the arm a `claude` deleted between resolution and launch takes, and
    /// it must ERROR — `main` prints that into the pane and exits non-zero —
    /// rather than exiting 0 and leaving the daemon to believe a runtime started.
    #[test]
    fn run_launch_spec_reports_a_program_that_cannot_be_spawned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = spec_naming(
            dir.path(),
            &dir.path().join("no-such-claude").to_string_lossy(),
        );

        let err = launch_in(dir.path(), &path).expect_err("an unspawnable program must error");

        assert!(
            err.to_string().contains("failed to spawn"),
            "the pane must be told the program could not start: {err:#}"
        );
    }

    /// #8233 review: the sentinel answers "did the pane's SHELL run the line",
    /// which is true even when the launch then fails. Writing it only on the
    /// success path would make every real launch failure look like a stuck
    /// parser — so it must already be there when the spawn arm errors.
    #[test]
    fn run_launch_spec_marks_started_before_it_spawns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = spec_naming(
            dir.path(),
            &dir.path().join("no-such-claude").to_string_lossy(),
        );

        let _ = launch_in(dir.path(), &path);

        let marker = trusty_mpm::runtime::launch_spec::LaunchSpec::started_marker_in(
            dir.path(),
            TEST_LAUNCH_ID,
        );
        assert!(
            marker.exists(),
            "the sentinel must record that the shell reached the shim: {}",
            marker.display()
        );
    }

    #[test]
    fn run_rejects_empty_argv() {
        // No program token → a clear error rather than a panic or a spawn of "".
        let err = run(Vec::new()).expect_err("empty argv must error");
        assert!(
            err.to_string().contains("requires a program"),
            "unexpected error: {err}"
        );
    }

    /// #4431 critic HIGH fix: the exact, full subcommand name — the only form
    /// `disclaim_pane_command`'s own self-invocation ever emits — must keep
    /// passing the guard.
    #[test]
    fn invoked_literally_accepts_exact_name() {
        let argv = vec![
            "tm".to_string(),
            "internal-spawn-disclaimed".to_string(),
            "claude".to_string(),
            "-p".to_string(),
        ];
        assert!(invoked_literally(&argv));
    }

    /// #4431 critic HIGH fix: `infer_subcommands` (#4398) makes clap resolve
    /// any of these abbreviated prefixes to `Command::InternalSpawnDisclaimed`
    /// (hidden subcommands are inferred too) — the guard must reject every
    /// one of them so none can reach the disclaimed-spawn leaf.
    #[test]
    fn invoked_literally_rejects_abbreviated_prefixes() {
        for prefix in ["int", "inte", "internal"] {
            let argv = vec!["tm".to_string(), prefix.to_string(), "claude".to_string()];
            assert!(
                !invoked_literally(&argv),
                "abbreviated prefix {prefix:?} must not pass the literal-invocation guard"
            );
        }
    }

    /// A bare `tm` (no subcommand token at all) must not panic or vacuously
    /// pass — `argv.get(1)` is `None`, which never equals the literal name.
    #[test]
    fn invoked_literally_rejects_missing_token() {
        let argv = vec!["tm".to_string()];
        assert!(!invoked_literally(&argv));
    }

    /// #8233 fail-closed: a spec that is not there must produce an ERROR — the
    /// pane then shows it and nothing is launched. Silently launching a bare
    /// `claude` would be the truncated-command failure in a new disguise.
    #[test]
    fn run_launch_spec_rejects_a_missing_spec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = launch_in(dir.path(), &dir.path().join("absent.json"))
            .expect_err("a missing spec must abort the launch");
        assert!(
            format!("{err:#}").contains("managed launch aborted"),
            "the pane must be told what happened: {err:#}"
        );
    }

    /// #8233 fail-closed: a spec that is present but undecodable must abort too
    /// — a partially-applied environment is worse than no launch.
    #[test]
    fn run_launch_spec_rejects_a_corrupt_spec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, b"{\"session_id\":").expect("seed");
        let err = launch_in(dir.path(), &path).expect_err("a corrupt spec must abort the launch");
        assert!(
            format!("{err:#}").contains("managed launch aborted"),
            "the pane must be told what happened: {err:#}"
        );
    }
}

#[cfg(test)]
#[path = "spawn_disclaimed_architect_tests.rs"]
mod architect_tests;
