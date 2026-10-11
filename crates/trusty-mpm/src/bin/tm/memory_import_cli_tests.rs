//! `tm memory import` / `import-auto-memory` CLI parse tests (#4837, #7685).
//!
//! Why: split out of `tests.rs` (#9340) so that file stays under the
//! 3000-SLOC test cap when `tm memory forget` adds its parse test there.
//! What: parse round-trips for the two bulk-import verbs, and for
//! `tm memory rename` (#9544).
//! Test: this file.

use clap::Parser;

use crate::cli::{Cli, Command, MemoryAction};

/// Why (#4837): `tm memory import` is the zero-inference bulk-load path;
/// its required `--palace` and the default (write-enabled) mode must parse.
#[test]
fn cli_parses_memory_import() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "memory",
        "import",
        "/tmp/mem",
        "--palace",
        "trusty-tools",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Memory {
            action:
                MemoryAction::Import {
                    dir,
                    palace,
                    dry_run,
                    refresh,
                    json,
                    allow_secret_like,
                    memory_socket,
                },
        } => {
            assert_eq!(dir, std::path::PathBuf::from("/tmp/mem"));
            assert_eq!(palace, "trusty-tools");
            assert!(!dry_run, "writes are the default mode");
            assert!(!refresh, "#5044: replacing a drifted drawer stays opt-in");
            assert!(!json);
            assert!(!allow_secret_like);
            assert!(
                memory_socket.is_none(),
                "the socket path defaults to the derived one"
            );
        }
        other => panic!("expected Memory/Import, got {other:?}"),
    }
}

/// Why (#7685): `tm memory import-auto-memory` is the migration that makes
/// emptying Claude Code's own `MEMORY.md` safe, so its kebab-case spelling and
/// its all-optional flags — every one of which has a derived default — must
/// parse. A bare invocation is the common case: cwd, the project's own palace.
#[test]
fn cli_parses_memory_import_auto_memory() {
    let cli = Cli::try_parse_from(["trusty-mpm", "memory", "import-auto-memory"]).unwrap();
    match cli.command.unwrap() {
        Command::Memory {
            action:
                MemoryAction::ImportAutoMemory {
                    project,
                    palace,
                    json,
                    memory_socket,
                },
        } => {
            assert!(project.is_none(), "the project defaults to the cwd");
            assert!(palace.is_none(), "the palace defaults to the project's own");
            assert!(!json);
            assert!(memory_socket.is_none());
        }
        other => panic!("expected Memory/ImportAutoMemory, got {other:?}"),
    }

    let explicit = Cli::try_parse_from([
        "trusty-mpm",
        "memory",
        "import-auto-memory",
        "--project",
        "/tmp/ws",
        "--palace",
        "trusty-tools",
        "--json",
    ])
    .unwrap();
    match explicit.command.unwrap() {
        Command::Memory {
            action:
                MemoryAction::ImportAutoMemory {
                    project,
                    palace,
                    json,
                    ..
                },
        } => {
            assert_eq!(project, Some(std::path::PathBuf::from("/tmp/ws")));
            assert_eq!(palace.as_deref(), Some("trusty-tools"));
            assert!(json);
        }
        other => panic!("expected Memory/ImportAutoMemory, got {other:?}"),
    }
}

/// Why (#4837): `--dry-run` is the safety flag an operator reaches for first,
/// and `--json` is the machine-readable report a caller verifies with — both
/// must round-trip, together with the explicit `--memory-socket` override and
/// `--refresh` (#5044).
#[test]
fn cli_parses_memory_import_dry_run_json() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "memory",
        "import",
        "/tmp/mem",
        "--palace",
        "p",
        "--dry-run",
        "--refresh",
        "--json",
        "--allow-secret-like",
        "--memory-socket",
        "/tmp/trusty-memory.sock",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Memory {
            action:
                MemoryAction::Import {
                    dry_run,
                    refresh,
                    json,
                    allow_secret_like,
                    memory_socket,
                    ..
                },
        } => {
            assert!(dry_run);
            assert!(refresh);
            assert!(json);
            assert!(allow_secret_like);
            assert_eq!(
                memory_socket.as_deref(),
                Some(std::path::Path::new("/tmp/trusty-memory.sock"))
            );
        }
        other => panic!("expected Memory/Import, got {other:?}"),
    }
}

/// Why (#9544): `tm memory rename <old> <new>` takes both ids positionally and
/// `--replace-empty` defaults off, so a bare call never replaces a palace.
#[test]
fn tm_memory_rename_parses_old_new_and_replace_empty() {
    for (extra, expect_replace) in [(None, false), (Some("--replace-empty"), true)] {
        let mut argv = vec!["trusty-mpm", "memory", "rename", "src-pal", "dst-pal"];
        argv.extend(extra);
        let cli = Cli::try_parse_from(argv).unwrap();
        match cli.command.unwrap() {
            Command::Memory {
                action:
                    MemoryAction::Rename {
                        old,
                        new,
                        replace_empty,
                        json,
                        memory_socket,
                    },
            } => {
                assert_eq!((old.as_str(), new.as_str()), ("src-pal", "dst-pal"));
                assert_eq!(replace_empty, expect_replace);
                assert!(!json);
                assert!(memory_socket.is_none());
            }
            other => panic!("expected memory rename, got {other:?}"),
        }
    }
    assert!(Cli::try_parse_from(["trusty-mpm", "memory", "rename", "only-one"]).is_err());
}
