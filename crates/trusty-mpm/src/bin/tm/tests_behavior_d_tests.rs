//! CLI parse tests for `tm session new`/`ls`/`activity`/`send`/`answer`/
//! `attach`/lifecycle-alias/`prune`/`catchup` — the session-manager MVP verb
//! surface (extracted from `tests.rs`, issue #610 SLOC cap / #1916 line-cap
//! rebalance after the `sessions`→`session` rename removed an incidental
//! `/*`-shaped comment substring that had been masking this file's true SLOC
//! count from `scripts/check_line_cap.sh`).
//!
//! Why: `tests.rs` grew past the 1500-SLOC test-file cap once the #1916
//! rename fix exposed its true size; this is a natural, self-contained slice
//! (every managed session-manager CLI-parse test) to split out, following the
//! existing `tests_behavior_a/b/c` convention.
//! What: parse round-trips for `SessionAction::New`/`Ls`/`Activity`/`Send`/
//! `Answer`/`Attach`/the deprecated lifecycle aliases/`Prune*`/`Catchup`.
//! Test: `cargo test -p trusty-mpm` runs this file as part of the `tm` binary
//! test suite.

use clap::Parser;

use crate::cli::{Cli, Command, SessionAction};

// ── Session-manager MVP CLI parse tests ─────────────────────────────────────

#[test]
fn cli_parses_session_new() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "new",
        "https://github.com/owner/repo",
        "--git-ref",
        "feat/x",
        "--task",
        "do the thing",
        "--name-hint",
        "ticket-1",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::New {
                    repo,
                    git_ref,
                    task,
                    name_hint,
                    runtime,
                    no_inject,
                    deliverable,
                },
        } => {
            assert_eq!(repo, "https://github.com/owner/repo");
            assert_eq!(git_ref, "feat/x");
            assert_eq!(task, "do the thing");
            assert_eq!(name_hint.as_deref(), Some("ticket-1"));
            // No --runtime flag → default claude-code (now a typed RuntimeKind).
            assert_eq!(runtime, trusty_mpm::runtime::RuntimeKind::ClaudeCode);
            // No --no-inject → turnkey injection is the default (#1903/#1299).
            assert!(!no_inject);
            // No --deliverable → no link (#2379).
            assert_eq!(deliverable, None);
        }
        other => panic!("expected session new, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_new_with_deliverable() {
    // Why: #2379 — `--deliverable <id>` binds the new session to a Deliverable.
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "new",
        "https://github.com/owner/repo",
        "--task",
        "do the thing",
        "--deliverable",
        "11111111-1111-1111-1111-111111111111",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::New { deliverable, .. },
        } => {
            assert_eq!(
                deliverable.as_deref(),
                Some("11111111-1111-1111-1111-111111111111")
            );
        }
        other => panic!("expected session new, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_new_no_inject() {
    // Why: #1903/#1299 — `--no-inject` selects the legacy metadata-only spawn.
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "new",
        "https://github.com/owner/repo",
        "--task",
        "do the thing",
        "--no-inject",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::New { no_inject, .. },
        } => {
            assert!(no_inject, "--no-inject must set the flag");
        }
        other => panic!("expected session new, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_new_with_tcode_runtime() {
    // Why: #1203 — operators must be able to select the tcode backend via
    // `--runtime tcode`; this guards the flag wiring on the New subcommand.
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "new",
        "https://github.com/owner/repo",
        "--task",
        "do the thing",
        "--runtime",
        "tcode",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::New { runtime, .. },
        } => {
            assert_eq!(runtime, trusty_mpm::runtime::RuntimeKind::Tcode);
        }
        other => panic!("expected session new, got {other:?}"),
    }
}

#[test]
fn cli_rejects_unknown_runtime_at_parse_time() {
    // Why: #1213 — `--runtime` is now a clap `ValueEnum`, so an unsupported
    // backend must fail during argument parsing (with a "possible values" hint)
    // rather than being forwarded to the daemon and rejected at the HTTP layer.
    let err = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "new",
        "https://github.com/owner/repo",
        "--task",
        "do the thing",
        "--runtime",
        "gpt",
    ])
    .expect_err("unknown runtime must be rejected at parse time");
    let msg = err.to_string();
    assert!(
        msg.contains("claude-code") && msg.contains("tcode"),
        "error must list the supported runtimes: {msg}"
    );
}

#[test]
fn cli_parses_session_ls() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "ls", "--json"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Ls { json, .. },
        } => assert!(json),
        other => panic!("expected session ls, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_ls_source_id() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "ls", "--source-id", "myorg/myrepo"])
        .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::Ls {
                    source_id,
                    current,
                    json,
                    all,
                    ..
                },
        } => {
            assert_eq!(source_id, Some("myorg/myrepo".to_string()));
            assert!(!current);
            assert!(!json);
            assert!(!all, "--all must default to false");
        }
        other => panic!("expected session ls, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_ls_current() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "ls", "--current"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::Ls {
                    current,
                    source_id,
                    json,
                    all,
                    ..
                },
        } => {
            assert!(current);
            assert!(source_id.is_none());
            assert!(!json);
            assert!(!all, "--all must default to false");
        }
        other => panic!("expected session ls, got {other:?}"),
    }
}

#[test]
fn cli_session_ls_source_id_and_current_conflict() {
    // --current conflicts_with --source-id: clap must reject both together.
    let result = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "ls",
        "--current",
        "--source-id",
        "foo/bar",
    ]);
    assert!(
        result.is_err(),
        "passing both --current and --source-id must be a parse error"
    );
}

#[test]
fn cli_parses_session_ls_all() {
    // Why (#1809): `--all` must opt in to showing decommissioned tombstones.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "ls", "--all"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Ls { all, json, .. },
        } => {
            assert!(all, "--all flag must parse to true");
            assert!(!json);
        }
        other => panic!("expected session ls --all, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_activity() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "activity", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Activity { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session activity, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_send() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "send", "abc", "hello"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Send { id, text },
        } => {
            assert_eq!(id, "abc");
            assert_eq!(text, "hello");
        }
        other => panic!("expected session send, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_answer() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "answer", "abc", "rebase"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Answer { id, answer },
        } => {
            assert_eq!(id, "abc");
            assert_eq!(answer, "rebase");
        }
        other => panic!("expected session answer, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_attach() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "attach", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Attach { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session attach, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_managed_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "managed-stop", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::ManagedStop { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session managed-stop, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_runtime_stop() {
    // The deprecated `runtime-stop` verb still parses (alias of `stop`, #1205).
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "runtime-stop", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::RuntimeStop { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session runtime-stop, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_managed_resume() {
    // The deprecated `managed-resume` verb still parses (alias of `resume`, #1205).
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "managed-resume", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::ManagedResume { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session managed-resume, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_stop_verb() {
    // The canonical `stop` verb parses to the local-session Stop action (#1205);
    // it shares the clean name the deprecated managed verbs now point at.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "stop", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Stop { id_or_name },
        } => assert_eq!(id_or_name, "abc"),
        other => panic!("expected session stop, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_resume_verb() {
    // The canonical `resume` verb parses to the Resume action (#1205).
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "resume", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Resume { id_or_name },
        } => assert_eq!(id_or_name, "abc"),
        other => panic!("expected session resume, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_decommission() {
    // `decommission` remains the terminal teardown verb (#1205).
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "decommission", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Decommission { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session decommission, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_delete() {
    // #2012: `delete` hard-deletes the record; defaults to force=false.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "delete", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Delete { id, force },
        } => {
            assert_eq!(id, "abc");
            assert!(!force, "default must be the fail-closed force=false");
        }
        other => panic!("expected session delete, got {other:?}"),
    }
    // With --force, bypasses the running-session guard.
    let cli2 = Cli::try_parse_from(["trusty-mpm", "session", "delete", "abc", "--force"]).unwrap();
    match cli2.command.unwrap() {
        Command::Session {
            action: SessionAction::Delete { id, force },
        } => {
            assert_eq!(id, "abc");
            assert!(force, "--force must set force=true");
        }
        other => panic!("expected session delete, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessions_rename_two_args() {
    // From the list: `rename <id-or-name> <new-name>` — arg1 is the target,
    // arg2 the new name.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "rename", "tm-old-01", "tm-new-01"])
        .unwrap();
    match cli.command.unwrap() {
        Command::Sessions {
            action: SessionAction::Rename { arg1, arg2 },
        } => {
            assert_eq!(arg1, "tm-old-01");
            assert_eq!(arg2.as_deref(), Some("tm-new-01"));
        }
        other => panic!("expected sessions rename, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessions_rename_in_session() {
    // In-session: `rename <new-name>` — arg1 is the new name, arg2 is None
    // (target resolved from $TM_MANAGED_SESSION_ID at runtime).
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "rename", "tm-new-name"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessions {
            action: SessionAction::Rename { arg1, arg2 },
        } => {
            assert_eq!(arg1, "tm-new-name");
            assert_eq!(arg2, None);
        }
        other => panic!("expected sessions rename, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_prune_idle() {
    // `prune-idle` (#1313) accepts both flags; defaults are false.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "prune-idle", "--dry-run", "--json"])
        .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::PruneIdle { dry_run, json },
        } => {
            assert!(dry_run);
            assert!(json);
        }
        other => panic!("expected session prune-idle, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_prune_idle_defaults() {
    // With no flags, prune-idle defaults to a live (non-dry-run), text run.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "prune-idle"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::PruneIdle { dry_run, json },
        } => {
            assert!(!dry_run);
            assert!(!json);
        }
        other => panic!("expected session prune-idle, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_decommission_ephemeral() {
    // #1508: the bulk-teardown verb takes no arguments.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "decommission-ephemeral"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::DecommissionEphemeral,
        } => {}
        other => panic!("expected session decommission-ephemeral, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_prune() {
    // #1508: `prune` requires `--state` and accepts `--dry-run`/`--include-active`.
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "prune",
        "--state",
        "stopped",
        "--dry-run",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::Prune {
                    state,
                    dry_run,
                    include_active,
                },
        } => {
            assert_eq!(state, "stopped");
            assert!(dry_run);
            assert!(
                !include_active,
                "include_active defaults to the fail-closed false"
            );
        }
        other => panic!("expected session prune, got {other:?}"),
    }
}

#[test]
fn cli_session_prune_requires_state() {
    // #1508: `--state` is mandatory — omitting it must be a parse error (the
    // fail-closed CLI contract; we never default to a destructive filter).
    let err = Cli::try_parse_from(["trusty-mpm", "session", "prune"]);
    assert!(err.is_err(), "prune without --state must fail to parse");
}

#[test]
fn cli_parses_session_prune_worktrees() {
    // #1840: `prune-worktrees` with no flags defaults to dry-run (force=false).
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "prune-worktrees"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::PruneWorktrees { force },
        } => {
            assert!(!force, "default must be dry-run (force=false)");
        }
        other => panic!("expected session prune-worktrees, got {other:?}"),
    }
    // With --force, actually deletes.
    let cli2 =
        Cli::try_parse_from(["trusty-mpm", "session", "prune-worktrees", "--force"]).unwrap();
    match cli2.command.unwrap() {
        Command::Session {
            action: SessionAction::PruneWorktrees { force },
        } => {
            assert!(force, "--force must set force=true");
        }
        other => panic!("expected session prune-worktrees, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessions_sync_assets() {
    // #2444: `sync-assets <id>` re-syncs ONE session.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "sync-assets", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessions {
            action: SessionAction::SyncAssets { id, all },
        } => {
            assert_eq!(id.as_deref(), Some("abc"));
            assert!(!all);
        }
        other => panic!("expected sessions sync-assets, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessions_sync_assets_all() {
    // #2444: `sync-assets --all` re-syncs every syncable session.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "sync-assets", "--all"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessions {
            action: SessionAction::SyncAssets { id, all },
        } => {
            assert_eq!(id, None);
            assert!(all);
        }
        other => panic!("expected sessions sync-assets --all, got {other:?}"),
    }
}

#[test]
fn cli_sessions_sync_assets_requires_id_or_all() {
    // Neither an id nor --all: must be a parse error (required_unless_present).
    let err = Cli::try_parse_from(["trusty-mpm", "sessions", "sync-assets"]);
    assert!(
        err.is_err(),
        "sync-assets with neither an id nor --all must fail to parse"
    );
}

#[test]
fn cli_sessions_sync_assets_id_and_all_conflict() {
    // Both an id and --all: must be a parse error (conflicts_with).
    let err = Cli::try_parse_from(["trusty-mpm", "sessions", "sync-assets", "abc", "--all"]);
    assert!(
        err.is_err(),
        "sync-assets with both an id and --all must fail to parse"
    );
}

#[test]
fn cli_parses_session_catchup() {
    // DOC-28 PR1 (#1762): `tm session catchup --all-projects --full` must parse
    // cleanly and deliver the expected field values.
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "catchup",
        "--all-projects",
        "--full",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Catchup { all_projects, full },
        } => {
            assert!(all_projects, "--all-projects should be true");
            assert!(full, "--full should be true");
        }
        other => panic!("expected session catchup, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_catchup_defaults() {
    // DOC-28 PR1 (#1762): bare `tm session catchup` defaults to false for both flags.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "catchup"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Catchup { all_projects, full },
        } => {
            assert!(!all_projects, "--all-projects should default to false");
            assert!(!full, "--full should default to false");
        }
        other => panic!("expected session catchup, got {other:?}"),
    }
}

// ── Top-level `tm ls` connector CLI parse tests (#2311) ─────────────────────

/// Bare `tm ls` parses as the session connector (no `--projects`, all defaults).
///
/// Why: the top-level `tm ls` is now the interactive managed-session connector;
/// this asserts the default field values that route it to the session path.
/// What: parses `tm ls` and asserts every flag defaults false / `None`.
/// Test: this test.
#[test]
fn cli_parses_ls_connector_bare() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            terms,
            projects,
            json,
            source_id,
            current,
            all,
            root,
        } => {
            assert!(
                terms.is_empty(),
                "bare `tm ls` must have no positional terms"
            );
            assert!(!projects, "bare `tm ls` must not set --projects");
            assert!(!json);
            assert!(source_id.is_none());
            assert!(!current);
            assert!(!all);
            assert!(root.is_none());
        }
        other => panic!("expected top-level ls, got {other:?}"),
    }
}

/// `tm ls --projects` routes to the legacy alias/project registry list.
#[test]
fn cli_parses_ls_projects() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--projects"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { projects, .. } => assert!(projects, "--projects must parse to true"),
        other => panic!("expected top-level ls --projects, got {other:?}"),
    }
}

/// `tm ls -p` is the short alias for `--projects`.
#[test]
fn cli_parses_ls_projects_short() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "-p"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { projects, .. } => assert!(projects, "-p must parse to true"),
        other => panic!("expected top-level ls -p, got {other:?}"),
    }
}

/// `tm ls --json` selects JSON output while staying in session (connector) mode.
#[test]
fn cli_parses_ls_json() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--json"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { json, projects, .. } => {
            assert!(json, "--json must parse to true");
            assert!(!projects, "--json alone must not imply --projects");
        }
        other => panic!("expected top-level ls --json, got {other:?}"),
    }
}

/// `tm ls --current` derives the source_id scope from the cwd (session mode).
#[test]
fn cli_parses_ls_current() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--current"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            current, source_id, ..
        } => {
            assert!(current, "--current must parse to true");
            assert!(source_id.is_none());
        }
        other => panic!("expected top-level ls --current, got {other:?}"),
    }
}

/// `tm ls --source-id <slug>` sets the explicit fleet scope filter.
#[test]
fn cli_parses_ls_source_id() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--source-id", "owner/repo"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            source_id, current, ..
        } => {
            assert_eq!(source_id, Some("owner/repo".to_string()));
            assert!(!current);
        }
        other => panic!("expected top-level ls --source-id, got {other:?}"),
    }
}

/// `tm ls --current --source-id <slug>` is a parse error (mutually exclusive).
#[test]
fn cli_ls_source_id_and_current_conflict() {
    let result =
        Cli::try_parse_from(["trusty-mpm", "ls", "--current", "--source-id", "owner/repo"]);
    assert!(
        result.is_err(),
        "passing both --current and --source-id to `tm ls` must be a parse error"
    );
}

// ── `tm ls` picker/static gate (#2311) ──────────────────────────────────────

use crate::commands::session_picker::should_show_picker;

/// The picker opens only on a fully-interactive terminal with ≥1 session.
#[test]
fn ls_connector_should_show_picker_interactive_with_sessions() {
    assert!(should_show_picker(true, true, false, false, 1));
    assert!(should_show_picker(true, true, false, false, 5));
}

/// A non-TTY stdin OR stdout forces the static (pipeable) list path — never a
/// blocking picker. Mirrors guided.rs's non-TTY gate.
#[test]
fn ls_connector_should_show_picker_non_tty_static() {
    assert!(
        !should_show_picker(false, true, false, false, 3),
        "piped stdin -> static"
    );
    assert!(
        !should_show_picker(true, false, false, false, 3),
        "piped stdout -> static"
    );
    assert!(!should_show_picker(false, false, false, false, 3));
}

/// `--json` and `--all` force static output even on a TTY, and 0 sessions never
/// opens an empty picker.
#[test]
fn ls_connector_should_show_picker_flags_and_empty_static() {
    assert!(
        !should_show_picker(true, true, true, false, 3),
        "--json -> static"
    );
    assert!(
        !should_show_picker(true, true, false, true, 3),
        "--all -> static"
    );
    assert!(
        !should_show_picker(true, true, false, false, 0),
        "0 sessions -> static"
    );
}

// ── `tm ls` inline sort/filter grammar (#3483, PM correction) ───────────
//
// The repo owner asked for `--sort`/`--filter` to be expressed as bare
// positional words instead of flags: `tm ls [recent|alpha] [filter-word...]`.
// `parse_ls_terms` is the pure grammar seam; `filter_sessions_by_term` and
// `sort_sessions` are the pure list operations it feeds.

use crate::commands::session_picker::{
    SessionSortArg, filter_sessions_by_term, parse_ls_terms, sort_sessions,
};

/// Bare `tm ls` (no positional words) → default sort, no filter.
#[test]
fn parse_ls_terms_empty_defaults_recent_no_filter() {
    assert_eq!(parse_ls_terms(&[]), (SessionSortArg::Recent, None));
}

/// A lone `recent` keyword selects `Recent` with no filter.
#[test]
fn parse_ls_terms_recent_keyword_only() {
    let terms = vec!["recent".to_string()];
    assert_eq!(parse_ls_terms(&terms), (SessionSortArg::Recent, None));
}

/// A lone `alpha` keyword selects `Alpha` with no filter.
#[test]
fn parse_ls_terms_alpha_keyword_only() {
    let terms = vec!["alpha".to_string()];
    assert_eq!(parse_ls_terms(&terms), (SessionSortArg::Alpha, None));
}

/// The sort keyword is matched case-insensitively (`ALPHA`, `Recent`, …).
#[test]
fn parse_ls_terms_keyword_case_insensitive() {
    let terms = vec!["ALPHA".to_string()];
    assert_eq!(parse_ls_terms(&terms), (SessionSortArg::Alpha, None));
    let terms = vec!["Recent".to_string()];
    assert_eq!(parse_ls_terms(&terms), (SessionSortArg::Recent, None));
}

/// `recent <word>` consumes the keyword and treats the rest as the filter.
#[test]
fn parse_ls_terms_recent_with_filter() {
    let terms = vec!["recent".to_string(), "api".to_string()];
    assert_eq!(
        parse_ls_terms(&terms),
        (SessionSortArg::Recent, Some("api".to_string()))
    );
}

/// `alpha <word>` consumes the keyword and treats the rest as the filter.
#[test]
fn parse_ls_terms_alpha_with_filter() {
    let terms = vec!["alpha".to_string(), "api".to_string()];
    assert_eq!(
        parse_ls_terms(&terms),
        (SessionSortArg::Alpha, Some("api".to_string()))
    );
}

/// A first word that is NOT `recent`/`alpha` is treated as the filter, with
/// sort defaulting to `Recent` — `tm ls foo` behaves like `tm ls recent foo`.
#[test]
fn parse_ls_terms_non_keyword_first_word_is_filter() {
    let terms = vec!["foo".to_string()];
    assert_eq!(
        parse_ls_terms(&terms),
        (SessionSortArg::Recent, Some("foo".to_string()))
    );
}

/// Multiple non-keyword words all become the filter, joined by a space.
#[test]
fn parse_ls_terms_multi_word_filter_without_keyword_joins_with_space() {
    let terms = vec!["foo".to_string(), "bar".to_string()];
    assert_eq!(
        parse_ls_terms(&terms),
        (SessionSortArg::Recent, Some("foo bar".to_string()))
    );
}

/// A filter term that happens to equal the OTHER sort keyword is reachable by
/// prefixing the keyword actually wanted: `tm ls recent alpha` sorts by
/// recency and filters for the literal substring "alpha".
#[test]
fn parse_ls_terms_keyword_then_filter_equal_to_other_keyword() {
    let terms = vec!["recent".to_string(), "alpha".to_string()];
    assert_eq!(
        parse_ls_terms(&terms),
        (SessionSortArg::Recent, Some("alpha".to_string()))
    );
    let terms = vec!["alpha".to_string(), "recent".to_string()];
    assert_eq!(
        parse_ls_terms(&terms),
        (SessionSortArg::Alpha, Some("recent".to_string()))
    );
}

/// Minimal `ManagedSessionSummary` builder for the sort/filter tests below.
fn ls_test_session(
    name: &str,
    state: &str,
    last_activity_at: Option<&str>,
    created_at: Option<&str>,
    source_id: Option<&str>,
    task: Option<&str>,
) -> trusty_mpm::client::ManagedSessionSummary {
    trusty_mpm::client::ManagedSessionSummary {
        id: format!("{name}-id"),
        name: name.to_string(),
        state: state.to_string(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: created_at.map(str::to_owned),
        last_activity_at: last_activity_at.map(str::to_owned),
        pending_decision: None,
        proposed_default: None,
        source_id: source_id.map(str::to_owned),
        task: task.map(str::to_owned),
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        attached: false,
        slot: 0,
        deleted: false,
    }
}

/// The filter matches a substring of the `name` column, case-insensitively.
#[test]
fn filter_sessions_by_term_matches_name() {
    let sessions = vec![
        ls_test_session("api-worker", "active", None, None, None, None),
        ls_test_session("web-frontend", "active", None, None, None, None),
    ];
    let out = filter_sessions_by_term(sessions, Some("API"));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "api-worker");
}

/// The filter matches a substring of the `task` column.
#[test]
fn filter_sessions_by_term_matches_task() {
    let sessions = vec![
        ls_test_session("s1", "active", None, None, None, Some("fix the login bug")),
        ls_test_session("s2", "active", None, None, None, Some("write docs")),
    ];
    let out = filter_sessions_by_term(sessions, Some("login"));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "s1");
}

/// The filter matches a substring of the `source_id` (project) column.
#[test]
fn filter_sessions_by_term_matches_source_id() {
    let sessions = vec![
        ls_test_session(
            "s1",
            "active",
            None,
            None,
            Some("bobmatnyc/trusty-tools"),
            None,
        ),
        ls_test_session("s2", "active", None, None, Some("other/repo"), None),
    ];
    let out = filter_sessions_by_term(sessions, Some("trusty-tools"));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "s1");
}

/// Matching is case-insensitive regardless of which column matches.
#[test]
fn filter_sessions_by_term_is_case_insensitive() {
    let sessions = vec![ls_test_session("MyApp", "ACTIVE", None, None, None, None)];
    assert_eq!(
        filter_sessions_by_term(sessions.clone(), Some("myapp")).len(),
        1
    );
    assert_eq!(filter_sessions_by_term(sessions, Some("active")).len(), 1);
}

/// A filter matching nothing returns an empty result (not an error).
#[test]
fn filter_sessions_by_term_no_match_returns_empty() {
    let sessions = vec![ls_test_session("s1", "active", None, None, None, None)];
    let out = filter_sessions_by_term(sessions, Some("nonexistent-substring"));
    assert!(out.is_empty(), "no match must yield an empty result");
}

/// `term = None` is a no-op — every session passes through unchanged.
#[test]
fn filter_sessions_by_term_none_is_noop() {
    let sessions = vec![
        ls_test_session("s1", "active", None, None, None, None),
        ls_test_session("s2", "stopped", None, None, None, None),
    ];
    assert_eq!(filter_sessions_by_term(sessions, None).len(), 2);
}

/// `Recent` orders descending by `last_activity_at` (most recent first).
#[test]
fn sort_sessions_recent_orders_by_last_activity() {
    let mut sessions = vec![
        ls_test_session(
            "older",
            "active",
            Some("2026-01-01T00:00:00Z"),
            None,
            None,
            None,
        ),
        ls_test_session(
            "newest",
            "active",
            Some("2026-07-01T00:00:00Z"),
            None,
            None,
            None,
        ),
        ls_test_session(
            "middle",
            "active",
            Some("2026-04-01T00:00:00Z"),
            None,
            None,
            None,
        ),
    ];
    sort_sessions(&mut sessions, SessionSortArg::Recent);
    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["newest", "middle", "older"]);
}

/// `Recent` falls back to `created_at` when `last_activity_at` is absent, and
/// a session with neither timestamp sorts last.
#[test]
fn sort_sessions_recent_falls_back_to_created_at() {
    let mut sessions = vec![
        ls_test_session("no-timestamp", "active", None, None, None, None),
        ls_test_session(
            "created-only",
            "active",
            None,
            Some("2026-03-01T00:00:00Z"),
            None,
            None,
        ),
        ls_test_session(
            "has-activity",
            "active",
            Some("2026-06-01T00:00:00Z"),
            Some("2026-01-01T00:00:00Z"),
            None,
            None,
        ),
    ];
    sort_sessions(&mut sessions, SessionSortArg::Recent);
    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["has-activity", "created-only", "no-timestamp"]);
}

/// `Alpha` orders ascending by `name`, case-insensitively.
#[test]
fn sort_sessions_alpha_orders_by_name_case_insensitive() {
    let mut sessions = vec![
        ls_test_session("Zebra", "active", None, None, None, None),
        ls_test_session("apple", "active", None, None, None, None),
        ls_test_session("Mango", "active", None, None, None, None),
    ];
    sort_sessions(&mut sessions, SessionSortArg::Alpha);
    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["apple", "Mango", "Zebra"]);
}

/// Filter + sort combined: filtering first, then sorting the remaining rows.
#[test]
fn filter_and_sort_combined() {
    let sessions = vec![
        ls_test_session(
            "api-old",
            "active",
            Some("2026-01-01T00:00:00Z"),
            None,
            None,
            None,
        ),
        ls_test_session(
            "web-new",
            "active",
            Some("2026-07-01T00:00:00Z"),
            None,
            None,
            None,
        ),
        ls_test_session(
            "api-new",
            "active",
            Some("2026-06-01T00:00:00Z"),
            None,
            None,
            None,
        ),
    ];
    let mut filtered = filter_sessions_by_term(sessions, Some("api"));
    assert_eq!(
        filtered.len(),
        2,
        "only the two 'api' sessions survive the filter"
    );
    sort_sessions(&mut filtered, SessionSortArg::Recent);
    let names: Vec<&str> = filtered.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["api-new", "api-old"]);
}

/// `tm ls recent` parses with `recent` captured in `terms`.
#[test]
fn cli_parses_ls_terms_recent_keyword() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "recent"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { terms, .. } => assert_eq!(terms, vec!["recent".to_string()]),
        other => panic!("expected top-level ls recent, got {other:?}"),
    }
}

/// `tm ls alpha foo` parses both words into `terms`, in order.
#[test]
fn cli_parses_ls_terms_alpha_with_filter() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "alpha", "foo"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { terms, .. } => {
            assert_eq!(terms, vec!["alpha".to_string(), "foo".to_string()])
        }
        other => panic!("expected top-level ls alpha foo, got {other:?}"),
    }
}

/// `tm ls somefilter` (no keyword) still parses fine as a single positional
/// term — disambiguation happens later, in `parse_ls_terms`, not at the clap
/// layer.
#[test]
fn cli_parses_ls_terms_bare_filter_word() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "somefilter"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { terms, .. } => assert_eq!(terms, vec!["somefilter".to_string()]),
        other => panic!("expected top-level ls somefilter, got {other:?}"),
    }
}

/// Positional `terms` compose with existing flags (e.g. `--source-id`).
#[test]
fn cli_parses_ls_terms_with_source_id_flag() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "ls",
        "--source-id",
        "owner/repo",
        "alpha",
        "foo",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            terms, source_id, ..
        } => {
            assert_eq!(terms, vec!["alpha".to_string(), "foo".to_string()]);
            assert_eq!(source_id, Some("owner/repo".to_string()));
        }
        other => panic!("expected top-level ls with source-id + terms, got {other:?}"),
    }
}

/// `tm sessions ls alpha foo` mirrors the same grammar on the canonical plural
/// surface (the `SessionAction::Ls` variant shared with `tm ls`).
#[test]
fn cli_parses_sessions_ls_terms_alpha_with_filter() {
    let cli = Cli::try_parse_from(["trusty-mpm", "sessions", "ls", "alpha", "foo"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessions {
            action: SessionAction::Ls { terms, .. },
        } => assert_eq!(terms, vec!["alpha".to_string(), "foo".to_string()]),
        other => panic!("expected sessions ls alpha foo, got {other:?}"),
    }
}

// ── #2304: picker delete — family routing + running-session force guard ─────

use crate::commands::picker_delete::{
    ManagedDeleteNext, classify_managed_delete, confirm_is_force, confirm_is_yes,
    delete_needs_force,
};

/// A running (active/provisioning) session requires an explicit force-confirm.
#[test]
fn delete_needs_force_running_true() {
    assert!(delete_needs_force("active"));
    assert!(delete_needs_force("provisioning"));
}

/// A stopped/errored session has no live runtime — a plain confirm is enough.
#[test]
fn delete_needs_force_stopped_and_errored_false() {
    assert!(!delete_needs_force("stopped"));
    assert!(!delete_needs_force("errored"));
}

/// Family routing: 200 keeps the managed delete, 404 falls back to the local
/// (project-session) store, 409 surfaces the running-guard refusal, and any
/// other status is an error.
#[test]
fn classify_managed_delete_maps_each_family() {
    assert_eq!(
        classify_managed_delete(reqwest::StatusCode::OK),
        ManagedDeleteNext::Deleted
    );
    assert_eq!(
        classify_managed_delete(reqwest::StatusCode::NOT_FOUND),
        ManagedDeleteNext::FallbackLocal,
        "404 -> project-session fallback (the other family)"
    );
    assert_eq!(
        classify_managed_delete(reqwest::StatusCode::CONFLICT),
        ManagedDeleteNext::Refused,
        "409 -> running guard, never auto-force"
    );
    assert_eq!(
        classify_managed_delete(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
        ManagedDeleteNext::Error
    );
}

/// The safe (non-force) confirm accepts only `y`/`yes`; empty is the safe reject.
#[test]
fn confirm_is_yes_accepts_only_y_variants() {
    assert!(confirm_is_yes("y"));
    assert!(confirm_is_yes("Y"));
    assert!(confirm_is_yes(" yes \n"));
    assert!(!confirm_is_yes(""));
    assert!(!confirm_is_yes("force"));
    assert!(!confirm_is_yes("delete"));
}

/// The force confirm requires the exact word `force` — a bare `y` is NOT enough.
#[test]
fn confirm_is_force_requires_the_word_force() {
    assert!(confirm_is_force("force"));
    assert!(confirm_is_force(" FORCE \n"));
    assert!(!confirm_is_force("y"));
    assert!(!confirm_is_force("yes"));
    assert!(!confirm_is_force(""));
}

// ── #2304 CRITICAL fix: local-session running guard ─────────────────────────
//
// `DELETE /sessions/{id}` (`remove_session` in `daemon/api.rs`) has NO
// server-side running guard — it unconditionally kills the tmux host for any
// status. Before this fix, `delete_local` issued that DELETE straight through
// on a managed-store 404, which meant a bare `tm session delete <local-id>`
// (previously a harmless "not found") could silently force-kill a LIVE local
// session. These tests drive the real HTTP path (a loopback daemon), not just
// the pure guard function, so a wiring mistake in `delete_local` itself would
// be caught, not just a bug in the predicate.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use trusty_mpm::core::session::{ControlModel, Session, SessionId, SessionStatus};
use trusty_mpm::daemon::state::DaemonState;

use crate::commands::picker_delete::{
    DeleteReport, delete_managed_then_local, local_session_needs_force,
};

/// A hermetic daemon bound to a random loopback port, for real-HTTP round trips
/// through `delete_managed_then_local`'s local-fallback guard.
///
/// Why: mirrors `tests/test_session_lifecycle.rs`'s `TestServer` — a real bind
/// is the only way to prove the CLI-side guard actually runs BEFORE the DELETE
/// reaches the daemon, not just that the predicate returns the right bool.
struct LocalTestServer {
    url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl LocalTestServer {
    /// Serve `state` (pre-seeded by the caller) on a fresh loopback port and
    /// block until `/health` answers.
    async fn spawn(state: std::sync::Arc<DaemonState>) -> Self {
        let app = trusty_mpm::daemon::api::router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback port");
        let addr: SocketAddr = listener.local_addr().expect("resolve bound addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let url = format!("http://{addr}");
        Self::wait_for_health(&url).await;
        Self { url, handle }
    }

    async fn wait_for_health(base: &str) {
        let client = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        let health = format!("{base}/health");
        loop {
            if let Ok(resp) = client.get(&health).send().await
                && resp.status().is_success()
            {
                return;
            }
            if Instant::now() >= deadline {
                panic!("daemon at {base} did not become healthy within 2s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for LocalTestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Spin up a hermetic daemon with exactly one local session pre-registered at
/// `status`; returns the server (keep it alive for the test's duration) and the
/// session's id string.
async fn spawn_with_local_session(status: SessionStatus) -> (LocalTestServer, String) {
    let state = DaemonState::shared();
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/2304-fixture", ControlModel::Tmux, None);
    session.status = status;
    state.register_session(session);
    let server = LocalTestServer::spawn(state).await;
    (server, id.0.to_string())
}

/// A RUNNING local session (`Active`) must be REFUSED without `--force` — never
/// silently killed. This is the exact scenario the review flagged: before the
/// fix, this call would have gone straight through with no guard at all.
#[tokio::test]
async fn local_delete_refuses_running_session_without_force() {
    let (server, id) = spawn_with_local_session(SessionStatus::Active).await;
    let client = reqwest::Client::new();

    let report = delete_managed_then_local(&client, &server.url, &id, false)
        .await
        .expect("routing call must not error");
    assert!(
        matches!(report, DeleteReport::Refused(_)),
        "running local session without --force must be Refused, got {report:?}"
    );

    // The guard must have fired BEFORE the DELETE — the session is still there.
    let still_there = client
        .get(format!("{}/sessions/{id}", server.url))
        .send()
        .await
        .expect("get session")
        .status();
    assert_eq!(
        still_there,
        reqwest::StatusCode::OK,
        "session must NOT have been deleted"
    );
}

/// A STOPPED local session deletes cleanly WITHOUT `--force`.
#[tokio::test]
async fn local_delete_allows_stopped_session_without_force() {
    let (server, id) = spawn_with_local_session(SessionStatus::Stopped).await;
    let client = reqwest::Client::new();

    let report = delete_managed_then_local(&client, &server.url, &id, false)
        .await
        .expect("routing call must not error");
    match report {
        DeleteReport::Deleted { local, .. } => assert!(local, "must report the local fallback"),
        other => panic!("expected Deleted, got {other:?}"),
    }

    let gone = client
        .get(format!("{}/sessions/{id}", server.url))
        .send()
        .await
        .expect("get session")
        .status();
    assert_eq!(gone, reqwest::StatusCode::NOT_FOUND, "session must be gone");
}

/// `force = true` (as sent after the picker's type-`force` confirm, or
/// `tm session delete --force`) bypasses the local guard even on a RUNNING
/// session — proving the guard interoperates with the existing force plumbing
/// rather than fighting it.
#[tokio::test]
async fn local_delete_force_bypasses_guard_on_running_session() {
    let (server, id) = spawn_with_local_session(SessionStatus::Active).await;
    let client = reqwest::Client::new();

    let report = delete_managed_then_local(&client, &server.url, &id, true)
        .await
        .expect("routing call must not error");
    match report {
        DeleteReport::Deleted { local, .. } => assert!(local),
        other => panic!("expected Deleted with force=true, got {other:?}"),
    }
}

// ── local_session_needs_force — pure guard predicate ─────────────────────────

#[test]
fn local_session_needs_force_stopped_false() {
    assert!(!local_session_needs_force(SessionStatus::Stopped));
}

#[test]
fn local_session_needs_force_active_and_others_true() {
    assert!(local_session_needs_force(SessionStatus::Starting));
    assert!(local_session_needs_force(SessionStatus::Active));
    assert!(local_session_needs_force(SessionStatus::AwaitingApproval));
    assert!(local_session_needs_force(SessionStatus::Detached));
    assert!(local_session_needs_force(SessionStatus::Paused));
}
