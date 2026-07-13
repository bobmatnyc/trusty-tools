//! `tm projects` command group (DOC-35 §3.1/§6/§10.8, #2115/#2120/#2381).
//!
//! Why: the deterministic CLI half of the project control plane. The verb tree is
//! large (five registry verbs + two nested CRUD subtrees), so it is split per
//! subtree — `registry` (list/register/show/status/config), `deliverables`, and
//! `milestones` — each a sibling file well under the 500-SLOC cap, with this
//! `mod.rs` a thin dispatcher plus the shared clap-arg → domain-type conversions.
//! What: [`projects`] routes a [`ProjectsAction`] to the right subtree handler
//! (unchanged since #2115); [`launch_bare_tui`] is the interactive entry point
//! `main.rs` calls for a bare `tm projects` BEFORE clap parsing even runs
//! (#2118 — see `main.rs`'s module doc for why the interception happens there
//! rather than by relaxing `ProjectsAction` to `Option`), gated by
//! [`should_launch_bare_tui`] on BOTH stdin and stdout being real terminals
//! (not stdout alone — see that function's doc for the footgun this avoids).
//! The `convert` submodule maps the CLI value enums to `trusty_mpm::deliverable`
//! domain types so `cli.rs` stays free of a domain dependency.
//! Test: `cli_parses_projects_*` (parse) in `tests_projects.rs`; the per-subtree
//! rendering/serialization tests live in each submodule.

pub(crate) mod convert;
pub(crate) mod deliverables;
pub(crate) mod milestones;
pub(crate) mod registry;

use crate::cli::ProjectsAction;

/// Poll interval (ms) for the `tm projects` TUI launched from a bare, TTY
/// invocation (#2118).
///
/// Why: named separately from `Command::Tui`'s / `SessionAction::Tui`'s own
/// `--interval-ms` flags because a bare `tm projects` has no flag surface of
/// its own to read one from; 1500ms matches `tm sessions tui`'s default.
const PROJECT_CTL_INTERVAL_MS: u64 = 1500;

/// Launch the `tm projects` 4-pane TUI for a bare, interactive invocation (#2118).
///
/// Why: `main.rs` intercepts a bare `tm projects` (exactly `["tm", "projects"]`,
/// no further arguments, both stdin AND stdout confirmed real terminals — see
/// [`should_launch_bare_tui`]) BEFORE `Cli::try_parse()` runs — see `main.rs`'s
/// module doc. Doing the interception there (rather than by making
/// `ProjectsAction` an `Option` and branching inside this module's dispatcher)
/// means `tm projects <verb>` and a non-interactive bare `tm projects` both
/// flow through the UNMODIFIED clap definition, so clap's own pre-#2118
/// "requires a subcommand" usage error (exit code 2) for the latter is
/// preserved byte-for-byte — reproducing that error manually was tried and
/// does not exactly match clap's internal formatting (it omits the
/// `[subcommands: …]` hint clap derives from richer internal match context),
/// so delegating to the untouched parse path is the more reliable choice.
/// What: resolves the daemon URL exactly as the normal dispatch path would
/// for an unspecified `--url` — reading `TRUSTY_MPM_URL` directly via
/// [`trusty_mpm::core::explicit_url_from_env`] (a bare invocation runs BEFORE
/// `Cli::try_parse()`, so there is no `--url` flag to read and no clap-parsed
/// `Option<String>` field yet) — and hands off to
/// `trusty_mpm::tui::project_ctl::run`.
///
/// Why (#2487): this call site used to pass `Some(crate::cli::DEFAULT_URL)` —
/// the literal default URL string — relying on the resolvers' old
/// value-equality heuristic to treat it as "no real override" so the gateway
/// probe still ran. That heuristic is gone (every resolver now trusts `Some`
/// unconditionally), and the old call never actually read `TRUSTY_MPM_URL` at
/// all, so a bare `tm projects` TUI silently ignored the env var even when
/// the "normal" `tm projects <verb>` dispatch honoured it. Using the shared
/// env-read helper closes that gap and keeps this path in lockstep with
/// `main.rs`'s dispatch resolution.
/// Test: terminal glue, exercised by launching the TUI; the gate condition
/// itself is [`should_launch_bare_tui`], which IS unit tested.
pub(crate) async fn launch_bare_tui() -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let explicit = trusty_mpm::core::explicit_url_from_env();
    let url = trusty_mpm::core::resolve_daemon_url_via_gateway(&client, explicit.as_deref()).await;
    trusty_mpm::tui::project_ctl::run(url, PROJECT_CTL_INTERVAL_MS).await
}

/// True when `argv` is exactly a bare `tm projects` invocation (#2118).
///
/// Why: split out of [`should_launch_bare_tui`] so the argv-shape rule and the
/// stream-liveness rule are each independently testable.
/// What: true only when `argv` has exactly two elements and the second is
/// literally `"projects"` — any flag (e.g. `--url ...`) or any verb makes
/// this false, so `tm projects list`, `tm --url … projects`, and every other
/// shape falls through to the unmodified clap parse path unaffected.
/// Test: `tests::is_bare_projects_argv_matches_only_the_exact_bare_form`.
pub(crate) fn is_bare_projects_argv(argv: &[String]) -> bool {
    argv.len() == 2 && argv[1] == "projects"
}

/// True when a bare `tm projects` invocation should launch the interactive TUI.
///
/// Why: launching a full-screen, raw-mode TUI requires BOTH stdin and stdout
/// to be real terminals. The initial #2118 landing checked stdout alone,
/// which left a footgun: a supervisor/wrapper that redirects stdin from
/// `/dev/null` while leaving stdout attached to a pty (systemd, launchd, some
/// CI runners) would still see `stdout.is_terminal() == true` and launch a
/// raw-mode TUI against a dead stdin — with no keyboard path able to ever
/// send `q`/Ctrl-C, only an external signal can kill it. Mirrors
/// `session_picker::should_show_picker`'s established "require both streams"
/// convention in this same codebase. Taking `stdin_tty`/`stdout_tty` as plain
/// `bool` parameters (rather than calling `std::io::stdin()/stdout()` here)
/// keeps this pure and unit-testable without faking a real terminal; `main.rs`
/// supplies the real `is_terminal()` results at the one call site.
/// What: true only when [`is_bare_projects_argv`] AND `stdin_tty` AND
/// `stdout_tty` are all true.
/// Test: `tests::should_launch_bare_tui_requires_both_streams`.
pub(crate) fn should_launch_bare_tui(argv: &[String], stdin_tty: bool, stdout_tty: bool) -> bool {
    is_bare_projects_argv(argv) && stdin_tty && stdout_tty
}

/// Dispatch a `tm projects <action>` invocation to its subtree handler.
///
/// Why: `main.rs` stays a thin bootstrap; all `projects` routing lives here.
/// What: matches the top-level action and forwards to the registry verb handlers
/// or the deliverables/milestones subtree dispatchers, threading the CLI's shared
/// `(reqwest::Client, url)` pair through to the typed `DaemonClient` methods.
/// Test: exercised end-to-end by the daemon integration tests; parse coverage in
/// `tests_projects.rs`.
pub(crate) async fn projects(
    client: &reqwest::Client,
    url: &str,
    action: ProjectsAction,
) -> anyhow::Result<()> {
    match action {
        ProjectsAction::List { json, tag } => registry::list(client, url, json, tag).await,
        ProjectsAction::Register {
            name,
            repo_url,
            default_branch,
            description,
            tags,
            stack_hint,
            gh_user,
        } => {
            registry::register(
                client,
                url,
                registry::RegisterInput {
                    name,
                    repo_url,
                    default_branch,
                    description,
                    tags,
                    stack_hint,
                    gh_user,
                },
            )
            .await
        }
        ProjectsAction::Show { name, json } => registry::show(client, url, &name, json).await,
        ProjectsAction::Status { name, json } => registry::status(client, url, &name, json).await,
        ProjectsAction::Config { name, json, action } => {
            registry::config(client, url, &name, json, action).await
        }
        ProjectsAction::Deliverables { action } => {
            deliverables::dispatch(client, url, action).await
        }
        ProjectsAction::Milestones { action } => milestones::dispatch(client, url, action).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_bare_projects_argv_matches_only_the_exact_bare_form() {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_bare_projects_argv(&owned(&["tm", "projects"])));
        assert!(!is_bare_projects_argv(&owned(&["tm", "projects", "list"])));
        assert!(!is_bare_projects_argv(&owned(&[
            "tm", "--url", "http://x", "projects"
        ])));
        assert!(!is_bare_projects_argv(&owned(&["tm"])));
        assert!(!is_bare_projects_argv(&owned(&["tm", "sessions"])));
    }

    #[test]
    fn should_launch_bare_tui_requires_both_streams() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let bare = argv(&["tm", "projects"]);

        // Both streams are real terminals: launch.
        assert!(should_launch_bare_tui(&bare, true, true));

        // stdout is a tty but stdin is NOT (the reviewer-reported footgun: a
        // supervisor/wrapper redirects stdin from /dev/null while stdout stays
        // attached to a pty) — must NOT launch a raw-mode TUI with a dead stdin.
        assert!(!should_launch_bare_tui(&bare, false, true));

        // stdin is a tty but stdout is NOT (piped output) — must not launch.
        assert!(!should_launch_bare_tui(&bare, true, false));

        // Neither stream is a tty (fully non-interactive) — must not launch.
        assert!(!should_launch_bare_tui(&bare, false, false));

        // Even with both streams live, a non-bare invocation never launches.
        let verb = argv(&["tm", "projects", "list"]);
        assert!(!should_launch_bare_tui(&verb, true, true));
    }
}
