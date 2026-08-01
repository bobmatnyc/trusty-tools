//! `tcode tui` — launch the interactive TUI REPL against a running
//! `tcode serve --http` daemon (issue #4424; DOC-50 §4.1, AC-2.4).
//!
//! Why: every DOC-50 MVP slice landed — the shared `trusty-tui` framework
//! (event loop, generalized `ReplApp`, widgets) and
//! `trusty_code::tui_client::CodeEngine` (the `TuiEngine` impl) — but
//! nothing ever CONSTRUCTED a `CodeEngine` outside tests, so the REPL was
//! unreachable from a user's shell. This module is that missing integration
//! point and nothing more: it owns no REPL behaviour, no rendering, and no
//! daemon logic, matching `crate::cli`'s "translate CLI args into a call,
//! decisions belong elsewhere" contract.
//! What: [`run`] resolves the optional project path, obtains a daemon URL to
//! drive from `super::daemon_autospawn`, and hands a `CodeEngine` pointed at
//! it to `trusty_tui::run::run` together with the shared `ReplApp` model,
//! its reducer (`trusty_tui::app::apply`), and its renderer
//! (`trusty_tui::layout::draw`).
//!
//! `tcode tui` AUTO-SPAWNS its daemon (#4512, reversing DOC-50 §4.1's
//! deferral): discovery is unchanged (`TCODE_DAEMON_URL` -> the `http_addr`
//! discovery file -> a `GET /health` liveness ping), but a missing or stale
//! discovery file now starts `tcode serve --http` instead of exiting with an
//! actionable message.
//!
//! **Quitting the TUI never stops the daemon** — not even one this command
//! started. The daemon owns PM lifecycle, agent dispatch, and agent
//! communication, and a TUI is one attached client among possibly several,
//! so a client exit must not end live work (owner directive, 2026-08-01).
//! There is correspondingly NO teardown step here. An
//! explicitly-set-but-unreachable `TCODE_DAEMON_URL` still errors rather
//! than spawning, since starting a daemon at a different address would
//! ignore that instruction, and a daemon bound to a DIFFERENT project than
//! this TUI is refused rather than attached to. See
//! `super::daemon_autospawn` for the whole policy — none of it lives here.
//!
//! Daemon resolution deliberately runs BEFORE `trusty_tui::run::run` enters
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
//! real TTY (`trusty_tui::TerminalGuard::enter`), so it is verified by
//! running `tcode tui` by hand; the engine half is already covered
//! end-to-end by `tests/tui_client_engine.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use trusty_code::tui_client::CodeEngine;
use trusty_tui::ReplApp;

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
pub async fn run(project: Option<PathBuf>) -> Result<()> {
    let project = resolve_project(project)?;
    let http = trusty_code::tui_client::build_http_client();
    // #4512: attach to a running daemon serving this project, or start one —
    // replaces #4424's "exit and tell the user to start one". Nothing is
    // torn down afterwards: the daemon outlives every client (module docs).
    let daemon_url = daemon_autospawn::ensure_daemon(&http, project.as_deref()).await?;
    let engine = CodeEngine::with_daemon_url(http, daemon_url, project);
    let app = ReplApp::new(PRODUCT_LABEL, user_label());

    trusty_tui::run::run(
        Arc::new(engine),
        app,
        trusty_tui::app::apply,
        trusty_tui::layout::draw,
    )
    .await
}

/// Canonicalize `--project` when given, so the daemon receives an absolute
/// path and an unusable one fails HERE (with the flag's name in the
/// message) rather than as a confusing `-32003 invalid_argument` from
/// `session.create` after the TUI has already started.
fn resolve_project(project: Option<PathBuf>) -> Result<Option<PathBuf>> {
    project
        .map(|p| {
            p.canonicalize()
                .with_context(|| format!("invalid --project path '{}'", p.display()))
        })
        .transpose()
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

    /// Omitting `--project` stays projectless — never silently defaults to
    /// the current directory (which would bind a session to whatever
    /// directory the operator happened to launch from).
    #[test]
    fn resolve_project_none_stays_projectless() {
        assert!(resolve_project(None).expect("resolve").is_none());
    }

    /// A real directory is canonicalized to an absolute path.
    #[test]
    fn resolve_project_canonicalizes_a_real_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resolved = resolve_project(Some(dir.path().to_path_buf()))
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
        let err = resolve_project(Some(missing.clone())).expect_err("must reject");
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
