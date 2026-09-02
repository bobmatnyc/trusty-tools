//! Bare `tm` outside a git work tree — list managed sessions, then offer the
//! two ways to start one (#6666).
//!
//! Why: bare `tm` in a plain directory used to run `git init` there and carry
//! on as though the operator had asked for a project (#6274/#6276). The owner
//! ruling of 2026-09-02 supersedes that OUTSIDE a work tree: show the managed
//! session list — the thing an operator in an arbitrary directory almost always
//! wants — and make the two ways to create a session explicit choices rather
//! than a side effect. Inside a work tree nothing changes; the auto-`git init`
//! still runs there for the directories that are already repositories.
//!
//! What: [`route_bare_tm`] is the pure decision (work tree vs not, TTY vs not);
//! [`parse_outside_git_choice`] turns one line of input into an
//! [`OutsideGitChoice`]; [`choice_argv`] turns that choice into the `tm
//! sessions …` argv the options block advertises, so the displayed command and
//! the executed one cannot drift; [`run_outside_git_menu`] is the orchestrator
//! with every I/O edge injected. [`dispatch_session_argv`] is the production
//! runner: it parses the argv back through the real `Cli` and calls the real
//! `sessions` handler — no process spawn.
//!
//! Test: `guided_outside_git_tests.rs`.

use std::future::Future;
use std::path::Path;

/// The `tm sessions` verb that creates an UNTRACKED session in place.
///
/// Why: `tm sessions start` runs the harness in the directory it is given and
/// registers nothing in the managed fleet — the "non-tracked session" half of
/// the ruling. Named here so the options block and [`choice_argv`] read the
/// same constant.
/// Test: `outside_git_untracked_choice_maps_to_sessions_start_argv`.
const UNTRACKED_VERB: &str = "start";

/// The `tm sessions` verb that creates a MANAGED, tracked session.
///
/// Why: `tm sessions new <path>` creates a managed-fleet record that `tm ls`
/// then lists — the "managed tracked session" half of the ruling. For a path
/// with no readable GitHub remote the daemon's in-project detection returns
/// `Ok(None)` and the spawn falls through to its local-path form, so a plain
/// directory is a valid target and nothing is initialised here.
/// Test: `outside_git_managed_choice_maps_to_sessions_new_argv`.
const MANAGED_VERB: &str = "new";

/// What bare `tm` should do for the current directory.
///
/// Why: the interception has to answer two questions at once — is this a
/// directory the existing guided default owns, and may we block on stdin? —
/// and both must be decidable without touching a terminal or a daemon.
/// What: `GuidedProject` means run the pre-#6666 flow unchanged.
/// `OutsideGit { interactive }` means show the listing and the options;
/// `interactive` is `true` only when a keypress can actually be read.
/// Test: `route_bare_tm_*`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BareTmRoute {
    /// Inside a git work tree, or a pane that belongs to a managed session —
    /// the existing guided default handles it.
    GuidedProject,
    /// Not inside a git work tree — list sessions and offer the two actions.
    OutsideGit {
        /// Read one line and run the choice; `false` prints and exits 0.
        interactive: bool,
    },
}

/// Which action the operator picked from the options block.
///
/// Test: `parse_outside_git_choice_*`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OutsideGitChoice {
    /// `[n]` — a session here with nothing registered anywhere.
    UntrackedSession,
    /// `[m]` — a managed-fleet session for this directory.
    ManagedSession,
    /// `[q]`, Enter, EOF, or anything unrecognised — do nothing.
    Quit,
}

/// Decide what bare `tm` does, given the four facts that settle it.
///
/// Why: a pane that belongs to a managed session can sit in any directory,
/// including one with no git work tree, and `fallback_protected`'s #4061 retry
/// exists to give that pane's Active→Stopped settle race a second bounded
/// attempt. Routing such a pane into this menu would print a session list at
/// an operator who asked to get their agent back, so the managed-pane signal
/// keeps the pre-#6666 flow.
/// What: `inside_work_tree` or `managed_pane` → `GuidedProject`. Otherwise
/// `OutsideGit`, interactive only when BOTH stdin and stdout are terminals —
/// the same anti-hang pairing `session_ls_connector::should_show_picker` and
/// `commands::projects::should_launch_bare_tui` use, because stdin redirected
/// from `/dev/null` would EOF instantly and a piped stdout must stay clean.
/// Test: `route_bare_tm_git_tree_is_guided`, `route_bare_tm_managed_pane_is_guided`,
/// `route_bare_tm_outside_git_tty_is_interactive`,
/// `route_bare_tm_outside_git_piped_stdin_is_static`,
/// `route_bare_tm_outside_git_piped_stdout_is_static`.
pub(crate) fn route_bare_tm(
    inside_work_tree: bool,
    managed_pane: bool,
    stdin_tty: bool,
    stdout_tty: bool,
) -> BareTmRoute {
    if inside_work_tree || managed_pane {
        return BareTmRoute::GuidedProject;
    }
    BareTmRoute::OutsideGit {
        interactive: stdin_tty && stdout_tty,
    }
}

/// Read one line of operator input as a choice.
///
/// What: case-insensitive, whitespace-trimmed; `n` selects the untracked
/// session, `m` the managed one, and everything else — `q`, a bare Enter, an
/// EOF-empty string, a typo — quits without touching anything.
/// Test: `parse_outside_git_choice_n_is_untracked`,
/// `parse_outside_git_choice_m_is_managed`,
/// `parse_outside_git_choice_empty_is_quit`,
/// `parse_outside_git_choice_unknown_is_quit`.
pub(crate) fn parse_outside_git_choice(input: &str) -> OutsideGitChoice {
    match input.trim().to_ascii_lowercase().as_str() {
        "n" => OutsideGitChoice::UntrackedSession,
        "m" => OutsideGitChoice::ManagedSession,
        _ => OutsideGitChoice::Quit,
    }
}

/// The `tm sessions …` argv a choice runs, without the leading `tm`.
///
/// Why: the options block advertises a command the operator could type; this
/// builds that same command as argv and [`dispatch_session_argv`] parses it
/// back through the real CLI. One source for both means the printed command
/// and the executed one cannot drift.
/// What: `Some(argv)` for the two session choices, `None` for `Quit`. The
/// managed form passes an empty `--task` because `tm sessions new` requires
/// the flag and an interactive session has no task to inject — the same empty
/// task `commands::session::start::start_session` builds for its own
/// protected-path dispatch.
/// Test: `outside_git_untracked_choice_maps_to_sessions_start_argv`,
/// `outside_git_managed_choice_maps_to_sessions_new_argv`,
/// `outside_git_quit_choice_maps_to_no_argv`.
pub(crate) fn choice_argv(choice: &OutsideGitChoice, cwd: &Path) -> Option<Vec<String>> {
    let path = cwd.to_string_lossy().into_owned();
    match choice {
        OutsideGitChoice::UntrackedSession => Some(vec![
            "sessions".to_string(),
            UNTRACKED_VERB.to_string(),
            "--dir".to_string(),
            path,
        ]),
        OutsideGitChoice::ManagedSession => Some(vec![
            "sessions".to_string(),
            MANAGED_VERB.to_string(),
            path,
            "--task".to_string(),
            String::new(),
        ]),
        OutsideGitChoice::Quit => None,
    }
}

/// The options block printed under the session listing.
///
/// Why: an operator standing outside a git project needs to know both that
/// nothing was created and what the two real commands are — so the block names
/// each command in full rather than describing it.
/// What: a multi-line string naming `[n]`, `[m]`, and `[q]` with the exact
/// commands [`choice_argv`] builds. Printed to stderr by
/// [`run_outside_git_menu`] so the listing on stdout stays pipeable.
/// Test: `outside_git_options_block_names_both_real_commands`,
/// `outside_git_options_block_matches_choice_argv`.
pub(crate) fn options_block(cwd: &Path) -> String {
    let path = cwd.to_string_lossy();
    format!(
        "\ntm: {path} is not inside a git work tree — nothing was created here.\n\
         tm:\n\
         tm:   [n] new untracked session here  →  tm sessions {UNTRACKED_VERB} --dir {path}\n\
         tm:   [m] new managed session here    →  tm sessions {MANAGED_VERB} {path} --task ''\n\
         tm:   [q] quit (default)"
    )
}

/// Read one line from stdin, treating EOF as an empty line.
///
/// Why: the menu's only blocking read. Kept separate from
/// [`run_outside_git_menu`] so every test drives the orchestrator with a
/// closure instead of a terminal.
/// Test: covered through `run_outside_git_menu`'s injected reader.
pub(crate) fn read_choice_line() -> std::io::Result<String> {
    use std::io::BufRead as _;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// List sessions, print the options, and run the operator's choice.
///
/// Why: every I/O edge is a parameter — the listing, the input read, and the
/// action — so the whole outside-git behaviour is testable with no daemon, no
/// terminal, and no spawned process.
/// What: runs `list_sessions` first (a failure is reported and the options are
/// still offered — a dead daemon is not a reason to hide the two commands),
/// prints [`options_block`], then returns `Ok(())` when not interactive. When
/// interactive it reads one line, maps it through [`parse_outside_git_choice`]
/// and [`choice_argv`], and awaits `run_argv` for the two action choices.
/// Test: `outside_git_menu_static_lists_then_prints_options`,
/// `outside_git_menu_static_never_reads_or_runs`,
/// `outside_git_menu_tty_choice_runs_the_advertised_argv`,
/// `outside_git_menu_quit_runs_nothing`,
/// `outside_git_menu_reports_a_listing_failure_and_still_offers_the_options`.
pub(crate) async fn run_outside_git_menu<L, LFut, R, RFut>(
    cwd: &Path,
    interactive: bool,
    list_sessions: L,
    read_choice: impl FnOnce() -> std::io::Result<String>,
    run_argv: R,
) -> anyhow::Result<()>
where
    L: FnOnce() -> LFut,
    LFut: Future<Output = anyhow::Result<()>>,
    R: FnOnce(Vec<String>) -> RFut,
    RFut: Future<Output = anyhow::Result<()>>,
{
    if let Err(e) = list_sessions().await {
        eprintln!("tm: could not list managed sessions ({e})");
    }
    eprintln!("{}", options_block(cwd));

    if !interactive {
        return Ok(());
    }

    eprint!("tm: choice [n/m/q]: ");
    let line = read_choice()?;
    match choice_argv(&parse_outside_git_choice(&line), cwd) {
        Some(argv) => run_argv(argv).await,
        None => Ok(()),
    }
}

/// The whole #6666 interception, wired to production I/O.
///
/// Why: `run_guided_default` reads as a sequence of "does this case own the
/// invocation?" gates (`try_inplace_relaunch` is the same shape), and keeping
/// the wiring here rather than inline there is also what holds `guided.rs`
/// under the 500-SLOC production cap.
/// What: evaluates [`route_bare_tm`] against the real classifier, the
/// managed-session environment, and both terminals. `None` means the caller
/// carries on unchanged. `Some(result)` is the finished menu — the listing
/// comes from `commands::managed::session_ls`, the one `tm sessions ls`
/// implementation, so there is no second renderer to drift from it.
/// Test: the decision by `route_bare_tm_*`, the menu by
/// `outside_git_menu_*`; the wiring itself by the binary smoke run in #6666.
pub(crate) async fn try_outside_git(
    client: &reqwest::Client,
    url: &str,
    cwd: &Path,
) -> Option<anyhow::Result<()>> {
    use std::io::IsTerminal as _;

    let BareTmRoute::OutsideGit { interactive } = route_bare_tm(
        !matches!(
            super::guided::classify_cwd_project(cwd),
            super::guided::CwdProject::NotGit
        ),
        super::guided_inplace::resolve_env_managed_session_id().is_some(),
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    ) else {
        return None;
    };

    Some(
        run_outside_git_menu(
            cwd,
            interactive,
            || {
                super::managed::session_ls(
                    client,
                    url,
                    false,
                    None,
                    false,
                    false,
                    super::session_picker::SessionSortArg::default(),
                    None,
                    false,
                )
            },
            read_choice_line,
            |argv| dispatch_session_argv(client, url, argv),
        )
        .await,
    )
}

/// Run a `tm sessions …` argv through the real CLI parser and handler.
///
/// Why: the options block promises the operator a command. Parsing that same
/// argv back through [`crate::cli::Cli`] and dispatching the resulting
/// `SessionAction` means the promise is kept by construction — a wrong flag
/// would fail to parse rather than silently run something else — and nothing
/// is spawned as a subprocess.
/// What: prepends the `tm` `argv[0]` clap expects, parses, and calls
/// [`crate::commands::session::session`]. Any other parsed command is an
/// internal bug and returns `Err`.
/// Test: `outside_git_dispatch_rejects_a_non_session_argv`; the happy paths are
/// covered by the argv tests plus the `sessions` handler's own suite.
pub(crate) async fn dispatch_session_argv(
    client: &reqwest::Client,
    url: &str,
    argv: Vec<String>,
) -> anyhow::Result<()> {
    use clap::Parser as _;

    let full = std::iter::once("tm".to_string()).chain(argv);
    let cli = crate::cli::Cli::try_parse_from(full)?;
    match cli.command {
        Some(crate::cli::Command::Sessions { action }) => {
            crate::commands::session::session(client, url, action).await
        }
        _ => anyhow::bail!("internal: the outside-git menu built a non-`sessions` command"),
    }
}

#[cfg(test)]
#[path = "guided_outside_git_tests.rs"]
mod tests;
