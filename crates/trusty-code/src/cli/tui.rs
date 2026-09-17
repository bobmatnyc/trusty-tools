//! `tcode tui` — launch the interactive TUI REPL against a running
//! `tcode serve` daemon (issue #4424; DOC-50 §4.1, AC-2.4).
//!
//! Why: every DOC-50 MVP slice landed — the shared `trusty-code-tui` framework
//! (event loop, generalized `ReplApp`, widgets) and
//! `trusty_code::tui_client::CodeEngine` (the `TuiEngine` impl) — but
//! nothing ever CONSTRUCTED a `CodeEngine` outside tests, so the REPL was
//! unreachable from a user's shell. This module is that missing integration
//! point and nothing more: it owns no REPL behaviour, no rendering, and no
//! daemon logic, matching `crate::cli`'s "translate CLI args into a call,
//! decisions belong elsewhere" contract.
//! What: [`run`] resolves the project the session homes in — since #8205
//! that defaults to the repository enclosing the launch directory, see
//! [`resolve_project`] — obtains the daemon
//! socket to drive from `super::daemon_autospawn`, and hands a `CodeEngine`
//! pointed at it to `trusty_code_tui::run::run` together with the shared
//! `ReplApp` model,
//! its reducer (`trusty_code_tui::app::apply`), and its renderer
//! (`trusty_code_tui::layout::draw`).
//!
//! `tcode tui` AUTO-SPAWNS its daemon (#4512, reversing DOC-50 §4.1's
//! deferral): the daemon answers on one derived socket path under the
//! trusty-code data directory (#6637), and nothing answering there starts
//! `tcode serve` instead of exiting with an actionable message.
//!
//! **Quitting the TUI never stops the daemon** — not even one this command
//! started. The daemon owns PM lifecycle, agent dispatch, and agent
//! communication, and a TUI is one attached client among possibly several,
//! so a client exit must not end live work (owner directive, 2026-08-01).
//! There is correspondingly NO teardown step here. A daemon bound to a
//! DIFFERENT project than this TUI is refused rather than attached to, since
//! every session would otherwise run against the wrong repository. See
//! `super::daemon_autospawn` for the whole policy — none of it lives here.
//!
//! Daemon resolution deliberately runs BEFORE `trusty_code_tui::run::run` enters
//! the alternate screen, so the startup spinner and any failure land on a
//! normal terminal instead of flashing behind a TUI that is about to tear
//! itself down.
//! Test: `tui_tests::*` covers the pure project-resolution helper;
//! `super::daemon_autospawn`'s tests cover every attach/spawn/binding-check
//! branch against real child processes;
//! `tests/cli_e2e.rs::{tui_subcommand_is_listed_in_help,
//! tui_auto_spawns_a_daemon_that_outlives_it,
//! tui_refuses_to_spawn_for_an_unreachable_explicit_daemon_url,
//! tui_refuses_a_daemon_bound_to_a_different_project}` cover the CLI surface
//! against the REAL binary. The launch path past daemon resolution needs a
//! real TTY (`trusty_code_tui::TerminalGuard::enter`), so it is verified by
//! running `tcode tui` by hand; the engine half is already covered
//! end-to-end by `tests/tui_client_engine.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use trusty_code::tui_client::CodeEngine;
use trusty_code_tui::ReplApp;

use super::daemon_autospawn;

/// Product label for the banner identity line and the `[tcode] ` status
/// prefix `ReplApp::new` derives from it.
const PRODUCT_LABEL: &str = "tcode";

/// Launch the TUI REPL, returning once the user quits — starting a daemon
/// first if none is running, and leaving it running on the way out.
///
/// Why/What/Test: see the module docs — this function IS the module.
/// `project` is `Option` because a projectless session is a first-class
/// state (`session.create` without a `project`), not a degraded one; it is
/// forwarded to a spawned daemon so the daemon's binding matches the TUI's,
/// and checked against a pre-existing daemon's reported binding.
///
/// (#8205) `projectless` is the opt-out from the cwd homing
/// [`resolve_project`] now performs by default.
/// (#8184) `delegate` is the `--delegate` opt-in back to the PM: `false` (the
/// default) mints the SOLO session `session.create` now defaults to, where the
/// agent reads and edits files itself through the permission prompt rather
/// than handing the work to a sub-agent.
pub async fn run(project: Option<PathBuf>, projectless: bool, delegate: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("resolve the current directory")?;
    // Canonicalized so the `$HOME` guard in `resolve_project` compares like
    // with like; an unresolvable home simply disables that one guard.
    let home = dirs::home_dir().and_then(|h| h.canonicalize().ok());
    let project = resolve_project(project, projectless, &cwd, home.as_deref())?;
    // #4512: attach to a running daemon serving this project, or start one —
    // replaces #4424's "exit and tell the user to start one". Nothing is
    // torn down afterwards: the daemon outlives every client (module docs).
    // #6637: the daemon is reached on its Unix socket, not a loopback port.
    let socket = daemon_autospawn::ensure_daemon(project.as_deref()).await?;
    // #8184: the interactive default is the solo agent; `--delegate` is the
    // one surface that asks for the delegating PM instead.
    let engine = if delegate {
        CodeEngine::with_socket_delegating(socket, project)
    } else {
        CodeEngine::with_socket(socket, project)
    };
    let mut app = ReplApp::new(PRODUCT_LABEL, user_label());
    // #8164: the banner's frame title must name the BINARY's version and SHA.
    // `ReplApp::new` defaults it to `trusty-code-tui`'s own crate version, so
    // a 0.7.0 tcode advertised itself as `tcode v0.2.0`.
    app.version = trusty_code::build_info::LONG_VERSION.to_string();

    trusty_code_tui::run::run(
        Arc::new(engine),
        app,
        trusty_code_tui::app::apply,
        trusty_code_tui::layout::draw,
    )
    .await
}

/// Decide which project a `tcode tui` session binds to.
///
/// Why (#8205): omitting `--project` used to mean "projectless", so a TUI
/// launched inside a repository ran against the executor's throwaway scratch
/// root — on 2026-09-16 an agent's `list_dir(".")` answered `(empty
/// directory)` inside a repository full of files and `search_code` found no
/// index, while the footer named the repo's workstream. DOC-75's MVP is
/// "sit in a repo, launch `tcode tui`", so the launch directory is now the
/// default binding and projectlessness is something you ask for.
///
/// This homing lives HERE, at the CLI layer, and not in
/// `ProjectBinding::resolve`: a launch directory is a choice the operator
/// made by typing the command there, whereas the daemon/protocol layer is
/// still forbidden from implicitly binding a CWD it was never told about
/// (see `ProjectBinding::agents_dir`).
///
/// What, in precedence order:
/// 1. `--project <path>` wins, canonicalized here so an unusable path fails
///    with the flag's name rather than as a confusing `-32003
///    invalid_argument` from `session.create` after the TUI has started.
/// 2. `--projectless` yields `None` — today's behaviour, opted into.
/// 3. Otherwise the session homes on `cwd`'s enclosing git toplevel
///    ([`trusty_common::find_git_root`]), or on `cwd` itself when no
///    repository encloses it. That walk is the SAME one trusty-search
///    derives an index id from, so the bound root and the index
///    `search_code` queries cannot disagree.
/// 4. `home` and the filesystem root are refused as a default home and stay
///    projectless: neither is a project, and binding one would ask
///    trusty-search to index an entire home directory. An explicit
///    `--project ~` still works — rule 1 outranks this.
///
/// Test: `tui_tests::resolve_project_defaults_to_the_enclosing_git_repo`,
/// `tui_tests::resolve_project_without_a_repo_binds_the_directory_itself`,
/// `tui_tests::resolve_project_projectless_opts_out_of_homing`,
/// `tui_tests::resolve_project_refuses_to_home_on_the_home_directory`,
/// `tui_tests::resolve_project_canonicalizes_a_real_directory`,
/// `tui_tests::resolve_project_rejects_a_missing_path`.
fn resolve_project(
    project: Option<PathBuf>,
    projectless: bool,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if let Some(p) = project {
        return p
            .canonicalize()
            .with_context(|| format!("invalid --project path '{}'", p.display()))
            .map(Some);
    }
    if projectless {
        return Ok(None);
    }
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("resolve the launch directory '{}'", cwd.display()))?;
    let root = trusty_common::find_git_root(&cwd).unwrap_or(cwd);
    if root.parent().is_none() || home.is_some_and(|h| h == root) {
        return Ok(None);
    }
    Ok(Some(root))
}

/// Name shown on the banner's `{user} · tcode` identity line — `$USER`, or a
/// generic fallback. Mirrors tagent's `repl::run` derivation so both REPLs
/// label the human the same way.
fn user_label() -> String {
    label_or_default(std::env::var("USER").ok())
}

/// The `$USER`-to-label rule, split out from [`user_label`] so it is
/// testable without mutating process-global environment state.
fn label_or_default(raw: Option<String>) -> String {
    raw.filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}

#[cfg(test)]
mod tui_tests {
    use super::*;

    /// A temp directory that IS a git repository (a bare `.git` directory is
    /// all `find_git_root` looks for), plus a nested subdirectory.
    fn repo_with_subdir() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonicalize");
        std::fs::create_dir(root.join(".git")).expect("create .git");
        let nested = root.join("crates").join("x");
        std::fs::create_dir_all(&nested).expect("create nested");
        (dir, root, nested)
    }

    /// #8205: launched anywhere inside a repository with no flags, the
    /// session binds that repository's TOPLEVEL — not the subdirectory the
    /// operator happened to stand in, and not the scratch root. This is the
    /// inverse of the pre-#8205 `resolve_project_none_stays_projectless`.
    #[test]
    fn resolve_project_defaults_to_the_enclosing_git_repo() {
        let (_guard, root, nested) = repo_with_subdir();
        let resolved = resolve_project(None, false, &nested, None)
            .expect("resolve")
            .expect("a repo must bind");
        assert_eq!(resolved, root);
    }

    /// Outside any repository the launch directory itself is the project —
    /// a directory with no `.git` is still a place to work and still
    /// indexable by its own basename.
    #[test]
    fn resolve_project_without_a_repo_binds_the_directory_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().canonicalize().expect("canonicalize");
        let resolved = resolve_project(None, false, &cwd, None)
            .expect("resolve")
            .expect("a plain directory must bind");
        assert_eq!(resolved, cwd);
    }

    /// The opt-out keeps the pre-#8205 behaviour — a projectless session is
    /// still first-class, it just has to be asked for.
    #[test]
    fn resolve_project_projectless_opts_out_of_homing() {
        let (_guard, _root, nested) = repo_with_subdir();
        assert!(
            resolve_project(None, true, &nested, None)
                .expect("resolve")
                .is_none()
        );
    }

    /// `$HOME` and the filesystem root are not projects: homing on either
    /// would hand trusty-search an entire home directory to index, so both
    /// stay projectless.
    #[test]
    fn resolve_project_refuses_to_home_on_the_home_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().canonicalize().expect("canonicalize");
        assert!(
            resolve_project(None, false, &home, Some(&home))
                .expect("resolve")
                .is_none(),
            "$HOME must not become the default project"
        );
        assert!(
            resolve_project(None, false, Path::new("/"), None)
                .expect("resolve")
                .is_none(),
            "the filesystem root must not become the default project"
        );
    }

    /// `--project` outranks both the homing and the opt-out, and is
    /// canonicalized to an absolute path.
    #[test]
    fn resolve_project_canonicalizes_a_real_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = std::env::temp_dir();
        let resolved = resolve_project(Some(dir.path().to_path_buf()), true, &cwd, None)
            .expect("resolve")
            .expect("some");
        assert!(resolved.is_absolute());
        assert_eq!(resolved, dir.path().canonicalize().expect("canonicalize"));
    }

    /// A path that does not exist is rejected with a message naming the
    /// flag and the offending path.
    #[test]
    fn resolve_project_rejects_a_missing_path() {
        let missing = std::env::temp_dir().join("tcode-tui-4424-does-not-exist");
        let cwd = std::env::temp_dir();
        let err =
            resolve_project(Some(missing.clone()), false, &cwd, None).expect_err("must reject");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("invalid --project path"), "{rendered}");
        assert!(
            rendered.contains(&missing.display().to_string()),
            "{rendered}"
        );
    }

    /// The banner identity label is never empty, even with `$USER` unset or
    /// blank (the banner would otherwise render a bare `· tcode`).
    #[test]
    fn label_or_default_falls_back_when_unset_or_blank() {
        assert_eq!(label_or_default(None), "user");
        assert_eq!(label_or_default(Some("   ".to_string())), "user");
        assert_eq!(label_or_default(Some("masa".to_string())), "masa");
    }
}
