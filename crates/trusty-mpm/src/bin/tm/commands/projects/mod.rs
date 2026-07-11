//! `tm projects` command group (DOC-35 §3.1/§10.8, #2115/#2381).
//!
//! Why: the deterministic CLI half of the project control plane. The verb tree is
//! large (four registry verbs + two nested CRUD subtrees), so it is split per
//! subtree — `registry` (list/register/show/status), `deliverables`, and
//! `milestones` — each a sibling file well under the 500-SLOC cap, with this
//! `mod.rs` a thin dispatcher plus the shared clap-arg → domain-type conversions.
//! What: [`projects`] routes a [`ProjectsAction`] to the right subtree handler
//! (unchanged since #2115); [`launch_bare_tui`] is the interactive-TTY entry
//! point `main.rs` calls for a bare `tm projects` BEFORE clap parsing even
//! runs (#2118 — see `main.rs`'s module doc for why the interception happens
//! there rather than by relaxing `ProjectsAction` to `Option`). The `convert`
//! submodule maps the CLI value enums to `trusty_mpm::deliverable` domain types
//! so `cli.rs` stays free of a domain dependency.
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
/// no further arguments) BEFORE `Cli::try_parse()` runs, once stdout is
/// confirmed a TTY — see `main.rs`'s module doc. Doing the interception there
/// (rather than by making `ProjectsAction` an `Option` and branching inside
/// this module's dispatcher) means `tm projects <verb>` and a non-interactive
/// bare `tm projects` both flow through the UNMODIFIED clap definition, so
/// clap's own pre-#2118 "requires a subcommand" usage error (exit code 2) for
/// the latter is preserved byte-for-byte — reproducing that error manually
/// was tried and does not exactly match clap's internal formatting (it omits
/// the `[subcommands: …]` hint clap derives from richer internal match
/// context), so delegating to the untouched parse path is the more reliable
/// choice.
/// What: resolves the daemon URL exactly as the normal dispatch path would
/// for an unspecified `--url` (a bare invocation has no flags to read one
/// from) and hands off to `trusty_mpm::tui::project_ctl::run`.
/// Test: terminal glue, exercised by launching the TUI; the argv-shape half
/// of the interception condition is [`is_bare_projects_argv`], which IS unit
/// tested (the `stdout().is_terminal()` half is not — see its doc).
pub(crate) async fn launch_bare_tui() -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let url =
        trusty_mpm::core::resolve_daemon_url_via_gateway(&client, Some(crate::cli::DEFAULT_URL))
            .await;
    trusty_mpm::tui::project_ctl::run(url, PROJECT_CTL_INTERVAL_MS).await
}

/// True when `argv` is exactly a bare `tm projects` invocation (#2118).
///
/// Why: `main.rs` combines this with a real `stdout().is_terminal()` check
/// before intercepting ahead of `Cli::try_parse()`. Pulling the argv-shape
/// half out into its own pure function keeps it unit-testable without
/// spawning a subprocess or faking a TTY (the actual liveness check is a
/// one-line, untestable `std::io` call left in `main.rs`).
/// What: true only when `argv` has exactly two elements and the second is
/// literally `"projects"` — any flag (e.g. `--url ...`) or any verb makes
/// this false, so `tm projects list`, `tm --url … projects`, and every other
/// shape falls through to the unmodified clap parse path unaffected.
/// Test: `tests::is_bare_projects_argv_matches_only_the_exact_bare_form`.
pub(crate) fn is_bare_projects_argv(argv: &[String]) -> bool {
    argv.len() == 2 && argv[1] == "projects"
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
}
