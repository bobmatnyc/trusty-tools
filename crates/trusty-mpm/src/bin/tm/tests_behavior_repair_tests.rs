//! CLI parse tests for `tm repair` (issues #2603, #2867).
//!
//! Why: `tm repair push-guard` is the only supported way to retrofit the
//! #2867 cross-branch push guard onto an already-provisioned clone, so its
//! invocation surface — the verb name doctor prints, and both flags — is a
//! contract. A rename that silently breaks doctor's printed remediation is
//! exactly the failure this pins.
//! What: `cli_parses_repair_deploy`, `cli_parses_repair_push_guard`,
//! `cli_parses_repair_push_guard_flags`.
//! Test: this module IS the test suite for the `tm repair` CLI surface.

use clap::Parser;

use crate::cli::{Cli, Command, RepairAction};

#[test]
fn cli_parses_repair_deploy() {
    let cli = Cli::try_parse_from(["trusty-mpm", "repair", "deploy"]).unwrap();
    match cli.command.unwrap() {
        Command::Repair {
            action: RepairAction::Deploy { force },
        } => assert!(!force),
        other => panic!("expected Command::Repair(Deploy), got {other:?}"),
    }
}

#[test]
fn cli_parses_repair_push_guard() {
    let cli = Cli::try_parse_from(["trusty-mpm", "repair", "push-guard"]).unwrap();
    match cli.command.unwrap() {
        Command::Repair {
            action: RepairAction::PushGuard { path, dry_run },
        } => {
            assert_eq!(path, None, "the default target is the current directory");
            assert!(!dry_run, "a bare invocation must actually install");
        }
        other => panic!("expected Command::Repair(PushGuard), got {other:?}"),
    }
}

#[test]
fn cli_parses_repair_push_guard_flags() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "repair",
        "push-guard",
        "--path",
        "/tmp/some-clone",
        "--dry-run",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Repair {
            action: RepairAction::PushGuard { path, dry_run },
        } => {
            assert_eq!(path, Some("/tmp/some-clone".to_string()));
            assert!(dry_run);
        }
        other => panic!("expected Command::Repair(PushGuard), got {other:?}"),
    }
}

/// Why (#5007): `tm doctor`'s `session_store` verdict and the store's own error
/// message both name this verb as the fix. A rename would break both printed
/// remediations at once, exactly as the push-guard tests above guard against.
/// What: pins the bare invocation and its defaults.
/// Test: this test.
#[test]
fn cli_parses_repair_session_store() {
    let cli = Cli::try_parse_from(["trusty-mpm", "repair", "session-store"]).unwrap();
    match cli.command.unwrap() {
        Command::Repair {
            action:
                RepairAction::SessionStore {
                    path,
                    dry_run,
                    force,
                },
        } => {
            assert_eq!(path, None, "the default target is the real store");
            assert!(!dry_run, "a bare invocation actually repairs");
            assert!(!force, "a bare invocation refuses an orphaned tail");
        }
        other => panic!("expected Command::Repair(SessionStore), got {other:?}"),
    }
}

/// Why (#7569): the command REWRITES the operator's savings ledger, so the
/// safe default is the contract — a bare invocation must report and write
/// nothing. A flag that defaulted the other way would destroy rows before
/// anyone authorised it.
/// What: pins the verb name and the write-nothing default.
/// Test: this test.
#[test]
fn cli_parses_repair_savings_ledger() {
    let cli = Cli::try_parse_from(["trusty-mpm", "repair", "savings-ledger"]).unwrap();
    match cli.command.unwrap() {
        Command::Repair {
            action:
                RepairAction::SavingsLedger {
                    root,
                    apply,
                    dry_run,
                    markers,
                },
        } => {
            assert_eq!(root, None, "the default target is the real framework root");
            assert!(!apply, "a bare invocation writes nothing");
            assert!(!dry_run, "dry-run is the default, not a flag it must carry");
            assert!(!markers, "a bare invocation leaves the markers alone");
        }
        other => panic!("expected Command::Repair(SavingsLedger), got {other:?}"),
    }
}

/// Why (#7569): `--apply` is the only way past the dry run, `--root` the only
/// way to rehearse against a copy, and `--markers` the only way to sweep the
/// stray marker directory; all three must be reachable.
/// What: pins the three flags together.
/// Test: this test.
#[test]
fn cli_parses_repair_savings_ledger_flags() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "repair",
        "savings-ledger",
        "--root",
        "/tmp/rehearsal",
        "--apply",
        "--markers",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Repair {
            action:
                RepairAction::SavingsLedger {
                    root,
                    apply,
                    dry_run,
                    markers,
                },
        } => {
            assert_eq!(root, Some("/tmp/rehearsal".to_string()));
            assert!(apply);
            assert!(!dry_run);
            assert!(markers);
        }
        other => panic!("expected Command::Repair(SavingsLedger), got {other:?}"),
    }
}

/// Why (#7569): `--apply` and `--dry-run` state opposite intentions, and a
/// command that silently picked one would repair a ledger an operator meant
/// only to inspect.
/// Test: this test.
#[test]
fn cli_rejects_repair_savings_ledger_apply_with_dry_run() {
    assert!(
        Cli::try_parse_from([
            "trusty-mpm",
            "repair",
            "savings-ledger",
            "--apply",
            "--dry-run",
        ])
        .is_err(),
        "--apply and --dry-run must not be combinable"
    );
}

/// Why: `--dry-run` is the only way to inspect the cut before authorising it
/// and `--force` the only way past the orphan refusal; both must be reachable.
/// What: pins all three flags.
/// Test: this test.
#[test]
fn cli_parses_repair_session_store_flags() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "repair",
        "session-store",
        "--path",
        "/tmp/sessions.json",
        "--dry-run",
        "--force",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Repair {
            action:
                RepairAction::SessionStore {
                    path,
                    dry_run,
                    force,
                },
        } => {
            assert_eq!(path, Some("/tmp/sessions.json".to_string()));
            assert!(dry_run);
            assert!(force);
        }
        other => panic!("expected Command::Repair(SessionStore), got {other:?}"),
    }
}
