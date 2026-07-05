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
                },
        } => {
            assert_eq!(repo, "https://github.com/owner/repo");
            assert_eq!(git_ref, "feat/x");
            assert_eq!(task, "do the thing");
            assert_eq!(name_hint.as_deref(), Some("ticket-1"));
            // No --runtime flag → default claude-code (now a typed RuntimeKind).
            assert_eq!(runtime, trusty_mpm::runtime::RuntimeKind::ClaudeCode);
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
