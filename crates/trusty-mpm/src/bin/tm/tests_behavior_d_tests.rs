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
            action:
                SessionAction::PruneWorktrees {
                    force,
                    discard_dirty,
                    merged_prs,
                },
        } => {
            assert!(!force, "default must be dry-run (force=false)");
            assert!(!discard_dirty, "default must never discard dirty work");
            assert!(
                !merged_prs,
                "#2919: the merged-PR pass must be off by default"
            );
        }
        other => panic!("expected session prune-worktrees, got {other:?}"),
    }
    // With --force, actually deletes.
    let cli2 =
        Cli::try_parse_from(["trusty-mpm", "session", "prune-worktrees", "--force"]).unwrap();
    match cli2.command.unwrap() {
        Command::Session {
            action:
                SessionAction::PruneWorktrees {
                    force,
                    discard_dirty,
                    merged_prs,
                },
        } => {
            assert!(force, "--force must set force=true");
            assert!(
                !discard_dirty,
                "#4091: --force alone must NOT imply discarding uncommitted work"
            );
            assert!(
                !merged_prs,
                "#2919: --force alone must NOT imply the merged-PR reclaim pass"
            );
        }
        other => panic!("expected session prune-worktrees, got {other:?}"),
    }
}

/// #4091: discarding uncommitted work requires its own explicit flag — it is
/// never implied by `--force`, and never on by default.
///
/// Why: `--force` already means "stop previewing, actually delete", and an
/// operator reaching for it to clear stale directories must not silently also
/// authorise destroying another session's unsaved work. Two separate flags
/// keep those two decisions separate.
#[test]
fn cli_prune_worktrees_discard_dirty_is_opt_in() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "prune-worktrees",
        "--force",
        "--discard-dirty",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::PruneWorktrees {
                    force,
                    discard_dirty,
                    merged_prs,
                },
        } => {
            assert!(force);
            assert!(discard_dirty, "--discard-dirty must set discard_dirty=true");
            assert!(
                !merged_prs,
                "#2919: --discard-dirty must NOT imply the merged-PR reclaim pass"
            );
        }
        other => panic!("expected session prune-worktrees, got {other:?}"),
    }
}

/// #2919: the merged-pull-request reclaim pass requires its own explicit flag.
///
/// Why: it is the only reclaim path that acts on GitHub state, so an operator
/// clearing stale directories must opt into it deliberately rather than
/// inheriting it from `--force`. Pinning it as a third independent flag is what
/// keeps anything automatic from ever reaching a merged-PR deletion.
#[test]
fn cli_prune_worktrees_merged_prs_is_opt_in() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "prune-worktrees",
        "--force",
        "--merged-prs",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::PruneWorktrees {
                    force,
                    discard_dirty,
                    merged_prs,
                },
        } => {
            assert!(force);
            assert!(merged_prs, "--merged-prs must set merged_prs=true");
            assert!(
                !discard_dirty,
                "#2919: --merged-prs must NOT imply discarding uncommitted work"
            );
        }
        other => panic!("expected session prune-worktrees, got {other:?}"),
    }
}

/// #4288: `reconcile-worktrees` parses, and defaults to the human-readable
/// form.
#[test]
fn cli_parses_session_reconcile_worktrees() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "reconcile-worktrees"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::ReconcileWorktrees { json },
        } => assert!(!json, "the default must be the readable report, not JSON"),
        other => panic!("expected session reconcile-worktrees, got {other:?}"),
    }
    let as_json =
        Cli::try_parse_from(["trusty-mpm", "session", "reconcile-worktrees", "--json"]).unwrap();
    match as_json.command.unwrap() {
        Command::Session {
            action: SessionAction::ReconcileWorktrees { json },
        } => assert!(json),
        other => panic!("expected session reconcile-worktrees, got {other:?}"),
    }
}

/// #4288: the reconcile verb has NO destructive flag — not `--force`, not
/// `--discard-dirty`, not `--dry-run`.
///
/// Why: slice 3 is report-only, and the way a reporting verb stops being
/// report-only is that someone adds a flag to it. `clap` rejecting these at
/// parse time is the mechanical form of "there is no destructive form of this
/// command", and this test turns any such addition red.
#[test]
fn cli_reconcile_worktrees_takes_no_destructive_flag() {
    for flag in ["--force", "--discard-dirty", "--dry-run"] {
        assert!(
            Cli::try_parse_from(["trusty-mpm", "session", "reconcile-worktrees", flag]).is_err(),
            "`reconcile-worktrees {flag}` must not parse — slice 3 writes nothing"
        );
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
            attached,
            no_prune,
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
            assert!(!attached, "bare `tm ls` must not set --attached");
            assert!(!no_prune, "bare `tm ls` keeps the #4702 auto-prune");
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

/// `tm ls -a` parses as attached-only, and does NOT set `--all`.
///
/// Why: `-a` conventionally means "all" (`ls -a`, `docker ps -a`), and `--all`
/// is a real neighbouring flag on this very command. This asserts the short
/// binds to `--attached` and leaves `--all` alone, so a future reader cannot
/// quietly swap them.
#[test]
fn cli_parses_ls_attached_short() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "-a"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls {
            attached,
            all,
            projects,
            ..
        } => {
            assert!(attached, "-a must parse to --attached");
            assert!(!all, "-a must NOT set --all");
            assert!(!projects, "-a alone must not imply --projects");
        }
        other => panic!("expected top-level ls -a, got {other:?}"),
    }
}

/// `tm ls --attached` is the long spelling of `-a`.
#[test]
fn cli_parses_ls_attached_long() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--attached"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { attached, all, .. } => {
            assert!(attached, "--attached must parse to true");
            assert!(!all);
        }
        other => panic!("expected top-level ls --attached, got {other:?}"),
    }
}

/// `--all` stays long-only: it and `-a` are independent flags that can coexist.
#[test]
fn cli_parses_ls_all_and_attached_are_independent() {
    let cli = Cli::try_parse_from(["trusty-mpm", "ls", "--all"]).unwrap();
    match cli.command.unwrap() {
        Command::Ls { all, attached, .. } => {
            assert!(all, "--all must parse to true");
            assert!(!attached, "--all must NOT imply --attached");
        }
        other => panic!("expected top-level ls --all, got {other:?}"),
    }
    let both = Cli::try_parse_from(["trusty-mpm", "ls", "--all", "-a"]).unwrap();
    match both.command.unwrap() {
        Command::Ls { all, attached, .. } => {
            assert!(
                all && attached,
                "--all -a must parse as both, not a conflict"
            );
        }
        other => panic!("expected top-level ls --all -a, got {other:?}"),
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

use crate::commands::session_ls_connector::should_show_picker;

/// The picker opens only on a fully-interactive terminal with ≥1 session.
#[test]
fn ls_connector_should_show_picker_interactive_with_sessions() {
    assert!(should_show_picker(true, true, false, false, false, 1));
    assert!(should_show_picker(true, true, false, false, false, 5));
}

/// A non-TTY stdin OR stdout forces the static (pipeable) list path — never a
/// blocking picker. Mirrors guided.rs's non-TTY gate.
#[test]
fn ls_connector_should_show_picker_non_tty_static() {
    assert!(
        !should_show_picker(false, true, false, false, false, 3),
        "piped stdin -> static"
    );
    assert!(
        !should_show_picker(true, false, false, false, false, 3),
        "piped stdout -> static"
    );
    assert!(!should_show_picker(false, false, false, false, false, 3));
}

/// `--json` and `--all` force static output even on a TTY, and 0 sessions never
/// opens an empty picker.
#[test]
fn ls_connector_should_show_picker_flags_and_empty_static() {
    assert!(
        !should_show_picker(true, true, true, false, false, 3),
        "--json -> static"
    );
    assert!(
        !should_show_picker(true, true, false, true, false, 3),
        "--all -> static"
    );
    assert!(
        !should_show_picker(true, true, false, false, false, 0),
        "0 sessions -> static"
    );
}

/// `-a` forces the static listing even on a full TTY with sessions present.
///
/// Why: the sessions `-a` keeps are exactly the ones a client is already on, so
/// opening the picker to "connect" to one is a no-op. The flag is a listing
/// question, and it must answer in a pipeable table like `--all` does.
#[test]
fn ls_connector_should_show_picker_attached_static() {
    assert!(
        !should_show_picker(true, true, false, false, true, 3),
        "--attached -> static even with sessions on a TTY"
    );
    assert!(
        should_show_picker(true, true, false, false, false, 3),
        "the same invocation WITHOUT -a still opens the picker"
    );
}

// ── `tm ls` inline sort/filter grammar (#3483, PM correction) ───────────
//
// The repo owner asked for `--sort`/`--filter` to be expressed as bare
// positional words instead of flags: `tm ls [recent|alpha] [filter-word...]`.
// `parse_ls_terms` is the pure grammar seam; `filter_sessions_by_term` and
// `sort_sessions` are the pure list operations it feeds.

use crate::commands::session_picker::{
    SessionFilter, SessionSortArg, filter_sessions_by_term, parse_ls_terms, sort_sessions,
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
        stale_assets_unchecked: false,
        attached: false,
        slot: 0,
        deleted: false,
    }
}

// ── `tm ls -a` — attached-only listing ──────────────────────────────────────

/// `-a` keeps exactly the sessions a tmux client is on.
#[test]
fn filter_attached_keeps_only_attached() {
    let mut connected = ls_test_session("connected", "active", None, None, None, None);
    connected.attached = true;
    let mut also_connected = ls_test_session("also-connected", "active", None, None, None, None);
    also_connected.attached = true;
    let lonely = ls_test_session("lonely", "active", None, None, None, None);

    let out =
        crate::commands::managed::filter_attached(vec![connected, lonely, also_connected], true);

    let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["connected", "also-connected"]);
}

/// A session that is RUNNING but has no client attached must NOT appear under
/// `-a`.
///
/// Why: this is the whole feature. `state == "active"` means the tmux entity is
/// live; `attached` means somebody is looking at it. If a detached-but-running
/// session survived this filter, `-a` would just be a slower spelling of the
/// default listing.
#[test]
fn filter_attached_excludes_running_but_detached() {
    // Identical in every respect EXCEPT the attach flag — so nothing but
    // `attached` can be what separates them.
    let mut watched = ls_test_session("running", "active", None, None, None, None);
    watched.attached = true;
    let mut unwatched = ls_test_session("running", "active", None, None, None, None);
    unwatched.attached = false;

    let out = crate::commands::managed::filter_attached(vec![watched, unwatched], true);

    assert_eq!(
        out.len(),
        1,
        "the detached `active` session must be dropped"
    );
    assert!(out[0].attached);
}

/// With `-a` off, the listing is byte-identical to today's — the filter is a
/// no-op, not a reorder or a dedupe.
#[test]
fn filter_attached_disabled_is_noop() {
    let mut connected = ls_test_session("connected", "active", None, None, None, None);
    connected.attached = true;
    let sessions = vec![
        ls_test_session("stopped-one", "stopped", None, None, None, None),
        connected,
        ls_test_session("detached-active", "active", None, None, None, None),
    ];
    let before: Vec<String> = sessions.iter().map(|s| s.name.clone()).collect();

    let out = crate::commands::managed::filter_attached(sessions, false);

    let after: Vec<String> = out.iter().map(|s| s.name.clone()).collect();
    assert_eq!(before, after, "-a off must leave the list untouched");
}

/// Zero attached sessions is an ANSWER, not an error: `-a` says so in words.
#[test]
fn filter_attached_zero_matches_yields_the_explicit_line() {
    let running_but_detached = vec![
        ls_test_session("a", "active", None, None, None, None),
        ls_test_session("b", "active", None, None, None, None),
    ];

    let out = crate::commands::managed::filter_attached(running_but_detached, true);

    assert!(out.is_empty(), "no session in the fixture is attached");
    // The caller renders this line and returns Ok(()) — exit 0, never an error.
    assert_eq!(
        crate::commands::managed_render::no_attached_sessions_line(None),
        "no attached sessions"
    );
}

/// The zero-attached line, unscoped.
#[test]
fn render_no_attached_sessions_line_unscoped() {
    assert_eq!(
        crate::commands::managed_render::no_attached_sessions_line(None),
        "no attached sessions"
    );
}

/// The zero-attached line names the scope when one was given, mirroring
/// `render_session_table`'s "no managed sessions for <slug>".
#[test]
fn render_no_attached_sessions_line_scoped_to_source_id() {
    assert_eq!(
        crate::commands::managed_render::no_attached_sessions_line(Some("owner/repo")),
        "no attached sessions for owner/repo"
    );
}

/// The filter matches a substring of the `name` column, case-insensitively.
#[test]
fn filter_sessions_by_term_matches_name() {
    let sessions = vec![
        ls_test_session("api-worker", "active", None, None, None, None),
        ls_test_session("web-frontend", "active", None, None, None, None),
    ];
    let out = filter_sessions_by_term(sessions, Some(&SessionFilter::visible("API")));
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
    let out = filter_sessions_by_term(sessions, Some(&SessionFilter::visible("login")));
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
    let out = filter_sessions_by_term(sessions, Some(&SessionFilter::visible("trusty-tools")));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "s1");
}

/// Matching is case-insensitive regardless of which column matches.
#[test]
fn filter_sessions_by_term_is_case_insensitive() {
    let sessions = vec![ls_test_session("MyApp", "ACTIVE", None, None, None, None)];
    assert_eq!(
        filter_sessions_by_term(sessions.clone(), Some(&SessionFilter::visible("myapp"))).len(),
        1
    );
    assert_eq!(
        filter_sessions_by_term(sessions, Some(&SessionFilter::visible("active"))).len(),
        1
    );
}

/// A filter matching nothing returns an empty result (not an error).
#[test]
fn filter_sessions_by_term_no_match_returns_empty() {
    let sessions = vec![ls_test_session("s1", "active", None, None, None, None)];
    let out = filter_sessions_by_term(
        sessions,
        Some(&SessionFilter::visible("nonexistent-substring")),
    );
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

// ── ordering: attached → active → stopped groups (owner request 2026-07-29) ─
//
// Bob's ask: the listing groups attached first, then active, then stopped —
// ABOVE whatever `recent`/`alpha` secondary sort applies. These tests use a
// deliberately-adversarial recency/alpha order within each group (the
// stopped/attached/active rows are seeded so the OLD flat sort would have
// interleaved them) to prove the grouping wins over the secondary key, not
// merely coincide with it.

/// `Recent` groups attached-first, then active, then everything else — even
/// when a "stopped" row is more recently active than the "active" one.
#[test]
fn sort_sessions_recent_groups_attached_before_active_before_stopped() {
    let mut stopped_but_newest = ls_test_session(
        "stopped-newest",
        "stopped",
        Some("2026-07-01T00:00:00Z"),
        None,
        None,
        None,
    );
    stopped_but_newest.attached = false;
    let mut active_but_older = ls_test_session(
        "active-older",
        "active",
        Some("2026-01-01T00:00:00Z"),
        None,
        None,
        None,
    );
    active_but_older.attached = false;
    let mut attached_but_oldest = ls_test_session(
        "attached-oldest",
        "active",
        Some("2025-01-01T00:00:00Z"),
        None,
        None,
        None,
    );
    attached_but_oldest.attached = true;

    let mut sessions = vec![stopped_but_newest, active_but_older, attached_but_oldest];
    sort_sessions(&mut sessions, SessionSortArg::Recent);
    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["attached-oldest", "active-older", "stopped-newest"],
        "attached must lead regardless of recency, active must lead over \
         stopped regardless of recency"
    );
}

/// `Alpha` groups attached-first, then active, then everything else — even
/// when a "stopped" row's name would otherwise sort first alphabetically.
#[test]
fn sort_sessions_alpha_groups_attached_before_active_before_stopped() {
    let mut stopped_but_a = ls_test_session("aaa-stopped", "stopped", None, None, None, None);
    stopped_but_a.attached = false;
    let mut active_but_m = ls_test_session("mmm-active", "active", None, None, None, None);
    active_but_m.attached = false;
    let mut attached_but_z = ls_test_session("zzz-attached", "active", None, None, None, None);
    attached_but_z.attached = true;

    let mut sessions = vec![stopped_but_a, active_but_m, attached_but_z];
    sort_sessions(&mut sessions, SessionSortArg::Alpha);
    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["zzz-attached", "mmm-active", "aaa-stopped"],
        "attached must lead regardless of name, active must lead over \
         stopped regardless of name"
    );
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
    let mut filtered = filter_sessions_by_term(sessions, Some(&SessionFilter::visible("api")));
    assert_eq!(
        filtered.len(),
        2,
        "only the two 'api' sessions survive the filter"
    );
    sort_sessions(&mut filtered, SessionSortArg::Recent);
    let names: Vec<&str> = filtered.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["api-new", "api-old"]);
}

// ── stale-daemon slot fallback (issue #3678) ─────────────────────────────────
//
// A daemon that predates #3034 omits `slot`/`deleted` from its response
// entirely; `ManagedSessionSummary`'s `#[serde(default)]` then decodes every
// row's `slot` to the shared `0` sentinel instead of erroring. Before this
// fix that collapsed the picker's rendered numbers to `[0]` on every row and
// resolved every typed choice to the first session. `slots_are_stale`
// detects the shape; `find_slot`/`shown_slot` fall back to a 1-based
// positional number so the printed menu and accepted input agree again. End-
// to-end coverage through `parse_picker_choice` lives in
// `guided_picker_stale_daemon_*` in `tests_behavior_c_tests.rs`; these test
// the three helpers directly. `ls_test_session` defaults `slot: 0`, so it
// doubles as the stale-daemon fixture with no extra plumbing.

use crate::commands::session_picker::{find_slot, shown_slot, slots_are_stale};

#[test]
fn slots_are_stale_true_when_all_zero() {
    let sessions = vec![
        ls_test_session("s1", "active", None, None, None, None),
        ls_test_session("s2", "active", None, None, None, None),
        ls_test_session("s3", "active", None, None, None, None),
    ];
    assert!(slots_are_stale(&sessions));
}

#[test]
fn slots_are_stale_false_when_any_nonzero() {
    // A single genuinely-assigned slot is conclusive proof the daemon is
    // healthy — never treat a real (possibly sparse) menu as stale.
    let mut sessions = vec![
        ls_test_session("s1", "active", None, None, None, None),
        ls_test_session("s2", "active", None, None, None, None),
    ];
    sessions[1].slot = 2;
    assert!(!slots_are_stale(&sessions));
}

#[test]
fn slots_are_stale_false_when_empty() {
    assert!(!slots_are_stale(&[]));
}

#[test]
fn find_slot_healthy_looks_up_by_real_slot() {
    let mut sessions = vec![
        ls_test_session("s1", "active", None, None, None, None),
        ls_test_session("s2", "active", None, None, None, None),
    ];
    sessions[0].slot = 1;
    sessions[1].slot = 5;
    assert_eq!(find_slot(&sessions, 5), Some(1));
    assert_eq!(find_slot(&sessions, 2), None);
}

#[test]
fn find_slot_stale_fallback_resolves_positionally() {
    // With every slot decoded to `0`, a healthy by-slot lookup would always
    // resolve to position 0 (or never resolve past the first row) — the
    // fallback must resolve `n` to position `n-1` instead, matching what
    // `shown_slot` renders.
    let sessions = vec![
        ls_test_session("s1", "active", None, None, None, None),
        ls_test_session("s2", "active", None, None, None, None),
        ls_test_session("s3", "active", None, None, None, None),
    ];
    assert_eq!(find_slot(&sessions, 1), Some(0));
    assert_eq!(find_slot(&sessions, 2), Some(1));
    assert_eq!(find_slot(&sessions, 3), Some(2));
    assert_eq!(find_slot(&sessions, 4), None);
    assert_eq!(find_slot(&sessions, 0), None);
}

#[test]
fn shown_slot_healthy_uses_real_slot() {
    let mut sessions = vec![ls_test_session("s1", "active", None, None, None, None)];
    sessions[0].slot = 7;
    assert_eq!(shown_slot(&sessions, 0), 7);
}

#[test]
fn shown_slot_stale_fallback_is_distinct_and_incrementing() {
    let sessions = vec![
        ls_test_session("s1", "active", None, None, None, None),
        ls_test_session("s2", "active", None, None, None, None),
        ls_test_session("s3", "active", None, None, None, None),
    ];
    assert_eq!(shown_slot(&sessions, 0), 1);
    assert_eq!(shown_slot(&sessions, 1), 2);
    assert_eq!(shown_slot(&sessions, 2), 3);
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

// ── auto-prune dead (unresumable) records at listing time (owner request
// 2026-07-29; hardened per code-critic WARN, PR #4384 review) ──────────────
//
// `auto_prune_dead_records_at` partitions on the daemon-computed `unresumable`
// flag and, for anything CONFIRMED dead (seen on two consecutive calls),
// drives the REAL `/decommission?record_only=true` HTTP route against a
// hermetic loopback daemon (mirroring `LocalTestServer` above) — proving the
// wiring, not just the pure partition logic. Every test uses an explicit
// tempdir-rooted marker path (`auto_prune_dead_records_at`), never the
// production `auto_prune_dead_records` wrapper, which resolves the real
// `~/.trusty-mpm` — tests must never touch the developer's real home dir.

use std::collections::HashSet;

use crate::commands::session_picker_prune::auto_prune_dead_records_at;

/// Seed one managed session whose workspace path is NEVER created on disk —
/// every workdir candidate is therefore verifiably absent, exactly
/// `is_unresumable`'s gate — and mark it `Errored` so it becomes
/// `unresumable: true` on the next fetch. Returns the session id.
async fn seed_dead_session(
    mgr: &trusty_mpm::session_manager::SessionManager,
    root: &std::path::Path,
    label: &str,
) -> trusty_mpm::session_manager::ManagedSessionId {
    let gone = root.join(format!("gone-workspace-{label}"));
    let id = trusty_mpm::session_manager::ManagedSessionId::new();
    mgr.create_with_id(
        id,
        format!("regression: auto-prune dead record {label}"),
        Some(gone.clone()),
        None,
        Some(gone),
        Some("https://example.com/r.git".to_string()),
        Some("main".to_string()),
        trusty_mpm::runtime::RuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed dead session");
    mgr.mark_errored(&id, "regression: simulate prior spawn failure")
        .await
        .expect("mark errored");
    id
}

/// A `PruneContext` bound to a tempdir marker with NO live tmux sessions.
///
/// Why: `session_ls_at` must never reach the real tmux enumeration in a test.
/// CI proved why: the `session_ls_*` tests did exactly that, so they passed on a
/// developer machine (tmux present) and failed on a runner with no tmux, where
/// the fail-closed rule correctly pruned nothing. An explicit empty set makes
/// the test say what it means — "no session is live" — on every machine.
fn hermetic_prune_ctx(
    marker_path: &std::path::Path,
) -> crate::commands::session_picker_prune::PruneContext {
    crate::commands::session_picker_prune::PruneContext::hermetic(
        marker_path.to_path_buf(),
        Some(HashSet::new()),
    )
}

/// Set every RECORD sighting to exactly `minutes` ago, leaving the stale-daemon
/// sentinel untouched (#4702).
///
/// Why: confirmation requires a real elapsed interval (`SIGHTING_MIN_AGE_SECS`,
/// 10 minutes). Tests simulate that rather than sleeping through it, exercising
/// the real comparison against the real constant instead of weakening either.
///
/// 🔴 This OVERWRITES the stamp, so it also masks a restamp bug: a test that
/// calls it between every listing cannot observe the clock being reset (PR #4725
/// review round 2). Use it only to establish a starting age. To simulate time
/// passing ACROSS listings, use [`age_sightings_by`], which is relative.
fn backdate_sightings(marker_path: &std::path::Path, minutes: i64) {
    let backdated = (chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339();
    rewrite_sightings(marker_path, |_| backdated.clone());
}

/// Move every RECORD sighting `minutes` FURTHER into the past, relative to
/// whatever is currently persisted (PR #4725 review round 2).
///
/// Why: this is how a test simulates wall-clock advance without destroying the
/// evidence. Because it is relative, a restamp performed by an intervening
/// listing survives into the next assertion — so a test using this catches the
/// "window resets on every listing" defect that an absolute overwrite hides.
fn age_sightings_by(marker_path: &std::path::Path, minutes: i64) {
    rewrite_sightings(marker_path, |current| {
        let parsed = chrono::DateTime::parse_from_rfc3339(current)
            .expect("a persisted sighting must be valid RFC 3339 to age it");
        (parsed - chrono::Duration::minutes(minutes)).to_rfc3339()
    });
}

/// Shared marker-file rewrite used by [`backdate_sightings`] and
/// [`age_sightings_by`]; never touches the stale-daemon sentinel.
fn rewrite_sightings(marker_path: &std::path::Path, f: impl Fn(&str) -> String) {
    let raw = std::fs::read_to_string(marker_path).expect("marker file must exist");
    let mut seen: std::collections::HashMap<String, String> =
        serde_json::from_str(&raw).expect("marker file must parse");
    for (key, value) in seen.iter_mut() {
        if key != "__stale_daemon_detected__" {
            *value = f(value);
        }
    }
    std::fs::write(marker_path, serde_json::to_string(&seen).unwrap()).unwrap();
}

/// Fetch the RAW (non-auto-pruning) live session list — bypasses
/// `fetch_live_sessions`'s own internal auto-prune entirely, so a test can
/// inspect pre-prune state or drive `auto_prune_dead_records_at` manually
/// without a prior fetch silently consuming the record.
async fn fetch_raw_live(
    client: &reqwest::Client,
    url: &str,
) -> Vec<trusty_mpm::client::ManagedSessionSummary> {
    let raw = crate::commands::session_picker::fetch_managed_raw(client, url, None)
        .await
        .expect("fetch raw");
    crate::commands::session_picker::parse_managed_sessions(&raw).expect("parse")
}

/// Scope a list the way a DEFAULT (non-`--all`) `tm ls` does, given an EMPTY
/// dead set — i.e. exactly the pre-#4994 filter: lifecycle state only.
///
/// 🔴 The empty set is the point, not a shortcut. `seed_dead_session` fixtures
/// carry `unresumable: true`, so a filter keyed on that flag would drop the
/// record whether or not the prune ever reached the daemon, and the three
/// registry-teardown assertions below would pass under a
/// `decommission_dead_record` that contacts nothing (critic HIGH, PR #4995
/// review). With no dead ids the ONLY thing that can hide the record is the
/// daemon actually flipping it to `decommissioned` — which is what those
/// assertions exist to prove.
fn default_view_pre_4994(
    sessions: Vec<trusty_mpm::client::ManagedSessionSummary>,
) -> Vec<trusty_mpm::client::ManagedSessionSummary> {
    crate::commands::session_picker::scope_for_display(
        sessions,
        false,
        &std::collections::HashSet::new(),
    )
}

/// A FIRST sighting of an `unresumable` record is never pruned — only
/// recorded as a candidate (critic HIGH finding #1: no single-observation
/// destructive action).
#[tokio::test]
async fn auto_prune_dead_records_first_sighting_is_not_pruned() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );
    let mgr = state.session_manager().await;
    let id = seed_dead_session(&mgr, root.path(), "first").await;

    let server = LocalTestServer::spawn(state).await;
    let client = reqwest::Client::new();
    let marker_path = root.path().join("seen.json");

    let sessions = fetch_raw_live(&client, &server.url).await;
    let target = sessions
        .iter()
        .find(|s| s.id == id.to_string())
        .expect("dead session must still surface on the raw fetch");
    assert!(target.unresumable, "seeded session must be unresumable");

    let outcome = auto_prune_dead_records_at(
        &client,
        &server.url,
        sessions,
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(outcome.pruned, 0, "a first sighting must never be pruned");
    assert_eq!(outcome.pending, 1, "a first sighting is reported pending");
    assert!(
        outcome.kept.iter().any(|s| s.id == id.to_string()),
        "the record must remain visible while awaiting confirmation"
    );

    // The daemon-side record itself must be untouched — still Errored, not
    // Decommissioned.
    let still_there = fetch_raw_live(&client, &server.url).await;
    let record = still_there
        .iter()
        .find(|s| s.id == id.to_string())
        .expect("record must still be live");
    assert_eq!(record.state, "errored");
}

/// A record confirmed dead on a SECOND (later) call is decommissioned via the
/// record-only route, and the daemon's own store reflects the teardown
/// (state flips to `decommissioned`, which the default live view hides) —
/// never a client-side-only illusion of removal.
#[tokio::test]
async fn auto_prune_dead_records_removes_confirmed_unresumable_records() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );
    let mgr = state.session_manager().await;
    let id = seed_dead_session(&mgr, root.path(), "confirmed").await;

    let server = LocalTestServer::spawn(state).await;
    let client = reqwest::Client::new();
    let marker_path = root.path().join("seen.json");

    // First call: first sighting, not yet acted on.
    let sessions = fetch_raw_live(&client, &server.url).await;
    let first = auto_prune_dead_records_at(
        &client,
        &server.url,
        sessions,
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(first.pruned, 0);

    // #4702: an immediate second call must NOT confirm — the sighting window
    // is time-based, not call-count-based.
    let sessions = fetch_raw_live(&client, &server.url).await;
    let immediate = auto_prune_dead_records_at(
        &client,
        &server.url,
        sessions,
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        immediate.pruned, 0,
        "a second call in the same instant must not confirm — the window is elapsed TIME"
    );

    // Second call after the window has genuinely elapsed: now CONFIRMED.
    backdate_sightings(&marker_path, 11);
    let sessions = fetch_raw_live(&client, &server.url).await;
    let second = auto_prune_dead_records_at(
        &client,
        &server.url,
        sessions,
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        second.pruned, 1,
        "confirmed on a second listing must be pruned"
    );
    assert_eq!(second.pending, 0);
    assert!(
        !second.kept.iter().any(|s| s.id == id.to_string()),
        "the pruned record must not remain in the returned list"
    );

    // The daemon's own record must have been torn down for real (not merely
    // dropped client-side) — decommission tombstones it, which the default
    // view hides.
    let refetched = default_view_pre_4994(fetch_raw_live(&client, &server.url).await);
    assert!(
        !refetched.iter().any(|s| s.id == id.to_string()),
        "the decommissioned record must no longer appear in the default listing"
    );
}

/// #4344 boundary: a record whose worktree removal was refused because the
/// tree was dirty RETAINS `workspace_path` pointing at a directory that still
/// genuinely exists on disk — such a record must NEVER be flagged
/// `unresumable` (the disk-existence probe finds it present) and therefore
/// must never be auto-pruned. This test stands in for that retained-path
/// shape directly: a stopped session whose `workspace_path` is a REAL,
/// still-existing directory.
#[tokio::test]
async fn auto_prune_dead_records_keeps_workspace_present_records() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );
    let mgr = state.session_manager().await;

    let ws = root.path().join("still-here-workspace");
    std::fs::create_dir_all(&ws).expect("create real workspace dir");
    let id = trusty_mpm::session_manager::ManagedSessionId::new();
    mgr.create_with_id(
        id,
        "regression: auto-prune keeps live workspace".to_string(),
        Some(ws.clone()),
        None,
        Some(ws),
        Some("https://example.com/r.git".to_string()),
        Some("main".to_string()),
        trusty_mpm::runtime::RuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session with real workspace");
    mgr.stop(&id)
        .await
        .expect("stop session (workspace intact)");

    let server = LocalTestServer::spawn(state).await;
    let client = reqwest::Client::new();
    let marker_path = root.path().join("seen.json");
    let sessions = fetch_raw_live(&client, &server.url).await;
    let target = sessions
        .iter()
        .find(|s| s.id == id.to_string())
        .expect("stopped session with a real workspace must surface");
    assert!(
        !target.unresumable,
        "a stopped session whose workspace still exists on disk must never \
         be flagged unresumable"
    );

    let outcome = auto_prune_dead_records_at(
        &client,
        &server.url,
        sessions,
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        outcome.pruned, 0,
        "a record with an existing workspace must never be pruned"
    );
    assert_eq!(outcome.pending, 0);
    assert!(
        outcome.kept.iter().any(|s| s.id == id.to_string()),
        "the record must survive auto-prune untouched"
    );
}

/// A list containing no `unresumable` records is a pure no-op — no HTTP call
/// is even attempted (an unroutable URL would otherwise surface as an error,
/// but the function never reaches it).
#[tokio::test]
async fn auto_prune_dead_records_is_noop_when_nothing_is_dead() {
    let client = reqwest::Client::new();
    let marker_path = tempfile::TempDir::new()
        .expect("tempdir")
        .path()
        .join("seen.json");
    let sessions = vec![ls_test_session("healthy", "active", None, None, None, None)];

    let outcome = auto_prune_dead_records_at(
        &client,
        "http://127.0.0.1:1",
        sessions,
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(outcome.pruned, 0);
    assert_eq!(outcome.pending, 0);
    assert_eq!(outcome.kept.len(), 1);
    assert_eq!(outcome.kept[0].name, "healthy");
}

/// The per-call cap (critic HIGH finding #1) limits a single call to at most
/// 5 decommissions even when MORE records are confirmed-eligible — a
/// bad-mount day must not mass-tombstone an entire fleet in one `tm ls`.
#[tokio::test]
async fn auto_prune_dead_records_honors_the_cap() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );
    let mgr = state.session_manager().await;
    for i in 0..6 {
        seed_dead_session(&mgr, root.path(), &format!("cap-{i}")).await;
    }

    let server = LocalTestServer::spawn(state).await;
    let client = reqwest::Client::new();
    let marker_path = root.path().join("seen.json");

    // First call: all 6 are first sightings — nothing pruned yet.
    let sessions = fetch_raw_live(&client, &server.url).await;
    assert_eq!(sessions.len(), 6);
    let first = auto_prune_dead_records_at(
        &client,
        &server.url,
        sessions,
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(first.pruned, 0);
    assert_eq!(first.pending, 6);

    // Second call, once the sighting window has genuinely elapsed: all 6 are
    // CONFIRMED, but the cap limits this call to 5.
    backdate_sightings(&marker_path, 11);
    let sessions = fetch_raw_live(&client, &server.url).await;
    assert_eq!(sessions.len(), 6);
    let second = auto_prune_dead_records_at(
        &client,
        &server.url,
        sessions,
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(second.pruned, 5, "the cap must limit a single call to 5");
    assert_eq!(
        second.pending, 1,
        "the 6th confirmed record must be reported pending, not silently dropped"
    );
    assert_eq!(second.kept.len(), 1);
}

/// Critic CRITICAL (2026-07-30 re-review): a STALE daemon that silently
/// ignores `?record_only=true` (no `Query<DecommissionQuery>` extractor —
/// exactly what a pre-this-fix build looks like) and reports
/// `workspace_removed: true` must NEVER be counted as a safe prune, and the
/// sweep must stop attempting further decommissions for the rest of this
/// call (and any later call sharing the same marker file).
///
/// Why: an HTTP 200 alone cannot distinguish "genuinely record-only" from
/// "an old daemon ran the full destructive teardown anyway" — both return
/// 200. Only the response body's `workspace_removed` field can, so this test
/// drives a STUB server (not the real daemon/SessionManager) that mimics the
/// old handler exactly: it accepts the POST, ignores every query param, and
/// always reports `workspace_removed: true`.
/// What: two CONFIRMED-dead session ids hit the stub in one call. The first
/// one's stale response must trip the stop; the second must never even be
/// attempted (it survives in `kept` untouched, `pruned` stays 0 for both).
#[tokio::test]
async fn auto_prune_dead_records_stops_sweep_when_daemon_reports_workspace_removed() {
    // Stub daemon: mimics a PRE-#4384 build — no `Query<DecommissionQuery>`
    // extractor on the route at all, so `record_only` is silently dropped by
    // axum; always answers with the OLD full-teardown shape.
    async fn stub_decommission(
        axum::extract::Path(_id): axum::extract::Path<String>,
    ) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({ "workspace_removed": true }))
    }
    let app = axum::Router::new().route(
        "/api/v1/sessions/managed/{id}/decommission",
        axum::routing::post(stub_decommission),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback port");
    let addr = listener.local_addr().expect("resolve bound addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let url = format!("http://{addr}");

    let client = reqwest::Client::new();
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");

    // Pre-seed BOTH ids as already-confirmed (first-sighted on a prior,
    // separate call) so this test isolates the stale-daemon STOP behavior
    // from the two-sightings confirmation logic covered elsewhere.
    let mut seen = std::collections::HashMap::new();
    seen.insert("id-a".to_string(), "2020-01-01T00:00:00Z".to_string());
    seen.insert("id-b".to_string(), "2020-01-01T00:00:00Z".to_string());
    std::fs::write(&marker_path, serde_json::to_string(&seen).unwrap()).unwrap();

    // #4702: both must carry a genuinely-absent workspace under an EXISTING
    // parent — the client's own verification is now required for every record,
    // so a bare `unresumable: true` no longer makes one a candidate.
    let mut a = stopped_session_at("id-a", &root.path().join("gone-a"));
    a.state = "errored".to_string();
    a.unresumable = true;
    let mut b = stopped_session_at("id-b", &root.path().join("gone-b"));
    b.state = "errored".to_string();
    b.unresumable = true;

    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![a, b],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        outcome.pruned, 0,
        "a stale daemon's workspace_removed=true must never count as pruned"
    );
    assert_eq!(
        outcome.kept.len(),
        2,
        "both records must be left on the manual [d<N>] path once the stale \
         daemon is detected"
    );
    assert_eq!(
        outcome.pending, 2,
        "both stale-daemon-held records must fold into `pending`, not vanish \
         from the reported count"
    );

    handle.abort();
}

/// Owner request 2026-07-30 follow-up: the stale-daemon sentinel expires
/// after its 1-hour TTL, so a daemon that gets restarted eventually stops
/// being wedged out of auto-prune forever. A FRESH sentinel still blocks;
/// an EXPIRED one lets the next call retry (and, since this stub daemon is
/// still "stale," immediately re-trips a fresh sentinel — correct
/// oscillation, one probe per hour, never a permanent lockout).
#[tokio::test]
async fn auto_prune_dead_records_stale_daemon_sentinel_expires_after_ttl() {
    async fn stub_decommission(
        axum::extract::Path(_id): axum::extract::Path<String>,
    ) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({ "workspace_removed": true }))
    }
    let app = axum::Router::new().route(
        "/api/v1/sessions/managed/{id}/decommission",
        axum::routing::post(stub_decommission),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback port");
    let addr = listener.local_addr().expect("resolve bound addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let url = format!("http://{addr}");

    let client = reqwest::Client::new();
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");

    // #4702: a candidate needs a verifiably-absent workspace under an existing
    // parent — `unresumable` alone is no longer sufficient.
    let gone = root.path().join("gone-ttl");
    let confirmed_session = || {
        let mut s = stopped_session_at("id-a", &gone);
        s.state = "errored".to_string();
        s.unresumable = true;
        s
    };

    // A FRESH sentinel (just now) must block — no decommission attempted.
    let mut seen = std::collections::HashMap::new();
    seen.insert("id-a".to_string(), "2020-01-01T00:00:00Z".to_string());
    seen.insert(
        "__stale_daemon_detected__".to_string(),
        chrono::Utc::now().to_rfc3339(),
    );
    std::fs::write(&marker_path, serde_json::to_string(&seen).unwrap()).unwrap();

    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![confirmed_session()],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(outcome.pruned, 0, "a fresh sentinel must still block");
    assert_eq!(outcome.kept.len(), 1);
    assert_eq!(outcome.pending, 1, "held record must fold into pending");

    // An EXPIRED sentinel (well past the 1-hour TTL) must let this call
    // retry. This stub always reports `workspace_removed: true`, so the
    // retry immediately re-trips a FRESH sentinel — proving both halves:
    // expiry allows a retry, and a still-stale daemon re-blocks right away.
    let mut seen = std::collections::HashMap::new();
    seen.insert("id-a".to_string(), "2020-01-01T00:00:00Z".to_string());
    seen.insert(
        "__stale_daemon_detected__".to_string(),
        "2020-01-01T00:00:00Z".to_string(),
    );
    std::fs::write(&marker_path, serde_json::to_string(&seen).unwrap()).unwrap();

    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![confirmed_session()],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        outcome.pruned, 0,
        "the stub is still stale, so the retry must not count as pruned"
    );
    assert_eq!(outcome.kept.len(), 1);

    // The re-trip must have rewritten a FRESH sentinel — verified directly
    // against the persisted marker file, not merely inferred from behavior.
    let raw = std::fs::read_to_string(&marker_path).expect("read marker file");
    let persisted: std::collections::HashMap<String, String> =
        serde_json::from_str(&raw).expect("parse marker file");
    let sentinel_ts = persisted
        .get("__stale_daemon_detected__")
        .expect("sentinel must be re-written after retry");
    let parsed = chrono::DateTime::parse_from_rfc3339(sentinel_ts).expect("valid RFC 3339");
    assert!(
        chrono::Utc::now().signed_duration_since(parsed) < chrono::Duration::minutes(1),
        "the re-tripped sentinel must be freshly timestamped, not the expired one"
    );

    handle.abort();
}

/// 🔴 #4995 review (MEDIUM): every confirmed record's decommission FAILS, and
/// the operator must still be told the rows were withheld.
///
/// Why: a failed decommission leaves the record in `kept` AND in `dead_ids`, so
/// the default `tm ls` hides it. Until this fix it was counted in neither
/// `pruned` nor `pending`, so a call where every attempt failed reported
/// `pending == 0`, [`pending_banner`] returned `None`, and the rows dropped off
/// the listing with nothing said — silent hiding arriving through the error
/// path, which is the outcome #4994's whole design exists to prevent.
///
/// Fails before the fix: `pending` is 0, so the banner assertion gets `None`.
/// What: a stub daemon that answers every decommission with HTTP 500 — the
/// transport-level failure arm, distinct from the `workspace_removed: true`
/// stale-daemon arm its sibling tests drive. Both ids are pre-seeded as already
/// confirmed so this isolates the counting behavior from the two-sightings
/// window.
#[tokio::test]
async fn auto_prune_counts_failed_decommissions_so_the_banner_still_prints() {
    use crate::commands::managed::filter_live_sessions;
    use crate::commands::session_picker_prune::pending_banner;

    let app = axum::Router::new().route(
        "/api/v1/sessions/managed/{id}/decommission",
        axum::routing::post(
            async |axum::extract::Path(_id): axum::extract::Path<String>| {
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback port");
    let addr = listener.local_addr().expect("resolve bound addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let url = format!("http://{addr}");

    let client = reqwest::Client::new();
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");

    let mut seen = std::collections::HashMap::new();
    seen.insert("id-a".to_string(), "2020-01-01T00:00:00Z".to_string());
    seen.insert("id-b".to_string(), "2020-01-01T00:00:00Z".to_string());
    std::fs::write(&marker_path, serde_json::to_string(&seen).unwrap()).unwrap();

    // Both workspaces are absent under an EXISTING parent (the tempdir), which
    // is what `workspace_verified_gone` requires to call a record dead.
    let dead_session = |id: &str, label: &str| {
        let mut s = stopped_session_at(id, &root.path().join(label));
        s.state = "errored".to_string();
        s.unresumable = true;
        s
    };

    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![
            dead_session("id-a", "gone-a"),
            dead_session("id-b", "gone-b"),
        ],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;

    assert_eq!(
        outcome.pruned, 0,
        "an HTTP 500 must never be counted as a successful prune"
    );
    assert_eq!(
        outcome.kept.len(),
        2,
        "both records survive the failed sweep"
    );
    let mut hidden: Vec<&str> = outcome.dead_ids.iter().map(String::as_str).collect();
    hidden.sort_unstable();
    assert_eq!(
        hidden,
        vec!["id-a", "id-b"],
        "both records stay classified dead, so the default view hides both"
    );

    // 🔴 The defect itself: the banner is the only notice these rows were
    // withheld, and `pending` is its sole gate.
    assert_eq!(
        pending_banner(outcome.pending, true).as_deref(),
        Some("tm: 2 more dead records pending confirmation (hidden; `tm ls --all` to see them)"),
        "a call whose every decommission failed must still name the withheld \
         rows — before this fix `pending` was 0 and nothing printed at all"
    );
    assert_eq!(
        outcome.pending,
        outcome.dead_ids.len(),
        "the reported count and the hidden set must be the same rows"
    );

    // And they really are hidden: the count above is not describing rows that
    // stayed on screen.
    assert!(
        filter_live_sessions(outcome.kept, &outcome.dead_ids).is_empty(),
        "the default view drops exactly the rows the banner just accounted for"
    );

    handle.abort();
}

// ── #4702: prune coverage — every invocation, and stopped records ───────────
//
// Two independent narrowings let dead records accumulate without bound before
// #4702, and each gets its own group below:
//   1. the prune fired only from the TTY picker, so `managed::session_ls` (every
//      piped / scripted / `--json` listing) never pruned at all;
//   2. it acted only on the daemon-computed `unresumable` flag, which is
//      computed ONLY for records whose PERSISTED state is `Stopped`/`Errored` —
//      a zombie (persisted `Active`, pane gone) is display-reconciled to
//      `stopped` and reads `unresumable == false` forever.
//
// Every test here injects a tempdir marker path, never the operator's real
// `~/.trusty-mpm/auto-prune-seen.json`.

/// Build a `stopped` summary whose two wire-visible workdir candidates
/// (`workspace_path`, `cwd`) both point at `workdir`, with `unresumable`
/// deliberately `false` — i.e. exactly the zombie shape the daemon never
/// probes. `#4702`.
fn stopped_session_at(
    id: &str,
    workdir: &std::path::Path,
) -> trusty_mpm::client::ManagedSessionSummary {
    let mut s = ls_test_session(id, "stopped", None, None, None, None);
    s.id = id.to_string();
    s.workspace_path = Some(workdir.to_string_lossy().to_string());
    s.cwd = Some(workdir.to_string_lossy().to_string());
    s.unresumable = false;
    s
}

/// Spawn a stub daemon that answers every decommission POST honestly
/// (`workspace_removed: false`, the record-only shape) and records the query
/// string it was called with. Returns `(url, captured, task handle)`.
async fn spawn_recording_decommission_stub() -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let captured: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = captured.clone();
    let app = axum::Router::new().route(
        "/api/v1/sessions/managed/{id}/decommission",
        axum::routing::post(
            move |axum::extract::Path(id): axum::extract::Path<String>,
                  axum::extract::RawQuery(q): axum::extract::RawQuery| {
                let sink = sink.clone();
                async move {
                    sink.lock()
                        .expect("capture lock")
                        .push(format!("{id}?{}", q.unwrap_or_default()));
                    axum::Json(serde_json::json!({ "workspace_removed": false }))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback port");
    let addr = listener.local_addr().expect("resolve bound addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), captured, handle)
}

/// Defect 2 (#4702): a `stopped` record whose workspace is INDEPENDENTLY
/// verified gone is cleared, even though the daemon never flagged it
/// `unresumable` (that flag is computed only for records whose PERSISTED state
/// is stopped/errored — a display-reconciled zombie never gets probed).
///
/// Fails before #4702: the partition keyed on `s.unresumable` alone, so this
/// record was never even a candidate.
#[tokio::test]
async fn auto_prune_clears_stopped_record_whose_workspace_is_gone() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("workspace-that-never-existed");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    // First sighting records a candidate but never acts.
    let first = auto_prune_dead_records_at(
        &client,
        &url,
        vec![stopped_session_at("z1", &gone)],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(first.pruned, 0, "a first sighting must never be pruned");
    assert_eq!(first.pending, 1);
    assert!(
        captured.lock().expect("capture lock").is_empty(),
        "a first sighting must issue no decommission call at all"
    );

    // Second sighting, once the sighting window has genuinely elapsed.
    backdate_sightings(&marker_path, 11);
    let second = auto_prune_dead_records_at(
        &client,
        &url,
        vec![stopped_session_at("z1", &gone)],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        second.pruned, 1,
        "a confirmed stopped record with a verifiably absent workspace must be cleared"
    );
    assert!(second.kept.is_empty());

    handle.abort();
}

/// The safety half of defect 2 (#4702): a `stopped` record whose workspace
/// STILL EXISTS is never cleared, no matter how many times it is listed. It may
/// still be resumable, and the record may be the only thing making resumption
/// possible — measured 2026-08-03, 4 of 5 spot-checked stopped workspaces were
/// still on disk.
#[tokio::test]
async fn auto_prune_keeps_stopped_record_whose_workspace_still_exists() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let present = root.path().join("live-workspace");
    std::fs::create_dir_all(&present).expect("create workspace");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    for round in 0..3 {
        let outcome = auto_prune_dead_records_at(
            &client,
            &url,
            vec![stopped_session_at("keeper", &present)],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(
            outcome.pruned, 0,
            "round {round}: a stopped record with a workspace ON DISK must never be cleared"
        );
        assert_eq!(outcome.kept.len(), 1, "round {round}: it must stay listed");
        assert_eq!(
            outcome.pending, 0,
            "round {round}: it is not even a pending candidate"
        );
    }
    assert!(
        captured.lock().expect("capture lock").is_empty(),
        "no decommission call may ever be issued for a live workspace"
    );

    handle.abort();
}

/// An `active` (running) record is never touched, even when its recorded
/// workspace path is absent — liveness, not workspace presence, decides
/// (#4702). `is_running()`'s real tmux probe upstream is what makes a
/// persisted-`stopped`-but-actually-alive session read `active` here.
#[tokio::test]
async fn auto_prune_never_touches_a_running_record() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("nowhere");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let mut live = stopped_session_at("alive", &gone);
    live.state = "active".to_string();

    for _ in 0..2 {
        let outcome = auto_prune_dead_records_at(
            &client,
            &url,
            vec![live.clone()],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(outcome.pruned, 0, "a running record must never be pruned");
        assert_eq!(outcome.kept.len(), 1);
    }
    assert!(captured.lock().expect("capture lock").is_empty());

    handle.abort();
}

/// `decommissioned` records are explicitly OUT of scope for #4702 — 35 exist on
/// the reporting machine and nobody has assessed what discarding them costs.
/// The state gate must exclude them even when their workspace is long gone.
#[tokio::test]
async fn auto_prune_never_touches_a_decommissioned_record() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("nowhere");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let mut tombstone = stopped_session_at("tombstone", &gone);
    tombstone.state = "decommissioned".to_string();
    // Also cover the display/persisted split: a row displayed `stopped` whose
    // PERSISTED state is already terminal.
    let mut persisted_terminal = stopped_session_at("terminal", &gone);
    persisted_terminal.persisted_state = Some("decommissioned".to_string());

    for _ in 0..2 {
        let outcome = auto_prune_dead_records_at(
            &client,
            &url,
            vec![tombstone.clone(), persisted_terminal.clone()],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(outcome.pruned, 0);
        assert_eq!(outcome.kept.len(), 2);
    }
    assert!(
        captured.lock().expect("capture lock").is_empty(),
        "a decommissioned record must never be decommissioned again"
    );

    handle.abort();
}

/// 🔴 The load-bearing safety test (#4702): the prune path must never invoke a
/// worktree removal. `prune_managed(PruneFilter::Stopped, …)` routes through the
/// FULL `decommission()`, which runs `git worktree remove --force` /
/// `fs::remove_dir_all` / `git branch -D`. Auto-prune must not inherit that.
///
/// This asserts on the PATH, not the end state: every decommission request the
/// sweep issues must carry `record_only=true`, which is what routes the daemon
/// to `SessionManager::decommission_record_only` (whose own never-removes
/// guarantee is pinned server-side by
/// `decommission_record_only_never_removes_existing_workspace`). A request
/// without it would silently take the full-teardown branch.
#[tokio::test]
async fn auto_prune_always_requests_record_only_never_full_teardown() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    // One record dead via the daemon's `unresumable` verdict, one via #4702's
    // client-side stopped-workspace-gone probe — both routes must be
    // record-only.
    let mut flagged = stopped_session_at("flagged", &gone);
    flagged.unresumable = true;
    let probed = stopped_session_at("probed", &gone);

    let sessions = vec![flagged.clone(), probed.clone()];
    auto_prune_dead_records_at(&client, &url, sessions, &marker_path, Some(HashSet::new())).await;
    backdate_sightings(&marker_path, 11);
    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![flagged, probed],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(outcome.pruned, 2, "both dead records must be cleared");

    let calls = captured.lock().expect("capture lock").clone();
    assert_eq!(
        calls.len(),
        2,
        "exactly two decommission calls, got {calls:?}"
    );
    for call in &calls {
        assert!(
            call.contains("record_only=true"),
            "every decommission the sweep issues must be RECORD-ONLY — a call \
             without it takes the full worktree-removal teardown: {call}"
        );
    }

    handle.abort();
}

/// Defect 1 (#4702): a piped / scripted / non-TTY `tm ls` prunes. Before the
/// fix this path (`managed::session_ls`) never called the prune at all, so
/// anything not driving the interactive picker accumulated dead records
/// forever.
#[tokio::test]
async fn session_ls_prunes_dead_records_on_piped_invocation() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );
    let mgr = state.session_manager().await;
    let id = seed_dead_session(&mgr, root.path(), "piped").await;

    let server = LocalTestServer::spawn(state).await;
    let client = reqwest::Client::new();

    // First listing records the sighting; the sighting window then elapses; the
    // second clears. The two-sightings guard applies to non-interactive callers
    // exactly as it does to the picker.
    let ls = async |ctx: &crate::commands::session_picker_prune::PruneContext| {
        crate::commands::managed::session_ls_at(
            &client,
            &server.url,
            false, // json = false: the piped/scripted table path
            None,
            false,
            false, // attached = false: unfiltered, as `tm ls` is by default
            crate::commands::session_picker::SessionSortArg::Recent,
            None,
            ctx,
        )
        .await
        .expect("session_ls");
    };

    let ctx = hermetic_prune_ctx(&marker_path);
    ls(&ctx).await;
    assert!(
        fetch_raw_live(&client, &server.url)
            .await
            .iter()
            .any(|s| s.id == id.to_string()),
        "a first sighting must leave the record in place"
    );

    backdate_sightings(&marker_path, 11);
    let ctx = hermetic_prune_ctx(&marker_path);
    ls(&ctx).await;

    let live = default_view_pre_4994(fetch_raw_live(&client, &server.url).await);
    assert!(
        !live.iter().any(|s| s.id == id.to_string()),
        "a non-TTY `tm ls` must clear a confirmed dead record from the registry"
    );
}

/// Defect 1 (#4702), `--json` half: the raw passthrough path prunes too.
#[tokio::test]
async fn session_ls_json_passthrough_prunes_dead_records() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );
    let mgr = state.session_manager().await;
    let id = seed_dead_session(&mgr, root.path(), "json").await;

    let server = LocalTestServer::spawn(state).await;
    let client = reqwest::Client::new();

    let ls_json = async |ctx: &crate::commands::session_picker_prune::PruneContext| {
        crate::commands::managed::session_ls_at(
            &client,
            &server.url,
            true, // json = true
            None,
            false,
            false, // attached = false
            crate::commands::session_picker::SessionSortArg::Recent,
            None,
            ctx,
        )
        .await
        .expect("session_ls --json");
    };

    ls_json(&hermetic_prune_ctx(&marker_path)).await;
    backdate_sightings(&marker_path, 11);
    ls_json(&hermetic_prune_ctx(&marker_path)).await;

    let live = default_view_pre_4994(fetch_raw_live(&client, &server.url).await);
    assert!(
        !live.iter().any(|s| s.id == id.to_string()),
        "`tm ls --json` must clear a confirmed dead record from the registry"
    );
}

/// `--no-prune` makes the listing a pure read (#5950).
///
/// Why: `tm sessions ls --all --json` is a READ verb, and it decommissioned
/// three records during a fleet census without being asked to. The default
/// stays as #4702 set it — the two tests above pin that — but an operator who
/// needs a listing that provably changes nothing now has a flag for it. This
/// is the exact setup of `session_ls_json_passthrough_prunes_dead_records`
/// with `PruneContext::disabled()` swapped in, so the ONLY difference between
/// "record gone" and "record intact" is the flag.
#[tokio::test]
async fn session_ls_no_prune_makes_the_read_non_mutating() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );
    let mgr = state.session_manager().await;
    let id = seed_dead_session(&mgr, root.path(), "no-prune").await;

    let server = LocalTestServer::spawn(state).await;
    let client = reqwest::Client::new();

    let ls_json = async |ctx: &crate::commands::session_picker_prune::PruneContext| {
        crate::commands::managed::session_ls_at(
            &client,
            &server.url,
            true, // json = true: the raw passthrough path
            None,
            false,
            false,
            crate::commands::session_picker::SessionSortArg::Recent,
            None,
            ctx,
        )
        .await
        .expect("session_ls --json --no-prune");
    };

    // Pre-seed the record as already CONFIRMED (sighted long ago), so a single
    // listing with the prune on would clear it. The only thing standing between
    // this record and a tombstone is the flag.
    let mut seen = std::collections::HashMap::new();
    seen.insert(id.to_string(), "2020-01-01T00:00:00Z".to_string());
    std::fs::write(
        &marker_path,
        serde_json::to_string(&seen).expect("serialize marker"),
    )
    .expect("write marker");

    // Identical context to the passing sibling test above, with ONLY the
    // `--no-prune` bit cleared — same marker file, same tmux set.
    ls_json(&hermetic_prune_ctx(&marker_path).without_prune()).await;
    ls_json(&hermetic_prune_ctx(&marker_path).without_prune()).await;

    let live = default_view_pre_4994(fetch_raw_live(&client, &server.url).await);
    assert!(
        live.iter().any(|s| s.id == id.to_string()),
        "`--no-prune` must leave the record live, not tombstoned"
    );
    assert_eq!(
        std::fs::read_to_string(&marker_path).expect("read marker"),
        serde_json::to_string(&seen).expect("serialize marker"),
        "`--no-prune` must not rewrite the confirmation marker either"
    );
}

/// `--no-prune` is what selects that context, and nothing else does (#5950).
#[test]
fn no_prune_flag_selects_the_non_mutating_prune_context() {
    assert!(
        !crate::commands::session_picker_prune::PruneContext::disabled().enabled,
        "--no-prune must disable the sweep"
    );
    assert!(
        crate::commands::session_picker_prune::PruneContext::production().enabled,
        "the default listing keeps the #4702 auto-prune"
    );
}

// ── PR #4725 review — the five required changes ────────────────────────────

/// Change 3 (PR #4725 review): an UNREACHABLE parent directory reads
/// `Ok(false)` from `try_exists`, not `Err`. Without the parent check, unplugging
/// an external volume makes every session on it "verified gone" and one `tm ls`
/// tombstones the lot.
///
/// What: the record's workspace sits under a directory that does not exist —
/// standing in for `/Volumes/<unmounted>` — so BOTH the leaf and its parent
/// probe absent. Even across many listings with the sighting window fully
/// elapsed, nothing may be pruned and no decommission call may be issued.
#[tokio::test]
async fn auto_prune_keeps_record_whose_parent_directory_is_unreachable() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    // Neither `unmounted-volume` nor the leaf below it exists.
    let unreachable = root
        .path()
        .join("unmounted-volume")
        .join("work")
        .join("proj");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    for round in 0..3 {
        let outcome = auto_prune_dead_records_at(
            &client,
            &url,
            vec![stopped_session_at("on-the-volume", &unreachable)],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(
            outcome.pruned, 0,
            "round {round}: an unreachable parent is NOT proof the workspace is gone"
        );
        if marker_path.exists() {
            backdate_sightings(&marker_path, 11);
        }
    }
    assert!(
        captured.lock().expect("capture lock").is_empty(),
        "no decommission may be issued for a path whose parent is unreachable"
    );

    handle.abort();
}

/// #4872: `git worktree remove` deletes the leaf, its parent AND its
/// grandparent in one operation, so the parent-only probe left four dead records
/// permanently stuck — never pruned, and never even marked in
/// `auto-prune-seen.json`, so repeated `tm ls` could not advance them.
///
/// What: the two shapes actually observed on 2026-08-05 —
/// `.claude/worktrees/critic-4850/crates/trusty-mpm` and
/// `.base/.worktrees/qa-4850/crates/trusty-mpm` — with the worktree ROOT
/// (`.claude/worktrees`, `.base/.worktrees`) still present and everything below
/// it removed. That root surviving is what proves the removal was a deletion,
/// not an unmount.
///
/// Fails before the fix: `workspace_verified_gone` returned `false` because the
/// immediate parent (`crates/`) was gone too, so `pruned` stayed 0 forever.
#[tokio::test]
async fn auto_prune_clears_record_whose_worktree_root_survived_the_removal() {
    for (marker_dir, container, worktree) in [
        (".claude", "worktrees", "critic-4850"),
        (".base", ".worktrees", "qa-4850"),
    ] {
        let root = tempfile::TempDir::new().expect("tempdir");
        let marker_path = root.path().join("seen.json");
        let worktree_root = root.path().join(marker_dir).join(container);
        std::fs::create_dir_all(&worktree_root).expect("create worktree root");
        // The worktree itself, and every level under it, is gone.
        let removed = worktree_root
            .join(worktree)
            .join("crates")
            .join("trusty-mpm");
        let (url, captured, handle) = spawn_recording_decommission_stub().await;
        let client = reqwest::Client::new();

        let first = auto_prune_dead_records_at(
            &client,
            &url,
            vec![stopped_session_at("tm-deadrt40493-01", &removed)],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(first.pruned, 0, "{container}: first sighting never prunes");
        assert_eq!(
            first.pending, 1,
            "{container}: the record must be RECORDED as dead — a record that \
             never reaches `pending` never gets a marker, which is why #4872's \
             four records could not advance across repeated `tm ls` calls"
        );

        backdate_sightings(&marker_path, 11);
        let second = auto_prune_dead_records_at(
            &client,
            &url,
            vec![stopped_session_at("tm-deadrt40493-01", &removed)],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(
            second.pruned, 1,
            "{container}: a removed worktree whose ROOT still exists is \
             verifiably deleted, not unmounted, and must be cleared"
        );
        assert!(second.kept.is_empty(), "{container}: nothing left to keep");
        assert_eq!(
            captured.lock().expect("capture lock").len(),
            1,
            "{container}: exactly one decommission call"
        );

        handle.abort();
    }
}

/// #4872, the safety half: the guard's ORIGINAL purpose must survive the fix. An
/// unmounted volume answers `Ok(false)` for every path under it, including the
/// worktree root — so nothing is corroborated and nothing may be cleared.
///
/// What: the same two worktree shapes, but the whole checkout sits under a
/// directory that does not exist. The tempdir ABOVE it does exist, standing in
/// for `/Volumes` — which survives every unmount. A fix that settled for "some
/// ancestor exists" would pass the sibling test above and mass-tombstone here;
/// this test is what rules that implementation out.
#[tokio::test]
async fn auto_prune_keeps_record_when_the_worktree_root_itself_is_gone() {
    for (marker_dir, container, worktree) in [
        (".claude", "worktrees", "critic-4850"),
        (".base", ".worktrees", "qa-4850"),
    ] {
        let root = tempfile::TempDir::new().expect("tempdir");
        let marker_path = root.path().join("seen.json");
        // `root` exists; `unmounted-volume` and everything below it does not.
        let removed = root
            .path()
            .join("unmounted-volume")
            .join(marker_dir)
            .join(container)
            .join(worktree)
            .join("crates")
            .join("trusty-mpm");
        let (url, captured, handle) = spawn_recording_decommission_stub().await;
        let client = reqwest::Client::new();

        for round in 0..3 {
            let outcome = auto_prune_dead_records_at(
                &client,
                &url,
                vec![stopped_session_at("on-the-volume", &removed)],
                &marker_path,
                Some(HashSet::new()),
            )
            .await;
            assert_eq!(
                outcome.pruned, 0,
                "{container} round {round}: an absent worktree ROOT means the \
                 filesystem is unreachable, not that the worktree was removed"
            );
            if marker_path.exists() {
                backdate_sightings(&marker_path, 11);
            }
        }
        assert!(
            captured.lock().expect("capture lock").is_empty(),
            "{container}: no decommission may be issued for an unreachable volume"
        );

        handle.abort();
    }
}

/// Change 3, second half (PR #4725 review): the daemon's `unresumable` verdict
/// alone must NOT clear a record whose parent is unreachable.
///
/// Why: `is_unresumable` probes with a bare `try_exists`, so it flags an entire
/// unmounted volume's sessions dead. A `if s.unresumable { return true }`
/// short-circuit in `is_dead_record` would route exactly that past the parent
/// check the sibling test above pins. This is why no such short-circuit exists.
#[tokio::test]
async fn auto_prune_ignores_daemon_unresumable_when_parent_is_unreachable() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let unreachable = root.path().join("unmounted-volume").join("proj");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let mut flagged = stopped_session_at("daemon-says-dead", &unreachable);
    flagged.unresumable = true; // the daemon's (mistaken) verdict

    for _ in 0..3 {
        let outcome = auto_prune_dead_records_at(
            &client,
            &url,
            vec![flagged.clone()],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(
            outcome.pruned, 0,
            "the daemon's unresumable flag must not override the client's own verification"
        );
        if marker_path.exists() {
            backdate_sightings(&marker_path, 11);
        }
    }
    assert!(captured.lock().expect("capture lock").is_empty());

    handle.abort();
}

/// Change 4 (PR #4725 review): confirmation requires ELAPSED TIME, not merely a
/// second call. `seen.contains_key(id)` was presence-only, so two `tm ls` calls
/// milliseconds apart confirmed and `AUTO_PRUNE_CAP` stopped being a rate limit.
#[tokio::test]
async fn auto_prune_does_not_confirm_two_calls_in_quick_succession() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    // Ten back-to-back listings, standing in for a tight scripted loop.
    for round in 0..10 {
        let outcome = auto_prune_dead_records_at(
            &client,
            &url,
            vec![stopped_session_at("hasty", &gone)],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(
            outcome.pruned, 0,
            "round {round}: repeated calls inside the sighting window must never confirm"
        );
    }
    assert!(
        captured.lock().expect("capture lock").is_empty(),
        "a tight loop must not be able to drive any decommission at all"
    );

    handle.abort();
}

/// 🔴 PR #4725 review round 2, HIGH: a listing inside the window must NOT
/// restamp the sighting.
///
/// Why: the fix restored a time bound but rewrote the stamp with `now` on every
/// call that did not confirm. That made the window measure the gap between
/// CONSECUTIVE LISTINGS rather than age since first sighting, so any `tm ls`
/// cadence tighter than `SIGHTING_MIN_AGE_SECS` reset the clock forever and
/// auto-prune could never fire — permanently inert on the very machine #4702
/// was filed from. No test caught it because the `backdate_sightings` helper
/// overwrites the stamp after each call, masking the restamp.
///
/// What: seeds a sighting 6 minutes old (inside the 10-minute window), runs a
/// listing, and asserts the PERSISTED STAMP IS UNCHANGED — the mechanism, not
/// just the "not pruned" outcome, which the buggy code also satisfied. Uses no
/// backdating helper for exactly that reason.
#[tokio::test]
async fn auto_prune_does_not_restamp_a_sighting_still_inside_the_window() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let seeded = (chrono::Utc::now() - chrono::Duration::minutes(6)).to_rfc3339();
    let mut seen = std::collections::HashMap::new();
    seen.insert("midwindow".to_string(), seeded.clone());
    std::fs::write(&marker_path, serde_json::to_string(&seen).unwrap()).unwrap();

    // A listing at T+6: too recent to confirm, and it must leave the clock alone.
    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![stopped_session_at("midwindow", &gone)],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        outcome.pruned, 0,
        "6 minutes is inside the 10-minute window"
    );

    let raw = std::fs::read_to_string(&marker_path).expect("read marker");
    let persisted: std::collections::HashMap<String, String> =
        serde_json::from_str(&raw).expect("parse marker");
    assert_eq!(
        persisted.get("midwindow"),
        Some(&seeded),
        "the ORIGINAL first-sighting stamp must survive an intervening listing — \
         restamping makes the window measure listing cadence, not age, and any \
         `tm ls` more often than every 10 minutes then leaves auto-prune inert"
    );
    assert!(captured.lock().expect("capture lock").is_empty());

    handle.abort();
}

/// The behavioral consequence of the restamp bug: a record dead longer than the
/// window still confirms even though it was listed repeatedly in between (PR
/// #4725 review round 2).
///
/// What: reproduces the real cadence. A record is first sighted, listed again
/// 6 minutes later (inside the window), and only then reaches 11 minutes of
/// AGE. Wall-clock advance is simulated by aging the stamp RELATIVE TO WHATEVER
/// IS PERSISTED — never by overwriting it with a fixed value — so a restamp
/// during the intervening listing carries through and the record never confirms,
/// exactly as it would on a real machine.
#[tokio::test]
async fn auto_prune_confirms_despite_frequent_intervening_listings() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, _captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let listing = async |marker: &std::path::Path| {
        auto_prune_dead_records_at(
            &client,
            &url,
            vec![stopped_session_at("patient", &gone)],
            marker,
            Some(HashSet::new()),
        )
        .await
        .pruned
    };

    // T+0: first sighting.
    assert_eq!(listing(&marker_path).await, 0);
    // Simulate 6 minutes passing, then a listing at T+6 (inside the window).
    age_sightings_by(&marker_path, 6);
    assert_eq!(
        listing(&marker_path).await,
        0,
        "a listing 6 minutes in must not confirm"
    );
    // Simulate 5 more minutes. The record is now 11 minutes old — UNLESS the
    // intervening listing restamped it, which is the defect.
    age_sightings_by(&marker_path, 5);
    assert_eq!(
        listing(&marker_path).await,
        1,
        "at 11 minutes of real age the record must confirm — a listing at T+6 \
         must not have reset the clock"
    );

    handle.abort();
}

/// 🔴 PR #4725 review round 2, HIGH: an `errored` record whose pane is LIVE but
/// DETACHED must never be tombstoned.
///
/// Why: `reconcile_live_state` only rewrites `state` for records persisted
/// `Active`/`Stopped`, so an errored row keeps its persisted state even with a
/// live pane, and `attached` is false when no client is attached — leaving a
/// running agent eligible for a TERMINAL `Decommissioned` tombstone that removes
/// it from resume, reattach, and the picker. Client-side tmux enumeration gives
/// the missing signal with no daemon change.
///
/// What: an errored, unresumable record with a vanished workspace — every other
/// condition for clearing is met — whose tmux name appears in the injected live
/// set. It must survive every listing.
#[tokio::test]
async fn auto_prune_never_touches_an_errored_record_with_a_live_detached_pane() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let mut errored = stopped_session_at("tm-live-agent-01", &gone);
    errored.state = "errored".to_string();
    errored.unresumable = true;
    errored.attached = false; // live but DETACHED — the hole `attached` missed

    let live: HashSet<String> = ["tm-live-agent-01".to_string()].into_iter().collect();

    for round in 0..3 {
        let outcome = auto_prune_dead_records_at(
            &client,
            &url,
            vec![errored.clone()],
            &marker_path,
            Some(live.clone()),
        )
        .await;
        assert_eq!(
            outcome.pruned, 0,
            "round {round}: a live tmux session must never be tombstoned into a \
             terminal state, detached or not"
        );
        if marker_path.exists() {
            backdate_sightings(&marker_path, 11);
        }
    }
    assert!(captured.lock().expect("capture lock").is_empty());

    handle.abort();
}

/// When tmux cannot be enumerated at all, prune nothing (PR #4725 review round
/// 2) — mirrors `reconcile_against_tmux`'s fail-closed contract. Without a
/// liveness signal, clearing an `errored` record could tombstone a running
/// agent.
#[tokio::test]
async fn auto_prune_prunes_nothing_when_tmux_cannot_be_enumerated() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let mut seen = std::collections::HashMap::new();
    seen.insert(
        "confirmed-but-blind".to_string(),
        (chrono::Utc::now() - chrono::Duration::minutes(30)).to_rfc3339(),
    );
    std::fs::write(&marker_path, serde_json::to_string(&seen).unwrap()).unwrap();

    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![stopped_session_at("confirmed-but-blind", &gone)],
        &marker_path,
        None, // tmux enumeration failed
    )
    .await;
    assert_eq!(
        outcome.pruned, 0,
        "no liveness signal means no clearing, even for a long-confirmed record"
    );
    assert_eq!(outcome.kept.len(), 1);
    assert!(captured.lock().expect("capture lock").is_empty());

    handle.abort();
}

/// Change 4, the other half: once the window HAS elapsed, confirmation works —
/// the time bound must not wedge auto-prune off permanently.
#[tokio::test]
async fn auto_prune_confirms_once_the_sighting_window_has_elapsed() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, _captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    auto_prune_dead_records_at(
        &client,
        &url,
        vec![stopped_session_at("patient", &gone)],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    backdate_sightings(&marker_path, 11);
    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![stopped_session_at("patient", &gone)],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        outcome.pruned, 1,
        "an elapsed sighting window must confirm — the bound is a delay, not a lockout"
    );

    handle.abort();
}

/// A corrupt/unparseable first-sighting timestamp must RESTART the window, never
/// satisfy it (PR #4725 review, change 4).
#[tokio::test]
async fn auto_prune_treats_an_unparseable_sighting_as_a_fresh_one() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let mut seen = std::collections::HashMap::new();
    seen.insert("corrupt".to_string(), "not-a-timestamp".to_string());
    std::fs::write(&marker_path, serde_json::to_string(&seen).unwrap()).unwrap();

    let outcome = auto_prune_dead_records_at(
        &client,
        &url,
        vec![stopped_session_at("corrupt", &gone)],
        &marker_path,
        Some(HashSet::new()),
    )
    .await;
    assert_eq!(
        outcome.pruned, 0,
        "an unparseable sighting must never count as confirmed"
    );
    assert!(captured.lock().expect("capture lock").is_empty());

    // …and it must have been REWRITTEN with a parseable stamp, so the window
    // actually restarts rather than staying permanently stuck.
    let raw = std::fs::read_to_string(&marker_path).expect("read marker");
    let persisted: std::collections::HashMap<String, String> =
        serde_json::from_str(&raw).expect("parse marker");
    let stamp = persisted.get("corrupt").expect("entry must survive");
    assert!(
        chrono::DateTime::parse_from_rfc3339(stamp).is_ok(),
        "the corrupt entry must be restamped with a valid timestamp, got {stamp:?}"
    );

    handle.abort();
}

/// An ATTACHED record is never a candidate, whatever its state says (PR #4725
/// review). `reconcile_live_state` only rewrites `state` for records persisted
/// `Active`/`Stopped`, so an `errored` row keeps its persisted state even with a
/// live pane; `attached` is the one liveness signal it does carry.
#[tokio::test]
async fn auto_prune_never_touches_an_attached_record() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let (url, captured, handle) = spawn_recording_decommission_stub().await;
    let client = reqwest::Client::new();

    let mut attached = stopped_session_at("attached-errored", &gone);
    attached.state = "errored".to_string();
    attached.unresumable = true;
    attached.attached = true;

    for _ in 0..3 {
        let outcome = auto_prune_dead_records_at(
            &client,
            &url,
            vec![attached.clone()],
            &marker_path,
            Some(HashSet::new()),
        )
        .await;
        assert_eq!(
            outcome.pruned, 0,
            "an attached session must never be cleared"
        );
        if marker_path.exists() {
            backdate_sightings(&marker_path, 11);
        }
    }
    assert!(captured.lock().expect("capture lock").is_empty());

    handle.abort();
}

/// Change 5 (PR #4725 review): `--json` echoes the body it already fetched and
/// never re-GETs after pruning.
///
/// Why: the re-GET returned the just-pruned row with `state` flipped to
/// `"decommissioned"` (the raw passthrough is unfiltered, so it did not
/// disappear), and its `?` could fail the command AFTER the registry had already
/// been mutated — the caller would see an error with no idea the prune landed.
/// What: counts GETs against a stub that also honours the decommission, across a
/// call that genuinely prunes. Exactly one GET per invocation.
///
/// PR #4725 review round 2 (MEDIUM): this test also asserts a decommission POST
/// actually fired. Without that it passed with `is_dead_record` stubbed to
/// always-false — proving only "one GET when nothing happens", which is not the
/// claim. The POST count is the precondition that makes the GET count meaningful.
#[tokio::test]
async fn session_ls_json_never_refetches_after_pruning() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let marker_path = root.path().join("seen.json");
    let gone = root.path().join("gone");
    let gets = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let deletes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let body = serde_json::json!({
        "sessions": [{
            "id": "j1",
            "name": "j1",
            "state": "stopped",
            "persisted_state": "stopped",
            "workspace_path": gone.to_string_lossy(),
            "cwd": gone.to_string_lossy(),
            "created_at": "2026-08-01T00:00:00Z",
            "unresumable": false,
            "slot": 1,
            "deleted": false,
        }]
    });
    let get_sink = gets.clone();
    let app = axum::Router::new()
        .route(
            "/api/v1/sessions/managed",
            axum::routing::get(move || {
                let get_sink = get_sink.clone();
                let body = body.clone();
                async move {
                    get_sink.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(body)
                }
            }),
        )
        .route(
            "/api/v1/sessions/managed/{id}/decommission",
            axum::routing::post({
                let delete_sink = deletes.clone();
                move |axum::extract::Path(_id): axum::extract::Path<String>| {
                    let delete_sink = delete_sink.clone();
                    async move {
                        delete_sink.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(serde_json::json!({ "workspace_removed": false }))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // First invocation records the sighting; after the window elapses the second
    // genuinely prunes — the case that used to trigger the re-GET.
    let mut seen = std::collections::HashMap::new();
    seen.insert(
        "j1".to_string(),
        (chrono::Utc::now() - chrono::Duration::minutes(11)).to_rfc3339(),
    );
    std::fs::write(&marker_path, serde_json::to_string(&seen).unwrap()).unwrap();

    crate::commands::managed::session_ls_at(
        &client,
        &url,
        true,
        None,
        false,
        false, // attached = false
        crate::commands::session_picker::SessionSortArg::Recent,
        None,
        &hermetic_prune_ctx(&marker_path),
    )
    .await
    .expect("session_ls --json");

    // PRECONDITION: a prune must actually have happened, or the GET count below
    // proves nothing. This is the assertion whose absence let the test pass
    // against an always-false `is_dead_record`.
    assert_eq!(
        deletes.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "precondition: this invocation must genuinely prune a record — otherwise \
         'exactly one GET' is vacuous"
    );
    assert_eq!(
        gets.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "`--json` must issue exactly ONE GET even when the prune removed a record"
    );

    handle.abort();
}

// ── `tm f` — the type-to-filter picker's CLI surface ─────────────────────────
//
// `f` is a new top-level command under `infer_subcommands = true`. No other
// subcommand starts with `f`, so it shadows nothing; these tests pin that the
// exact spelling resolves to `Command::F` and that its flags mirror `ls`.

/// `tm f api` captures the pattern positionally.
#[test]
fn cli_parses_f_pattern() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f", "api"]).unwrap();
    match cli.command.unwrap() {
        Command::F(a) => assert_eq!(a.pattern, vec!["api".to_string()]),
        other => panic!("expected top-level f, got {other:?}"),
    }
}

/// Multi-word patterns are captured in order; the handler joins them with a
/// single space. Unlike `ls`, the first word is NEVER a sort keyword — `alpha`
/// here is part of the pattern.
#[test]
fn cli_parses_f_multi_word_pattern() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f", "alpha", "api"]).unwrap();
    match cli.command.unwrap() {
        Command::F(a) => {
            assert_eq!(a.pattern, vec!["alpha".to_string(), "api".to_string()])
        }
        other => panic!("expected top-level f, got {other:?}"),
    }
}

/// Bare `tm f` opens the picker with an empty filter box — the pattern is a
/// seed, not a required argument.
#[test]
fn cli_parses_f_bare_is_allowed() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f"]).unwrap();
    match cli.command.unwrap() {
        Command::F(a) => assert!(a.pattern.is_empty()),
        other => panic!("expected top-level f, got {other:?}"),
    }
}

/// `--json` parses and rides alongside the pattern; the handler uses it to skip
/// the interactive path entirely.
#[test]
fn cli_parses_f_json() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f", "--json", "api"]).unwrap();
    match cli.command.unwrap() {
        Command::F(a) => {
            assert_eq!(a.pattern, vec!["api".to_string()]);
            assert!(a.json);
        }
        other => panic!("expected top-level f --json, got {other:?}"),
    }
}

/// `--source-id` and `--current` are mutually exclusive, matching `tm ls`.
#[test]
fn cli_f_source_id_and_current_conflict() {
    let err = Cli::try_parse_from([
        "trusty-mpm",
        "f",
        "--source-id",
        "owner/repo",
        "--current",
        "api",
    ]);
    assert!(err.is_err(), "--source-id and --current must conflict");
}

/// `tm f` must not shadow any pre-existing subcommand. Under
/// `infer_subcommands`, an `f` that collided with (say) a hypothetical `fix`
/// would either resolve to that command or error as ambiguous — this pins that
/// it resolves to `Command::F` itself.
#[test]
fn cli_f_does_not_shadow_another_subcommand() {
    let cli = Cli::try_parse_from(["trusty-mpm", "f", "x"]).unwrap();
    assert!(matches!(cli.command, Some(Command::F(_))));
}

/// `tm f`'s NAME-only scope is the reason it exists next to `tm ls <term>`,
/// whose scope stays every visible column. Both scopes go through the SAME
/// `filter_sessions_by_term`, so this pins that the scope — not a second
/// filter function — is what differs.
#[test]
fn filter_sessions_by_name_ignores_non_name_columns() {
    let sessions = vec![
        ls_test_session(
            "tm-alpha-01",
            "active",
            None,
            None,
            Some("acme/api-server"),
            Some("fix the api"),
        ),
        ls_test_session("tm-api-02", "active", None, None, None, None),
    ];
    let by_name = filter_sessions_by_term(sessions.clone(), Some(&SessionFilter::name("api")));
    assert_eq!(
        by_name.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["tm-api-02"],
        "NAME scope must ignore a matching task and project slug"
    );
    let by_visible = filter_sessions_by_term(sessions, Some(&SessionFilter::visible("api")));
    assert_eq!(
        by_visible.len(),
        2,
        "the ls scope still matches task/source_id"
    );
}
