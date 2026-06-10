//! CLI parse tests: attach, connect, daemon flags, services subcommands,
//! repair subcommands, and regression tests for issue #382
//! (compose_session_instructions).
//!
//! Why: companion to `tests_behavior_a.rs`; extracting the second half of
//! the behavioral suite keeps both files well under the 500-line cap.
//! What: `cli_parses_attach_*`, `cli_parses_connect_*`,
//! `cli_parses_daemon_custom_addr`, services subcommand parse tests,
//! `cli_parses_repair_deploy`, and three `compose_session_instructions_*`
//! regression tests.
//! Test: `cargo test -p trusty-mpm` runs the full suite.

use clap::Parser;

use crate::cli::{Cli, Command, RepairAction, ServicesAction};
use crate::commands::session::compose_session_instructions;

#[test]
fn cli_parses_attach() {
    let cli = Cli::try_parse_from(["trusty-mpm", "attach", "frontend"]).unwrap();
    match cli.command {
        Command::Attach { target, json } => {
            assert_eq!(target, "frontend");
            assert!(!json);
        }
        other => panic!("expected Attach, got {other:?}"),
    }
}

#[test]
fn cli_parses_attach_with_json() {
    let cli = Cli::try_parse_from(["trusty-mpm", "attach", "abc-123", "--json"]).unwrap();
    match cli.command {
        Command::Attach { target, json } => {
            assert_eq!(target, "abc-123");
            assert!(json);
        }
        other => panic!("expected Attach, got {other:?}"),
    }
}

#[test]
fn cli_attach_requires_target() {
    assert!(Cli::try_parse_from(["trusty-mpm", "attach"]).is_err());
}

#[test]
fn cli_parses_connect() {
    // `tm connect` is the no-deployment session starter; it takes an
    // optional project directory, exactly like `tm launch`.
    let cli = Cli::try_parse_from(["trusty-mpm", "connect"]).unwrap();
    match cli.command {
        Command::Connect { dir } => assert_eq!(dir, None),
        other => panic!("expected Connect, got {other:?}"),
    }
}

#[test]
fn cli_parses_connect_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "connect", "/work/p"]).unwrap();
    match cli.command {
        Command::Connect { dir } => assert_eq!(dir.as_deref(), Some("/work/p")),
        other => panic!("expected Connect, got {other:?}"),
    }
}

#[test]
fn cli_parses_daemon_custom_addr() {
    let cli = Cli::try_parse_from(["trusty-mpm", "daemon", "--addr", "0.0.0.0:9000"]).unwrap();
    match cli.command {
        Command::Daemon { addr, .. } => assert_eq!(addr.to_string(), "0.0.0.0:9000"),
        other => panic!("expected Daemon, got {other:?}"),
    }
}

// ── tm services subcommand parse tests (issue #339) ──────────────────────

#[test]
fn cli_parses_services_list() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "list"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Services {
            action: ServicesAction::List { json: false }
        }
    ));
}

#[test]
fn cli_parses_services_list_json() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "list", "--json"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Services {
            action: ServicesAction::List { json: true }
        }
    ));
}

#[test]
fn cli_parses_services_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "status", "trusty-search"]).unwrap();
    match cli.command {
        Command::Services {
            action: ServicesAction::Status { name, json },
        } => {
            assert_eq!(name, "trusty-search");
            assert!(!json);
        }
        other => panic!("expected services status, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_status_json() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "services",
        "status",
        "trusty-search",
        "--json",
    ])
    .unwrap();
    match cli.command {
        Command::Services {
            action: ServicesAction::Status { name, json },
        } => {
            assert_eq!(name, "trusty-search");
            assert!(json);
        }
        other => panic!("expected services status --json, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_port() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "port", "trusty-search"]).unwrap();
    match cli.command {
        Command::Services {
            action: ServicesAction::Port { name },
        } => assert_eq!(name, "trusty-search"),
        other => panic!("expected services port, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_url() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "url", "trusty-search"]).unwrap();
    match cli.command {
        Command::Services {
            action: ServicesAction::Url { name },
        } => assert_eq!(name, "trusty-search"),
        other => panic!("expected services url, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_health() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "health", "trusty-search"]).unwrap();
    match cli.command {
        Command::Services {
            action: ServicesAction::Health { name },
        } => assert_eq!(name, "trusty-search"),
        other => panic!("expected services health, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_log() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "log", "trusty-search"]).unwrap();
    match cli.command {
        Command::Services {
            action: ServicesAction::Log { name },
        } => assert_eq!(name, "trusty-search"),
        other => panic!("expected services log, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_init() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "init"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Services {
            action: ServicesAction::Init { force: false }
        }
    ));
}

#[test]
fn cli_parses_services_init_force() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "init", "--force"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Services {
            action: ServicesAction::Init { force: true }
        }
    ));
}

#[test]
fn cli_parses_services_restart() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "restart", "trusty-search"]).unwrap();
    match cli.command {
        Command::Services {
            action: ServicesAction::Restart { name },
        } => assert_eq!(name, "trusty-search"),
        other => panic!("expected services restart, got {other:?}"),
    }
}

// ------------------------------------------------------------------
// Regression tests for issue #382: compose_session_instructions must
// display exactly what it stashes and what the live launch prompt is.
// ------------------------------------------------------------------

#[test]
fn compose_session_instructions_display_matches_stash() {
    // Why: the #382 bug was that `tm session instructions` printed
    // `output.merged` (old pipeline text) while the stash held the
    // override-resolved PM prompt — a visible divergence. After the fix
    // both come from `resolve_pm_prompt`, so they must be identical.
    // What: calls `compose_session_instructions` and reads the written
    // stash file; asserts the returned display string equals it.
    // Test: the return value is compared byte-for-byte against the
    // on-disk stash to detect any future divergence.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    let fw = trusty_mpm::core::paths::FrameworkPaths::default();

    let (display, _output, stash_path) =
        compose_session_instructions(&fw, project).expect("compose succeeds");

    let on_disk =
        std::fs::read_to_string(&stash_path).expect("stash file must be readable after compose");

    assert_eq!(
        display, on_disk,
        "tm session instructions display must equal the stash file (issue #382)"
    );
}

#[test]
fn compose_session_instructions_display_matches_live_prompt() {
    // Why: `tm session instructions` must show exactly what `claude` receives
    // via `--append-system-prompt-file`; the live prompt is produced by
    // `build_system_prompt_for`, which calls `resolve_pm_prompt`. If
    // `compose_session_instructions` ever returns something different from
    // `build_system_prompt_for`, the stash would again diverge from reality.
    // What: runs `compose_session_instructions` and `build_system_prompt_for`
    // on the same empty project directory and asserts the outputs match.
    // Test: any future change that re-introduces the #382 divergence will
    // break this test immediately.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    let fw = trusty_mpm::core::paths::FrameworkPaths::default();

    let (display, _output, _stash) =
        compose_session_instructions(&fw, project).expect("compose succeeds");

    let live_prompt = trusty_mpm::core::session_launch::build_system_prompt_for(project);

    assert_eq!(
        display, live_prompt,
        "tm session instructions output must match the live launch prompt (issue #382)"
    );
}

#[test]
fn compose_session_instructions_display_matches_live_prompt_with_override() {
    // Why: the same convergence guarantee must hold when project-level override
    // files are present — the stash and the display must reflect the override,
    // not the bundled defaults.
    // What: writes a `WORKFLOW.md` override, then asserts the display and the
    // live prompt both include it (and don't include the bundled heading).
    // Test: if `compose_session_instructions` stops reading overrides for the
    // display path, this test fails.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    let fw = trusty_mpm::core::paths::FrameworkPaths::default();

    let override_dir = project.join(".trusty-mpm");
    std::fs::create_dir_all(&override_dir).unwrap();
    std::fs::write(
        override_dir.join("WORKFLOW.md"),
        "# Custom Workflow\n\nCOMPOSE_OVERRIDE_MARKER\n",
    )
    .unwrap();

    let (display, _output, _stash) =
        compose_session_instructions(&fw, project).expect("compose succeeds");

    let live_prompt = trusty_mpm::core::session_launch::build_system_prompt_for(project);

    assert_eq!(
        display, live_prompt,
        "display and live prompt must match with overrides present (issue #382)"
    );
    assert!(
        display.contains("COMPOSE_OVERRIDE_MARKER"),
        "override must be reflected in display"
    );
    assert!(
        !display.contains("# PM Workflow Configuration"),
        "bundled workflow must be replaced in display"
    );
}

#[test]
fn cli_parses_daemon_mcp() {
    let cli = Cli::try_parse_from(["trusty-mpm", "daemon", "--mcp"]).unwrap();
    match cli.command {
        Command::Daemon { mcp, .. } => assert!(mcp),
        other => panic!("expected Daemon, got {other:?}"),
    }
}

#[test]
fn cli_parses_repair_deploy() {
    // `tm repair deploy` must parse to Command::Repair { action: RepairAction::Deploy { force: false } }.
    let cli = Cli::try_parse_from(["trusty-mpm", "repair", "deploy"]).unwrap();
    match cli.command {
        Command::Repair {
            action: RepairAction::Deploy { force },
        } => {
            assert!(!force, "force must default to false");
        }
        other => panic!("expected Repair {{ Deploy }}, got {other:?}"),
    }
}

#[test]
fn cli_parses_repair_deploy_force() {
    // `tm repair deploy --force` must parse with force=true.
    let cli = Cli::try_parse_from(["trusty-mpm", "repair", "deploy", "--force"]).unwrap();
    match cli.command {
        Command::Repair {
            action: RepairAction::Deploy { force },
        } => {
            assert!(force, "force must be true with --force flag");
        }
        other => panic!("expected Repair {{ Deploy {{ force: true }} }}, got {other:?}"),
    }
}
