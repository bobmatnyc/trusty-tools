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

// ── Regression tests for #1724 (guided-default fallback pollution guard) ──────

/// Why (#1724): the guided-default fallback MUST NOT deploy framework files
/// into a GitHub-backed git project's live checkout, even when the daemon is
/// unreachable. This test locks in that guarantee.
/// What: creates a temp directory, initialises a git repo inside it, sets a
/// fake GitHub remote URL, pre-creates a stub base clone (so `ensure_base_clone`
/// short-circuits without a network call), and calls `fallback_protected`
/// directly with the daemon set to an unreachable address. Asserts that no
/// framework files (`.trusty-mpm/`, `CLAUDE.md`, `.mcp.json`, `.claude/`) exist
/// in the temp directory after the call.
/// Test: this is the test. Annotated `serial` because it mutates `TRUSTY_MPM_REPOS_ROOT`.
#[tokio::test]
#[serial_test::serial]
async fn guided_fallback_never_pollutes_github_git_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    // Build a minimal git repository at `project` with a fake GitHub remote.
    // We use `git init` + `git remote add` so `get_origin_url` (which shells
    // out to `git config --get remote.origin.url`) returns a parseable URL.
    let git_init_ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(project)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !git_init_ok {
        // git binary unavailable in this environment; skip gracefully.
        eprintln!(
            "guided_fallback_never_pollutes_github_git_checkout: git not available, skipping"
        );
        return;
    }
    let _ = std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/trusty-ci-nonexistent/no-repo-1724.git",
        ])
        .current_dir(project)
        .status();

    // Pre-create a stub base clone so `ensure_base_clone` returns immediately
    // (it checks `base/.git` and returns Ok when found), avoiding any real
    // network call. `create_session_worktree` will fail because the stub is
    // not a real git repository — the error path is the one we want to test.
    //
    // SAFETY: env-var mutation is accepted by the project convention (same
    // pattern as crates/trusty-mpm/src/core/trusty_tools_config.rs tests).
    // The test mutates TRUSTY_MPM_REPOS_ROOT to a unique tempdir so it does
    // not conflict with concurrently-running tests on different paths.
    let repos_root = tempfile::tempdir().unwrap();
    let fake_base = repos_root
        .path()
        .join("trusty-ci-nonexistent")
        .join("no-repo-1724");
    std::fs::create_dir_all(fake_base.join(".git")).unwrap();

    let prev = std::env::var(trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV).ok();
    unsafe {
        std::env::set_var(
            trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV,
            repos_root.path(),
        );
    }

    // Call the protected fallback with an unreachable daemon URL and our fake
    // git project as the working directory.
    let client = reqwest::Client::new();
    let result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", project).await;

    // Restore the env var regardless of the outcome.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(
                trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV,
                v,
            ),
            None => {
                std::env::remove_var(trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV)
            }
        }
    }

    // ── Acceptance criterion (#1724) ─────────────────────────────────────────
    // None of the framework files that `prepare_session_with_style` writes must
    // appear inside the live git checkout.
    assert!(
        !project.join("CLAUDE.md").exists(),
        "#1724: CLAUDE.md must NOT be written to the live git checkout"
    );
    assert!(
        !project.join(".mcp.json").exists(),
        "#1724: .mcp.json must NOT be written to the live git checkout"
    );
    assert!(
        !project.join(".trusty-mpm").exists(),
        "#1724: .trusty-mpm dir must NOT be created in the live git checkout"
    );
    assert!(
        !project.join(".claude").exists(),
        "#1724: .claude dir must NOT be created in the live git checkout"
    );

    // The function must return Err (refused to deploy) rather than silently
    // falling through to a live-checkout deploy.
    assert!(
        result.is_err(),
        "#1724: fallback must Err rather than deploy to the live checkout; \
         got Ok which means framework files may have been written"
    );
}

/// Why (#1724 residual gap): the protected-checkout guarantee MUST hold for ANY
/// git project, not only GitHub-backed ones. A git repo with a non-GitHub remote
/// (e.g. a Gitea, GitLab, or bare SSH URL) or with NO remote configured must
/// also be refused — the fallback must return `Err` and leave the live working
/// tree untouched.
/// What: inits a real git repo, adds a non-GitHub remote URL, then calls
/// `fallback_protected` directly. Asserts that the four framework artifacts are
/// absent and the call returns `Err`.
/// Test: this is the test.
#[tokio::test]
async fn guided_fallback_blocks_non_github_git_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    // Build a minimal git repo with a non-GitHub (Gitea) remote.
    let git_init_ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(project)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !git_init_ok {
        eprintln!("guided_fallback_blocks_non_github_git_checkout: git not available, skipping");
        return;
    }
    // Add a non-GitHub remote to confirm it is not parsed as a GitHub project.
    let _ = std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://gitea.example.com/org/repo.git",
        ])
        .current_dir(project)
        .status();

    // Call the protected fallback with an unreachable daemon URL.
    let client = reqwest::Client::new();
    let result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", project).await;

    // ── Acceptance criterion (#1724 residual gap) ────────────────────────────
    // None of the framework files must appear in the live git checkout.
    assert!(
        !project.join("CLAUDE.md").exists(),
        "#1724: CLAUDE.md must NOT be written to a non-GitHub git checkout"
    );
    assert!(
        !project.join(".mcp.json").exists(),
        "#1724: .mcp.json must NOT be written to a non-GitHub git checkout"
    );
    assert!(
        !project.join(".trusty-mpm").exists(),
        "#1724: .trusty-mpm dir must NOT be created in a non-GitHub git checkout"
    );
    assert!(
        !project.join(".claude").exists(),
        "#1724: .claude dir must NOT be created in a non-GitHub git checkout"
    );

    // The fallback must refuse with Err, not silently deploy.
    assert!(
        result.is_err(),
        "#1724: fallback must Err for non-GitHub git checkout; \
         got Ok which means framework files may have been deployed"
    );

    // Verify it's the REFUSAL error, not a network-error from a clone attempt.
    // If is_github_remote() incorrectly accepts non-GitHub URLs, it would try
    // to clone from gitea.example.com and fail with a network error instead.
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("auto-managed clones require a GitHub remote")
            || err_msg.contains("live git checkout"),
        "#1724: error message must explain the protection; got: {err_msg}"
    );
}

/// Why (#1724 non-git path + #1839 Fix 2): the protected-checkout guarantee applies
/// only to git projects. A plain directory (no `.git`) should exit cleanly with a
/// helpful hint rather than attempting `launch()` which would fail with a confusing
/// daemon error. The fallback must NOT blindly block all directories, only git trees.
/// What: creates a temp dir with NO `.git`, calls `fallback_protected`. Since #1839
/// the non-git path returns `Ok(())` after printing a help hint to stderr — no
/// daemon contact is made and no framework files are created.
/// Test: this is the test.
#[tokio::test]
async fn guided_fallback_non_git_dir_reaches_launch_path() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    // No git init — plain directory.
    assert!(
        !project.join(".git").exists(),
        "test precondition: must NOT be a git repo"
    );

    // Call the protected fallback with any daemon URL — the non-git path now
    // exits cleanly without contacting the daemon (#1839 Fix 2).
    let client = reqwest::Client::new();
    let result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", project).await;

    // The result must be Ok(()) — no git repo means we print a help hint and exit 0.
    assert!(
        result.is_ok(),
        "#1839: non-git dir must exit cleanly (Ok) with a help hint; got: {result:?}"
    );

    // Framework files must NOT have been created.
    assert!(
        !project.join("CLAUDE.md").exists(),
        "CLAUDE.md must not exist for plain non-git directories"
    );
}

/// Why (#1724 HIGH gap): `fallback_protected` previously used `cwd.join(".git").exists()`
/// which only fires when `cwd` IS the repo root. Running `tm` from a subdirectory
/// (e.g. `~/project/src/`) silently bypassed the guard and called `launch(None)`,
/// resolving to `cwd` and deploying framework files into the live tree. This test
/// covers that scenario for a GitHub-backed project: the guard must detect the git
/// repo root at any depth and refuse to deploy.
/// What: creates a real git repo, creates a nested `src/module/` subdir, and calls
/// `fallback_protected` from within that subdir. Asserts no framework files appear
/// anywhere in the repo tree and that the call returns `Err`.
/// Test: this is the test.
#[tokio::test]
#[serial_test::serial]
async fn guided_fallback_blocks_github_git_from_subdirectory() {
    let repo_dir = tempfile::tempdir().unwrap();
    let git_init_ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(repo_dir.path())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !git_init_ok {
        eprintln!("guided_fallback_blocks_github_git_from_subdirectory: git unavailable, skipping");
        return;
    }
    let _ = std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/trusty-ci-nonexistent/subdir-test-1724.git",
        ])
        .current_dir(repo_dir.path())
        .status();

    // Run from a NESTED subdirectory inside the repo.
    let subdir = repo_dir.path().join("src").join("module");
    std::fs::create_dir_all(&subdir).unwrap();

    let client = reqwest::Client::new();
    let result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", &subdir).await;

    // ── Acceptance criterion (#1724 HIGH gap) ────────────────────────────────
    // No framework files must exist anywhere in the repo tree.
    assert!(
        !repo_dir.path().join("CLAUDE.md").exists(),
        "#1724: CLAUDE.md must NOT appear in repo root when called from subdir"
    );
    assert!(
        !subdir.join("CLAUDE.md").exists(),
        "#1724: CLAUDE.md must NOT appear in the subdir"
    );
    assert!(
        !repo_dir.path().join(".mcp.json").exists(),
        "#1724: .mcp.json must NOT appear in repo root"
    );
    assert!(
        !subdir.join(".mcp.json").exists(),
        "#1724: .mcp.json must NOT appear in the subdir"
    );
    // The call must return Err (refused or clone attempt failed — not deployed).
    assert!(
        result.is_err(),
        "#1724: fallback from subdir must Err, not deploy to the live tree"
    );
}

/// Why (#1724 success path): proves that when `redirect_to_managed_clone` succeeds
/// (base clone exists as a real git repo so `create_session_worktree` can create a
/// worktree), `launch()` is called with `Some(worktree)` — not `None` — so
/// framework files go to the worktree, never to the live checkout.
/// What: (1) creates a real git base clone (git init + empty commit); (2) sets
/// `TRUSTY_MPM_REPOS_ROOT` to point at it; (3) creates a live checkout with a
/// matching GitHub remote; (4) calls `fallback_protected`; (5) asserts live
/// checkout is clean AND at least one per-session worktree was created under the
/// base clone (proving `launch(Some(worktree))` was invoked, not `launch(None)`).
/// Test: this is the test. Annotated `serial` because it mutates `TRUSTY_MPM_REPOS_ROOT`.
#[tokio::test]
#[serial_test::serial]
async fn guided_fallback_redirect_success_worktree_not_live_checkout() {
    // ── Set up a real git base clone (minimal: init + one empty commit) ──────
    let repos_root = tempfile::tempdir().unwrap();
    // Repo path must match parse_github_path("https://github.com/test-owner-1724/test-repo-1724.git")
    let base = repos_root
        .path()
        .join("test-owner-1724")
        .join("test-repo-1724");
    std::fs::create_dir_all(&base).unwrap();

    let git_init_ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(&base)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !git_init_ok {
        eprintln!(
            "guided_fallback_redirect_success_worktree_not_live_checkout: git unavailable, skipping"
        );
        return;
    }
    // Configure minimal git identity so commit doesn't fail.
    let _ = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "config",
            "user.email",
            "ci@test.invalid",
        ])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C", base.to_str().unwrap(), "config", "user.name", "CI"])
        .status();
    // One empty commit gives us a HEAD branch so `git worktree add` succeeds.
    let commit_ok = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !commit_ok {
        eprintln!(
            "guided_fallback_redirect_success_worktree_not_live_checkout: git commit failed, skipping"
        );
        return;
    }

    // ── Set up a live checkout with the matching GitHub remote ───────────────
    let live_dir = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .arg("init")
        .current_dir(live_dir.path())
        .status();
    let _ = std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/test-owner-1724/test-repo-1724.git",
        ])
        .current_dir(live_dir.path())
        .status();

    // ── Point REPOS_ROOT at our temp dir (RAII: restore on scope exit) ───────
    let repos_root_key = trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV;
    let prev_repos_root = std::env::var(repos_root_key).ok();
    unsafe { std::env::set_var(repos_root_key, repos_root.path()) };

    let client = reqwest::Client::new();
    let _result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", live_dir.path())
            .await;

    // Restore env var immediately (before assertions that might panic).
    unsafe {
        match prev_repos_root {
            Some(ref v) => std::env::set_var(repos_root_key, v),
            None => std::env::remove_var(repos_root_key),
        }
    }

    // ── Acceptance criteria (#1724 success path) ─────────────────────────────
    // Live checkout must be untouched.
    assert!(
        !live_dir.path().join("CLAUDE.md").exists(),
        "#1724: CLAUDE.md must NOT appear in the live checkout after redirect"
    );
    assert!(
        !live_dir.path().join(".mcp.json").exists(),
        "#1724: .mcp.json must NOT appear in the live checkout after redirect"
    );
    assert!(
        !live_dir.path().join(".trusty-mpm").exists(),
        "#1724: .trusty-mpm must NOT be created in the live checkout"
    );
    assert!(
        !live_dir.path().join(".claude").exists(),
        "#1724: .claude must NOT be created in the live checkout"
    );
    // A per-session worktree must have been created inside the base clone,
    // proving `launch(Some(worktree))` was called, not `launch(None)`.
    let worktrees_dir = base.join(".worktrees");
    assert!(
        worktrees_dir.exists(),
        "#1724 #1803: .worktrees/ dir must exist under base clone after successful redirect"
    );
    let sessions: Vec<_> = std::fs::read_dir(&worktrees_dir)
        .expect(".worktrees dir must be listable")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !sessions.is_empty(),
        "#1724 #1803: at least one per-session worktree must be present under .worktrees/, \
         proving launch(Some(worktree)) was invoked rather than launch(None)"
    );
}
