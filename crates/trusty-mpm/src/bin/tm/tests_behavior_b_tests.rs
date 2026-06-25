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

use crate::cli::{Cli, Command, RepairAction, ServicesAction, SessctlAction};
use crate::commands::session::compose_session_instructions;

#[test]
fn cli_parses_attach() {
    let cli = Cli::try_parse_from(["trusty-mpm", "attach", "frontend"]).unwrap();
    match cli.command.unwrap() {
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
    match cli.command.unwrap() {
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
    match cli.command.unwrap() {
        Command::Connect { dir } => assert_eq!(dir, None),
        other => panic!("expected Connect, got {other:?}"),
    }
}

#[test]
fn cli_parses_connect_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "connect", "/work/p"]).unwrap();
    match cli.command.unwrap() {
        Command::Connect { dir } => assert_eq!(dir.as_deref(), Some("/work/p")),
        other => panic!("expected Connect, got {other:?}"),
    }
}

#[test]
fn cli_sm_aliases_reach_coordinator() {
    // DOC-14 D0.2: `tm coordinator`, `tm sm`, `tm session-manager`, and the
    // hidden `tm coord` must all parse to the SAME `Command::Coordinator`, so
    // every spelling reaches one code path.
    for name in ["coordinator", "sm", "session-manager", "coord"] {
        let cli = Cli::try_parse_from(["trusty-mpm", name, "hello there"])
            .unwrap_or_else(|e| panic!("`tm {name}` must parse: {e}"));
        match cli.command.unwrap() {
            Command::Coordinator { message, action } => {
                assert_eq!(
                    message.as_deref(),
                    Some("hello there"),
                    "`tm {name}` carries the message"
                );
                assert!(action.is_none(), "a plain message has no subcommand");
            }
            other => panic!("`tm {name}` must be Command::Coordinator, got {other:?}"),
        }
    }
}

/// DOC-14 SM-STDIO (#1291): `tm sm serve --stdio` (and via every coordinator
/// alias) must parse to `Command::Coordinator` with the `Serve { stdio: true }`
/// subcommand — the JSON-RPC over STDIO adapter entry point.
#[test]
fn cli_parses_sm_serve_stdio() {
    use crate::cli::CoordinatorAction;
    for name in ["coordinator", "sm", "session-manager", "coord"] {
        let cli = Cli::try_parse_from(["trusty-mpm", name, "serve", "--stdio"])
            .unwrap_or_else(|e| panic!("`tm {name} serve --stdio` must parse: {e}"));
        match cli.command.unwrap() {
            Command::Coordinator { message, action } => {
                assert!(message.is_none(), "the serve subcommand carries no message");
                match action {
                    Some(CoordinatorAction::Serve { stdio }) => {
                        assert!(stdio, "--stdio sets the stdio flag");
                    }
                    other => panic!("`tm {name} serve` must be Serve, got {other:?}"),
                }
            }
            other => panic!("`tm {name} serve` must be Command::Coordinator, got {other:?}"),
        }
    }
}

#[test]
fn cli_parses_daemon_custom_addr() {
    let cli = Cli::try_parse_from(["trusty-mpm", "daemon", "--addr", "0.0.0.0:9000"]).unwrap();
    match cli.command.unwrap() {
        Command::Daemon { addr, .. } => assert_eq!(addr.to_string(), "0.0.0.0:9000"),
        other => panic!("expected Daemon, got {other:?}"),
    }
}

// ── tm services subcommand parse tests (issue #339) ──────────────────────

#[test]
fn cli_parses_services_list() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "list"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Services {
            action: ServicesAction::List { json: false }
        }
    ));
}

#[test]
fn cli_parses_services_list_json() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "list", "--json"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Services {
            action: ServicesAction::List { json: true }
        }
    ));
}

#[test]
fn cli_parses_services_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "status", "trusty-search"]).unwrap();
    match cli.command.unwrap() {
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
    match cli.command.unwrap() {
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
    match cli.command.unwrap() {
        Command::Services {
            action: ServicesAction::Port { name },
        } => assert_eq!(name, "trusty-search"),
        other => panic!("expected services port, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_url() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "url", "trusty-search"]).unwrap();
    match cli.command.unwrap() {
        Command::Services {
            action: ServicesAction::Url { name },
        } => assert_eq!(name, "trusty-search"),
        other => panic!("expected services url, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_health() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "health", "trusty-search"]).unwrap();
    match cli.command.unwrap() {
        Command::Services {
            action: ServicesAction::Health { name },
        } => assert_eq!(name, "trusty-search"),
        other => panic!("expected services health, got {other:?}"),
    }
}

#[test]
fn cli_parses_services_log() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "log", "trusty-search"]).unwrap();
    match cli.command.unwrap() {
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
        cli.command.unwrap(),
        Command::Services {
            action: ServicesAction::Init { force: false }
        }
    ));
}

#[test]
fn cli_parses_services_init_force() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "init", "--force"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Services {
            action: ServicesAction::Init { force: true }
        }
    ));
}

#[test]
fn cli_parses_services_restart() {
    let cli = Cli::try_parse_from(["trusty-mpm", "services", "restart", "trusty-search"]).unwrap();
    match cli.command.unwrap() {
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
    // Why: the #382 bug was that `tm sessions instructions` printed
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
        "tm sessions instructions display must equal the stash file (issue #382)"
    );
}

#[test]
fn compose_session_instructions_display_matches_live_prompt() {
    // Why: `tm sessions instructions` must show exactly what `claude` receives
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
        "tm sessions instructions output must match the live launch prompt (issue #382)"
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
    match cli.command.unwrap() {
        Command::Daemon { mcp, .. } => assert!(mcp),
        other => panic!("expected Daemon, got {other:?}"),
    }
}

#[test]
fn cli_parses_repair_deploy() {
    // `tm repair deploy` must parse to Command::Repair { action: RepairAction::Deploy { force: false } }.
    let cli = Cli::try_parse_from(["trusty-mpm", "repair", "deploy"]).unwrap();
    match cli.command.unwrap() {
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
    match cli.command.unwrap() {
        Command::Repair {
            action: RepairAction::Deploy { force },
        } => {
            assert!(force, "force must be true with --force flag");
        }
        other => panic!("expected Repair {{ Deploy {{ force: true }} }}, got {other:?}"),
    }
}

// ── standalone rm / update CLI parse tests ──────────────────────────────────

#[test]
fn cli_parses_rm_standalone() {
    let cli = Cli::try_parse_from(["trusty-mpm", "rm", "my-proj"]).unwrap();
    match cli.command.unwrap() {
        Command::Rm { alias, root } => {
            assert_eq!(alias, "my-proj");
            assert_eq!(root, None);
        }
        other => panic!("expected Rm, got {other:?}"),
    }
}

#[test]
fn cli_parses_rm_with_root() {
    let cli =
        Cli::try_parse_from(["trusty-mpm", "rm", "my-proj", "--root", "/tmp/custom"]).unwrap();
    match cli.command.unwrap() {
        Command::Rm { alias, root } => {
            assert_eq!(alias, "my-proj");
            assert_eq!(root.as_deref(), Some("/tmp/custom"));
        }
        other => panic!("expected Rm, got {other:?}"),
    }
}

#[test]
fn cli_rm_requires_alias() {
    assert!(Cli::try_parse_from(["trusty-mpm", "rm"]).is_err());
}

#[test]
fn cli_parses_update_standalone() {
    let cli = Cli::try_parse_from(["trusty-mpm", "update", "my-proj"]).unwrap();
    match cli.command.unwrap() {
        Command::Update { alias, root } => {
            assert_eq!(alias.as_deref(), Some("my-proj"));
            assert_eq!(root, None);
        }
        other => panic!("expected Update, got {other:?}"),
    }
}

#[test]
fn cli_parses_update_all_standalone() {
    // `tm update` without alias should update all loaded aliases.
    let cli = Cli::try_parse_from(["trusty-mpm", "update"]).unwrap();
    match cli.command.unwrap() {
        Command::Update { alias, root } => {
            assert_eq!(alias, None);
            assert_eq!(root, None);
        }
        other => panic!("expected Update, got {other:?}"),
    }
}

#[test]
fn cli_parses_update_with_root() {
    let cli = Cli::try_parse_from(["trusty-mpm", "update", "--root", "/tmp/r"]).unwrap();
    match cli.command.unwrap() {
        Command::Update { alias, root } => {
            assert_eq!(alias, None);
            assert_eq!(root.as_deref(), Some("/tmp/r"));
        }
        other => panic!("expected Update, got {other:?}"),
    }
}

// ── standalone rm / update handler unit tests ───────────────────────────────

#[test]
fn rm_cmd_removes_registry_entry_and_project_dir() {
    use crate::commands::managed_root::ManagedPaths;
    use crate::commands::standalone::rm_cmd;
    use trusty_mpm::core::standalone::registry::ManagedRegistry;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let paths = ManagedPaths::from_root(root.clone());

    // Register and set up a fake project dir.
    let mut reg = ManagedRegistry::load(&root).unwrap();
    reg.add("demo", "https://github.com/org/repo", false)
        .unwrap();
    reg.save().unwrap();

    let project_dir = root.join("projects").join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("sentinel.txt"), b"data").unwrap();

    rm_cmd(&paths, "demo").expect("rm_cmd must succeed");

    // Registry entry must be gone.
    let reg2 = ManagedRegistry::load(&root).unwrap();
    assert!(
        reg2.list().is_empty(),
        "registry must be empty after rm_cmd"
    );
    // Project dir must be deleted.
    assert!(
        !project_dir.exists(),
        "project dir must be deleted by rm_cmd"
    );
}

#[test]
fn rm_cmd_errors_on_unknown_alias() {
    use crate::commands::managed_root::ManagedPaths;
    use crate::commands::standalone::rm_cmd;

    let tmp = tempfile::TempDir::new().unwrap();
    let paths = ManagedPaths::from_root(tmp.path().to_path_buf());

    let result = rm_cmd(&paths, "nonexistent");
    assert!(result.is_err(), "rm_cmd must error on unknown alias");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("nonexistent"),
        "error message must mention the alias"
    );
}

#[test]
fn rm_cmd_leaves_claude_config_intact() {
    use crate::commands::managed_root::ManagedPaths;
    use crate::commands::standalone::rm_cmd;
    use trusty_mpm::core::standalone::registry::ManagedRegistry;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let paths = ManagedPaths::from_root(root.clone());

    // Create a fake shared claude-config dir.
    let cfg_dir = root.join("claude-config");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("shared.json"), b"{}").unwrap();

    // Register a project and create its directory.
    let mut reg = ManagedRegistry::load(&root).unwrap();
    reg.add("proj", "https://github.com/org/repo", false)
        .unwrap();
    reg.save().unwrap();
    let project_dir = root.join("projects").join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();

    rm_cmd(&paths, "proj").expect("rm_cmd must succeed");

    // The shared claude-config dir must still be intact.
    assert!(
        cfg_dir.exists(),
        "claude-config dir must NOT be touched by rm_cmd"
    );
    assert!(
        cfg_dir.join("shared.json").exists(),
        "files inside claude-config must survive rm_cmd"
    );
}

#[test]
fn rm_cmd_succeeds_when_project_dir_absent() {
    // If the alias is registered but was never loaded (no project dir), rm must
    // still deregister cleanly — the dir removal is best-effort.
    use crate::commands::managed_root::ManagedPaths;
    use crate::commands::standalone::rm_cmd;
    use trusty_mpm::core::standalone::registry::ManagedRegistry;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let paths = ManagedPaths::from_root(root.clone());

    let mut reg = ManagedRegistry::load(&root).unwrap();
    reg.add("ghost", "https://github.com/org/repo", false)
        .unwrap();
    reg.save().unwrap();

    // No project dir exists (never loaded).
    assert!(!root.join("projects").join("ghost").exists());

    rm_cmd(&paths, "ghost").expect("rm_cmd must succeed even without project dir");

    let reg2 = ManagedRegistry::load(&root).unwrap();
    assert!(
        reg2.list().is_empty(),
        "registry must be empty after rm_cmd on unloaded alias"
    );
}

#[test]
fn update_cmd_errors_if_alias_not_in_registry() {
    use crate::commands::managed_root::ManagedPaths;
    use crate::commands::standalone::update_cmd;

    let tmp = tempfile::TempDir::new().unwrap();
    let paths = ManagedPaths::from_root(tmp.path().to_path_buf());

    let result = update_cmd(&paths, Some("missing-alias"));
    assert!(result.is_err(), "update_cmd must error when alias unknown");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("missing-alias"),
        "error must name the unknown alias"
    );
}

#[test]
fn update_cmd_errors_if_not_loaded() {
    // `tm update <alias>` where alias is registered but project dir does not
    // exist (never loaded) must error IMMEDIATELY with a message that hints to
    // run `tm load <alias>` first — not a generic end-of-loop bail.
    use crate::commands::managed_root::ManagedPaths;
    use crate::commands::standalone::update_cmd;
    use trusty_mpm::core::standalone::registry::ManagedRegistry;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let paths = ManagedPaths::from_root(root.clone());

    // Register the alias but do NOT create the project dir (never loaded).
    let mut reg = ManagedRegistry::load(&root).unwrap();
    reg.add("not-loaded", "https://github.com/org/repo", false)
        .unwrap();
    reg.save().unwrap();
    assert!(!root.join("projects").join("not-loaded").exists());

    let result = update_cmd(&paths, Some("not-loaded"));
    assert!(
        result.is_err(),
        "update_cmd must error when alias is registered but not yet loaded"
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("not-loaded"),
        "error must name the alias: {msg}"
    );
    assert!(
        msg.contains("tm load"),
        "error must hint to run `tm load`: {msg}"
    );
}

#[test]
fn update_cmd_all_skips_unloaded_returns_ok_when_none_loaded() {
    // `tm update` (no alias) with zero loaded projects should print a message
    // and return Ok.
    use crate::commands::managed_root::ManagedPaths;
    use crate::commands::standalone::update_cmd;
    use trusty_mpm::core::standalone::registry::ManagedRegistry;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let paths = ManagedPaths::from_root(root.clone());

    // Register an alias but never load it (no project dir).
    let mut reg = ManagedRegistry::load(&root).unwrap();
    reg.add("unloaded", "https://github.com/org/repo", false)
        .unwrap();
    reg.save().unwrap();

    // With no loaded projects, update_cmd should return Ok (nothing to do).
    let result = update_cmd(&paths, None);
    assert!(
        result.is_ok(),
        "update_cmd with no loaded aliases must return Ok"
    );
}

// ── WI-2 #1593: `tm sessctl` CLI parse round-trips ───────────────────────────

#[test]
fn cli_parses_sessctl_run() {
    // Why: `tm sessctl run <project-id>` must parse to Command::Sessctl with
    // SessctlAction::Run carrying the project_id.
    // What: assert parse round-trip for the run subcommand.
    // Test: direct parse via Cli::try_parse_from.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessctl", "run", "my-proj"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessctl {
            action: SessctlAction::Run {
                project_id, tmux, ..
            },
        } => {
            assert_eq!(project_id, "my-proj");
            assert!(!tmux);
        }
        other => panic!("expected sessctl run, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessctl_run_tmux() {
    // Why: `--tmux` must flip the backend to tmux.
    // What: assert `tmux == true` when the flag is present.
    // Test: direct parse via Cli::try_parse_from.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessctl", "run", "--tmux", "proj"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessctl {
            action: SessctlAction::Run { tmux, .. },
        } => assert!(tmux),
        other => panic!("expected sessctl run, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessctl_connect() {
    // Why: `tm sessctl connect <id>` must parse to SessctlAction::Connect.
    // What: assert the session_id field is populated.
    // Test: direct parse via Cli::try_parse_from.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessctl", "connect", "my-proj-0"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessctl {
            action: SessctlAction::Connect { session_id },
        } => assert_eq!(session_id, "my-proj-0"),
        other => panic!("expected sessctl connect, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessctl_stop() {
    // Why: `tm sessctl stop <id>` must parse to SessctlAction::Stop with
    // force=false by default.
    // What: assert force defaults to false.
    // Test: direct parse via Cli::try_parse_from.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessctl", "stop", "my-proj-0"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessctl {
            action: SessctlAction::Stop { session_id, force },
        } => {
            assert_eq!(session_id, "my-proj-0");
            assert!(!force);
        }
        other => panic!("expected sessctl stop, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessctl_stop_force() {
    // Why: `tm sessctl stop --force <id>` must set force=true.
    // What: assert force is true when the flag is present.
    // Test: direct parse via Cli::try_parse_from.
    let cli =
        Cli::try_parse_from(["trusty-mpm", "sessctl", "stop", "--force", "my-proj-0"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessctl {
            action: SessctlAction::Stop { force, .. },
        } => assert!(force),
        other => panic!("expected sessctl stop --force, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessctl_auth() {
    // Why: `tm sessctl auth <id>` must parse to SessctlAction::Auth.
    // What: assert the session_id field is populated.
    // Test: direct parse via Cli::try_parse_from.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessctl", "auth", "my-proj-0"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessctl {
            action: SessctlAction::Auth { session_id },
        } => assert_eq!(session_id, "my-proj-0"),
        other => panic!("expected sessctl auth, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessctl_list() {
    // Why: `tm sessctl list` must parse to SessctlAction::List with default
    // format="table" and no project filter.
    // What: assert defaults are applied.
    // Test: direct parse via Cli::try_parse_from.
    let cli = Cli::try_parse_from(["trusty-mpm", "sessctl", "list"]).unwrap();
    match cli.command.unwrap() {
        Command::Sessctl {
            action: SessctlAction::List { project, format },
        } => {
            assert!(project.is_none());
            assert_eq!(format, "table");
        }
        other => panic!("expected sessctl list, got {other:?}"),
    }
}

#[test]
fn cli_parses_sessctl_list_with_project_and_json() {
    // Why: `tm sessctl list --project foo --format json` must parse both flags.
    // What: assert project filter and json format are set.
    // Test: direct parse via Cli::try_parse_from.
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "sessctl",
        "list",
        "--project",
        "foo",
        "--format",
        "json",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Sessctl {
            action: SessctlAction::List { project, format },
        } => {
            assert_eq!(project.as_deref(), Some("foo"));
            assert_eq!(format, "json");
        }
        other => panic!("expected sessctl list, got {other:?}"),
    }
}
