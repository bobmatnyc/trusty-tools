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

use crate::cli::{AuthAction, Cli, Command, RepairAction, ServicesAction, SessctlAction};
use crate::commands::session::{
    compose_session_instructions, compose_session_instructions_with_roster,
};

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
    // `tm connect` is the live-checkout session starter; it takes an
    // optional project directory, exactly like `tm launch`.
    let cli = Cli::try_parse_from(["trusty-mpm", "connect"]).unwrap();
    match cli.command.unwrap() {
        Command::Connect { dir } => assert_eq!(dir, None),
        other => panic!("expected Connect, got {other:?}"),
    }
    // Issue #2230: `connect` must carry the same PM system-prompt +
    // isolation-flag carrier every other launch path has — no more bare
    // `claude --dangerously-skip-permissions` with nothing injected.
    assert_connect_claude_cmd_carries_persona_flags();
}

#[test]
fn cli_parses_connect_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "connect", "/work/p"]).unwrap();
    match cli.command.unwrap() {
        Command::Connect { dir } => assert_eq!(dir.as_deref(), Some("/work/p")),
        other => panic!("expected Connect, got {other:?}"),
    }
    assert_connect_claude_cmd_carries_persona_flags();
}

/// Assert `connect`'s composed `claude` invocation carries both the injected
/// PM system prompt and the shared session-isolation flag (issue #2230).
///
/// Why: `crate::commands::launch::connect_claude_cmd` is the exact function
/// `connect()` calls to build the command sent to `tmux send-keys`; asserting
/// on it directly proves the wiring without needing a live daemon/tmux to
/// drive the full async `connect()` end-to-end.
fn assert_connect_claude_cmd_carries_persona_flags() {
    let path = std::path::Path::new("/tmp/trusty-mpm-connect-test-prompt.txt");

    // #4181: `connect` now relocates `CLAUDE_CONFIG_DIR` when the tm-owned config
    // home resolves, and falls back to the pre-#4181 shape when it does not.
    // Both shapes must carry the prompt and an isolation flag, so assert on both
    // rather than narrowing this to whichever one the test machine happens to
    // produce.
    let fallback = crate::commands::launch::connect_claude_cmd(Some(path), None, &[]);
    assert!(
        fallback.contains("--append-system-prompt-file"),
        "connect claude_cmd must inject the PM system prompt file: {fallback}"
    );
    assert!(
        fallback.contains("--setting-sources project,local"),
        "with no config dir the user tier is the operator's own ~/.claude and \
         must stay excluded (#1269): {fallback}"
    );

    let dir = std::path::Path::new("/tm/claude-config");
    let relocated = crate::commands::launch::connect_claude_cmd(Some(path), Some(dir), &[]);
    assert!(
        relocated.contains("--append-system-prompt-file"),
        "connect claude_cmd must inject the PM system prompt file: {relocated}"
    );
    assert!(
        relocated.contains("CLAUDE_CONFIG_DIR='/tm/claude-config'"),
        "the relocated shape must redirect the user tier to the tm-owned home: \
         {relocated}"
    );
    assert!(
        relocated.contains("--setting-sources user,project,local"),
        "the relocated shape loads the user tier it just redirected: {relocated}"
    );
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

/// A fixture agent roster, rendered exactly as the live tiers would render it.
///
/// Why (#5544, #4937): the two convergence tests below compare two independently
/// composed prompts, and each composition rescanned the agent roster from tiers
/// rooted in `$HOME` and `$CLAUDE_CONFIG_DIR` — machine-global mutable state, so
/// the two scans could legitimately disagree and the `assert_eq!` failed with a
/// message indistinguishable from a real prompt regression. On this workstation
/// `~/.claude/agents` is the only tier carrying `copyeditor`, `pangram-editor`,
/// `proofreader`, `writer` and `writing-critic`, which is why the diff was
/// always those exact five names and why CI — which has no ambient tier at all —
/// never reproduced it.
///
/// The previous revision pinned `$HOME` and `$CLAUDE_CONFIG_DIR` around the test
/// and took `#[serial]`. That treated the symptom with the disease: a process
/// env write is visible to EVERY test in this binary for its whole duration, and
/// `#[serial]` excludes only other `#[serial]` tests. Injecting one roster into
/// both sides of the comparison removes the shared global entirely, so these
/// tests neither write env nor need serialising.
/// What: seeds one agent into a tempdir tier and renders it through
/// `roster_section_from_dirs` — the same function the live path uses — returning
/// the rendered `## Delegation Authority` block. The `TempDir` is returned so the
/// caller keeps it alive.
/// Test: used by `compose_session_instructions_display_matches_live_prompt` and
/// `compose_session_instructions_display_matches_live_prompt_with_override`.
fn fixture_roster_section() -> (tempfile::TempDir, String) {
    let tiers = tempfile::tempdir().expect("fixture tier root");
    let agents = tiers.path().join("agents");
    std::fs::create_dir_all(&agents).expect("fixture tier");
    // One agent, so the roster-present composer path is the one under test —
    // the same branch the ambient roster used to select.
    std::fs::write(
        agents.join("engineer.md"),
        "---\nname: engineer\nrole: engineer\nmodel: sonnet\n---\n\n# Engineer\n",
    )
    .expect("seed fixture agent");

    let section = trusty_mpm::core::delegation_authority::roster_section_from_dirs(&[agents])
        .expect("a seeded tier renders a roster section");
    (tiers, section)
}

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

    let (display, _output, stash_path) =
        compose_session_instructions(project).expect("compose succeeds");

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
    // What: runs both composers on the same empty project directory with the
    // SAME injected roster and asserts the outputs match.
    // #5544: the roster is injected rather than rescanned per side — see
    // `fixture_roster_section`. No process env is touched, so this test needs
    // no `#[serial]`.
    // Test: any future change that re-introduces the #382 divergence will
    // break this test immediately.
    let (_tiers, roster) = fixture_roster_section();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();

    let (display, _output, _stash) =
        compose_session_instructions_with_roster(project, Some(roster.clone()))
            .expect("compose succeeds");

    let live_prompt = trusty_mpm::core::session_launch::build_system_prompt_for_with_roster(
        project,
        Some(roster),
    );

    assert_eq!(
        display, live_prompt,
        "tm session instructions output must match the live launch prompt (issue #382)"
    );
    assert!(
        display.contains("### engineer"),
        "the injected fixture roster must reach the composed prompt, or this \
         test is no longer comparing the roster-present composer path"
    );
}

#[test]
fn compose_session_instructions_display_matches_live_prompt_with_override() {
    // Why: the same convergence guarantee must hold when project-level override
    // files are present — the stash and the display must reflect the override,
    // not the bundled defaults.
    // What: writes a `CLAUDE.md` WORKFLOW named section (#4286 — the retired
    // `.trusty-mpm/WORKFLOW.md` file this used to write is no longer read),
    // then asserts the display and the live prompt both include it (and don't
    // include the bundled heading).
    // #5544: one injected roster feeds both sides — see the test above.
    // Test: if `compose_session_instructions` stops reading overrides for the
    // display path, this test fails.
    let (_tiers, roster) = fixture_roster_section();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();

    std::fs::write(
        project.join("CLAUDE.md"),
        "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n\
         # Custom Workflow\n\nCOMPOSE_OVERRIDE_MARKER\n\
         <!-- TRUSTY-MPM: WORKFLOW END -->\n",
    )
    .unwrap();

    let (display, _output, _stash) =
        compose_session_instructions_with_roster(project, Some(roster.clone()))
            .expect("compose succeeds");

    let live_prompt = trusty_mpm::core::session_launch::build_system_prompt_for_with_roster(
        project,
        Some(roster),
    );

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

// ── issue #2246: `tm auth` CLI parse tests ──────────────────────────────────

#[test]
fn cli_parses_auth_set_token() {
    let cli = Cli::try_parse_from(["trusty-mpm", "auth", "set-token", "--token", "abc"]).unwrap();
    match cli.command.unwrap() {
        Command::Auth {
            action: AuthAction::SetToken { token, stdin },
        } => {
            assert_eq!(token.as_deref(), Some("abc"));
            assert!(!stdin, "stdin flag must default to false");
        }
        other => panic!("expected Auth {{ SetToken }}, got {other:?}"),
    }
}

#[test]
fn cli_parses_auth_set_token_stdin() {
    // No --token: the handler reads from stdin. --stdin is accepted for
    // clarity but does not change behaviour (see AuthAction::SetToken docs).
    let cli = Cli::try_parse_from(["trusty-mpm", "auth", "set-token", "--stdin"]).unwrap();
    match cli.command.unwrap() {
        Command::Auth {
            action: AuthAction::SetToken { token, stdin },
        } => {
            assert_eq!(token, None);
            assert!(stdin);
        }
        other => panic!("expected Auth {{ SetToken }}, got {other:?}"),
    }
}

#[test]
fn cli_parses_auth_clear_token() {
    let cli = Cli::try_parse_from(["trusty-mpm", "auth", "clear-token"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Auth {
            action: AuthAction::ClearToken
        }
    ));
}

#[test]
fn cli_parses_auth_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "auth", "status"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Auth {
            action: AuthAction::Status
        }
    ));
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

/// Why (#2534): bare `tm` run from a directory that physically sits inside a
/// GitHub repo's working tree but is NOT part of its tracked tree (the same shape
/// as the APFS `~/Duetto/cto` case-fold collision) must NOT inherit the enclosing
/// repo's identity. It must NOT redirect to that repo's managed clone (which would
/// attempt a real `git clone` of the github.com URL and fail on the network); it
/// must route to the clean non-git fallback and return `Ok(())` without writing
/// any framework files.
/// What: inits a real git repo with a committed file + a github.com remote, then
/// creates an UNTRACKED subdir inside it and calls `fallback_protected` from that
/// subdir. Asserts `Ok(())`, no framework artifacts, and no managed-clone attempt.
/// Test: this is the test.
#[tokio::test]
async fn guided_fallback_untracked_ancestor_does_not_redirect() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    // git init + identity + an initial commit (so HEAD's tree exists).
    let git_ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(project)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !git_ok {
        eprintln!(
            "guided_fallback_untracked_ancestor_does_not_redirect: git unavailable, skipping"
        );
        return;
    }
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
        let _ = std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(project)
            .status();
    }
    std::fs::write(project.join("README.md"), b"x").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(project)
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "init", "-q"])
        .current_dir(project)
        .status();
    // A github.com remote — if the ancestor were (wrongly) trusted, the fallback
    // would try to clone THIS into the managed workspace.
    let _ = std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ])
        .current_dir(project)
        .status();

    // An UNTRACKED sibling directory inside the working tree — never `git add`ed.
    let untracked = project.join("cto");
    std::fs::create_dir_all(&untracked).unwrap();

    let client = reqwest::Client::new();
    let result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", &untracked)
            .await;

    // Must exit cleanly (non-git fallback), NOT Err (non-github refusal) and NOT
    // a redirect (which would network-fail cloning the github remote).
    assert!(
        result.is_ok(),
        "#2534: untracked ancestor subdir must route to the clean non-git \
         fallback (Ok), not the ancestor's managed-clone redirect; got: {result:?}"
    );
    // No framework artifacts anywhere.
    for p in ["CLAUDE.md", ".mcp.json", ".trusty-mpm", ".claude"] {
        assert!(
            !untracked.join(p).exists(),
            "#2534: {p} must NOT be written to the untracked directory"
        );
        assert!(
            !project.join(p).exists(),
            "#2534: {p} must NOT be written to the enclosing repo"
        );
    }
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

// ── #4061: managed-pane settle race vs. the non-git fallback ─────────────────
//
// Root cause: `try_inplace_relaunch` (the FIRST thing bare `tm` tries, at the
// top of `run_guided_default`) is gated on the daemon's session record
// actually reading "stopped" within a small bounded retry budget
// (`FETCH_RETRY_BUDGET`, ~400ms) — the async `SessionEnd` healing step that
// flips a just-exited managed pane's record from "active" to "stopped" can
// still be in flight at that exact moment. When that gate misses, control
// used to fall straight through project detection to `fallback_protected`'s
// `CwdProject::NotGit` arm, which printed the "not in a git project" hint —
// actively misleading for a pane that plainly IS (or very recently was) a
// managed session's own pane. These tests exercise `fallback_protected`
// directly (the layer the fix lives in) with `TM_MANAGED_SESSION_ID` present,
// mirroring the exact "operator's shell already has the env var exported"
// scenario from #2023 component B.

#[tokio::test]
#[serial_test::serial]
async fn guided_fallback_non_git_dir_no_managed_env_is_fast() {
    // Why: the common case — no managed-session signal in the environment at
    // all — must take the ordinary, unaffected fast path: no extra daemon
    // round trip, no added latency (#4061 must not slow down bare `tm` for a
    // genuinely non-managed pane).
    let managed_key = "TM_MANAGED_SESSION_ID";
    let prev = std::env::var(managed_key).ok();
    // SAFETY: serialised by `#[serial_test::serial]`.
    unsafe { std::env::remove_var(managed_key) };
    let prev_tmux = std::env::var("TMUX").ok();
    // SAFETY: serialised by `#[serial_test::serial]`.
    unsafe { std::env::remove_var("TMUX") };

    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    assert!(!project.join(".git").exists(), "test precondition");

    let client = reqwest::Client::new();
    let start = std::time::Instant::now();
    let result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", project).await;
    let elapsed = start.elapsed();

    // Restore before any assertion can panic.
    unsafe {
        match prev {
            Some(ref v) => std::env::set_var(managed_key, v),
            None => std::env::remove_var(managed_key),
        }
        match prev_tmux {
            Some(ref v) => std::env::set_var("TMUX", v),
            None => std::env::remove_var("TMUX"),
        }
    }

    assert!(
        result.is_ok(),
        "non-git dir must exit cleanly; got {result:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(150),
        "no managed-session signal must never pay the #4061 retry budget; took {elapsed:?}"
    );
}

/// Local HTTP mock replying `{"id","name","state"}`, walking through `states`
/// one entry per connection (clamping to the last once exhausted) — mirrors
/// `commands::guided_inplace::tests::spawn_state_mock`'s convention (that
/// helper lives in a sibling test module and is not reachable from this file,
/// hence the small local copy).
async fn spawn_4061_state_mock(
    id: &'static str,
    states: Vec<&'static str>,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_task = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let n = hits_task.fetch_add(1, Ordering::SeqCst);
            // Drain the request before replying (avoids connection-reset
            // flakiness on a naive single read).
            let mut buf = [0u8; 4096];
            loop {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    sock.read(&mut buf),
                )
                .await
                {
                    Ok(Ok(n)) if n > 0 => {
                        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            let idx = n.min(states.len().saturating_sub(1));
            let state = states.get(idx).copied().unwrap_or("active");
            let body = format!(r#"{{"id":"{id}","name":"tm-test-4061","state":"{state}"}}"#);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), hits)
}

#[tokio::test]
#[serial_test::serial]
async fn guided_fallback_non_git_dir_with_managed_env_settles_quickly_returns_promptly() {
    // #4061 race, reproduced: a managed-session id IS present in the
    // environment, and the daemon record settles to "stopped" on the very
    // next poll (simulating the `SessionEnd` healing step catching up a
    // beat after the first bare-`tm` attempt already gave up). The retried
    // `try_inplace_relaunch` this fix adds must observe that promptly —
    // never exhausting the full retry budget once the state resolves — and
    // must exit cleanly rather than ever printing the misleading "not in a
    // git project" hint for this pane.
    const TEST_ID: &str = "11111111-2222-3333-4444-555555555555";
    let (url, hits) = spawn_4061_state_mock(TEST_ID, vec!["active", "stopped"]).await;

    let managed_key = "TM_MANAGED_SESSION_ID";
    let prev = std::env::var(managed_key).ok();
    // SAFETY: serialised by `#[serial_test::serial]`.
    unsafe { std::env::set_var(managed_key, TEST_ID) };

    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    let client = reqwest::Client::new();
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::commands::guided::fallback_protected(&client, &url, project),
    )
    .await;
    let elapsed = start.elapsed();

    // Restore before any assertion can panic.
    unsafe {
        match prev {
            Some(ref v) => std::env::set_var(managed_key, v),
            None => std::env::remove_var(managed_key),
        }
    }

    let result = result.expect("#4061: fallback_protected must never hang");
    assert!(
        result.is_ok(),
        "a managed pane whose record settles must still exit cleanly; got {result:?}"
    );
    assert!(
        !project.join("CLAUDE.md").exists(),
        "CLAUDE.md must not be written for this non-git cwd"
    );
    assert!(
        hits.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "the retried in-place check must actually query the daemon"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "a record that settles on the second poll must not pay the full \
         retry budget; took {elapsed:?}"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn guided_fallback_non_git_dir_with_managed_env_unreachable_daemon_does_not_hang() {
    // #4061: even in the worst case — a managed-session id present but the
    // daemon unreachable (or the record never settling) — the fallback must
    // still resolve promptly (bounded by the SAME small retry budget
    // `try_inplace_relaunch` already uses) rather than hang, and must still
    // exit cleanly.
    const TEST_ID: &str = "11111111-2222-3333-4444-555555555555";
    let managed_key = "TM_MANAGED_SESSION_ID";
    let prev = std::env::var(managed_key).ok();
    // SAFETY: serialised by `#[serial_test::serial]`.
    unsafe { std::env::set_var(managed_key, TEST_ID) };

    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();

    let client = reqwest::Client::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", project),
    )
    .await;

    // Restore before any assertion can panic.
    unsafe {
        match prev {
            Some(ref v) => std::env::set_var(managed_key, v),
            None => std::env::remove_var(managed_key),
        }
    }

    let result = result.expect("#4061: fallback_protected must never hang, even unreachable");
    assert!(
        result.is_ok(),
        "an unreachable daemon must still exit cleanly; got {result:?}"
    );
    assert!(
        !project.join("CLAUDE.md").exists(),
        "CLAUDE.md must not be written for this non-git cwd"
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
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
        let _ = std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(repo_dir.path())
            .status();
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

    // Run from a NESTED, TRACKED subdirectory inside the repo. The subdir is
    // committed so it genuinely belongs to the repo's tracked tree — as of #2534
    // an UNTRACKED ancestor subdir instead routes to the clean non-git fallback
    // (covered by `guided_fallback_untracked_ancestor_does_not_redirect`).
    let subdir = repo_dir.path().join("src").join("module");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(subdir.join(".keep"), b"").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo_dir.path())
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "init", "-q"])
        .current_dir(repo_dir.path())
        .status();

    // ── Hermetic base-clone root (#3450) ─────────────────────────────────────
    // Without this override, `fallback_protected` → `ensure_base_clone` resolves
    // `repos_root()` to its PRODUCTION default (`~/trusty-mpm-projects`) and
    // attempts a real (failing) network clone of the fake GitHub URL above,
    // depositing `trusty-ci-nonexistent/subdir-test-1724(.old-layout-backup-*)`
    // directly into the operator's real projects root — every single run, on
    // every machine, regardless of `$TMPDIR`. Mirrors the stub-base-clone
    // pattern in `guided_fallback_never_pollutes_github_git_checkout` above:
    // pre-create a fake `.git` marker so `ensure_base_clone` short-circuits
    // without ever touching the network, and `create_session_worktree` then
    // fails (not a real repo) — the error path this test wants to observe.
    //
    // SAFETY: env-var mutation is accepted by the project convention (same
    // pattern as the sibling tests in this file); `#[serial_test::serial]`
    // above prevents cross-test races on the process-global env var.
    let repos_root = tempfile::tempdir().unwrap();
    let fake_base = repos_root
        .path()
        .join("trusty-ci-nonexistent")
        .join("subdir-test-1724");
    std::fs::create_dir_all(fake_base.join(".git")).unwrap();

    let prev = std::env::var(trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV).ok();
    unsafe {
        std::env::set_var(
            trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV,
            repos_root.path(),
        );
    }

    let client = reqwest::Client::new();
    let result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", &subdir).await;

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

/// Why (#1724 success path): proves that when `launch_protected_workspace` succeeds
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

    let _repos_root_env = ReposRootEnv::set(repos_root.path());

    let client = reqwest::Client::new();
    let _result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", live_dir.path())
            .await;

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

/// The guided fallback prepares the session IN the worktree it provisioned,
/// not in the shared base clone (#5836 critic HIGH).
///
/// Why: `guided_fallback_redirect_success_worktree_not_live_checkout` above
/// asserts only that a worktree EXISTS under `<base>/.worktrees`, and
/// `provision_for_fallback` creates that worktree before `launch()` runs — so
/// that test passes whether the session then runs in the worktree or somewhere
/// else entirely. It stayed green while `provision_for_launch`'s managed-checkout
/// redirect discarded the worktree and collapsed the placement onto `<base>`,
/// which puts two concurrent daemon-unreachable sessions in one tree.
/// What: same fixture as the test above, then asserts on the DIRECTORY the
/// session was prepared in — `prepare_isolated_session` deploys `.claude/` into
/// the placement `launch()` resolved, so the worktree must carry it and the base
/// clone must not.
/// Test: this is the test. RED at 34da769f: `.claude` appeared in `<base>`.
#[tokio::test]
#[serial_test::serial]
async fn guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone() {
    let origin = "https://github.com/test-owner-5836/test-repo-5836.git";
    let repos_root = tempfile::tempdir().unwrap();
    // Canonical because `find_git_root` resolves the macOS `/var` symlink and
    // the placement rule compares paths by plain equality (ADR-0037).
    let repos_root_path = repos_root.path().canonicalize().unwrap();
    // Must match `parse_github_path(origin)`.
    let base = repos_root_path
        .join("test-owner-5836")
        .join("test-repo-5836");
    fallback_git_repo(&base);
    // The base clone carries the origin its worktrees inherit — without it
    // `launch()` stops at "no git origin remote found" and never reaches
    // placement at all.
    fallback_git_remote(&base, origin);

    let live_dir = tempfile::tempdir().unwrap();
    fallback_git_repo(live_dir.path());
    fallback_git_remote(live_dir.path(), origin);

    let _repos_root_env = ReposRootEnv::set(&repos_root_path);

    let client = reqwest::Client::new();
    let _result =
        crate::commands::guided::fallback_protected(&client, "http://127.0.0.1:1", live_dir.path())
            .await;

    // Exactly one worktree, and it is the directory the session must run in.
    let worktrees_dir = base.join(".worktrees");
    let mut sessions: Vec<std::path::PathBuf> = std::fs::read_dir(&worktrees_dir)
        .expect(".worktrees must be listable — the fallback provisions it")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    sessions.sort();
    assert_eq!(
        sessions.len(),
        1,
        "the fallback provisions exactly one per-session worktree, found {sessions:?}"
    );
    let worktree = &sessions[0];

    assert!(
        !base.join(".claude").exists(),
        "the shared base clone must NOT become the session directory — the \
         fallback already resolved placement to {}",
        worktree.display()
    );
    assert!(
        !base.join("CLAUDE.md").exists(),
        "the shared base clone must NOT be deployed into: {}",
        base.join("CLAUDE.md").display()
    );
    assert!(
        worktree.join(".claude").is_dir(),
        "the session must be prepared in the worktree the fallback provisioned: {} \
         has no .claude",
        worktree.display()
    );
}

/// A git repository with one empty commit at `path`, created or asserted.
///
/// Why: `git worktree add` needs a HEAD commit, and a fixture that cannot be
/// built is a test failure rather than a skip — the older fallback tests above
/// return early when git is missing, which hides a broken fixture as a pass.
/// What: `git init` + identity + `commit --allow-empty`, asserting each step.
/// Test: used by `guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone`.
fn fallback_git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
        assert!(
            status.success(),
            "git {args:?} failed in {}",
            path.display()
        );
    };
    run(&["init"]);
    run(&["config", "user.email", "ci@test.invalid"]);
    run(&["config", "user.name", "CI"]);
    run(&["commit", "--allow-empty", "-m", "init"]);
}

/// RAII override of `TRUSTY_MPM_REPOS_ROOT`, restored on drop (unwind included).
///
/// Why: the two fallback tests here each open-coded a set + two-arm restore, and
/// each restored BEFORE its assertions so a panicking assertion could not leak
/// the variable — six process-global writes to work around a problem `Drop`
/// does not have. One guard covers both and shrinks this file's
/// `ENV_MUTATION_BUDGET` row rather than growing it.
/// What: sets the var on construction, restores the previous value (or removes
/// it) on drop. Callers MUST be `#[serial_test::serial]` — the variable is
/// process-global.
/// Test: used by the two `guided_fallback_*` tests above.
struct ReposRootEnv {
    prev: Option<std::ffi::OsString>,
}

impl ReposRootEnv {
    fn set(path: &std::path::Path) -> Self {
        let key = trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV;
        let prev = std::env::var_os(key);
        // SAFETY: every caller is `#[serial_test::serial]`, so no other test
        // thread races this set/restore.
        unsafe { std::env::set_var(key, path) };
        Self { prev }
    }
}

impl Drop for ReposRootEnv {
    fn drop(&mut self) {
        let key = trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV;
        // SAFETY: see `set`.
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}

/// Point `path`'s `origin` remote at `url`.
fn fallback_git_remote(path: &std::path::Path, url: &str) {
    let status = std::process::Command::new("git")
        .args(["remote", "add", "origin", url])
        .current_dir(path)
        .status()
        .expect("git remote add must spawn");
    assert!(
        status.success(),
        "git remote add failed in {}",
        path.display()
    );
}

/// Issue #4203: every CLI path whose spawn carries `--setting-sources
/// project,local` must deploy through `prepare_isolated_session`, never through
/// a self-resolved `FrameworkPaths::default()`.
///
/// Why a SOURCE-TEXT guard rather than a behavioural one: `launch()`,
/// `connect()`, and `launch_and_wait()` are async functions that create tmux
/// sessions, register with the daemon over HTTP, and then block on
/// `attach-session`, so none of them is reachable from a unit test. A test that
/// exercises the deploy layout alone proves the layout is right while staying
/// completely silent if a call site stops using it — which is exactly how the
/// first version of this guard shipped green with the bug reintroduced in both
/// call sites. Binding the CALL SITES is the whole point, so the guard reads the
/// call sites. Source-text guards are the established idiom here
/// (`scripts/check_line_cap.sh` and the tracked-file CI guards are the same
/// shape).
///
/// What: asserts neither file constructs `FrameworkPaths::default()`, and that
/// each still routes through the isolated seam the expected number of times.
/// The counts are deliberately exact — a call site deleted or a fourth one added
/// without thought fails here and forces the author to confirm its spawn posture.
#[test]
fn launch_paths_prepare_through_the_isolated_seam() {
    // `include_str!` is relative to THIS file (`src/bin/tm/`).
    const LAUNCH_SRC: &str = include_str!("commands/launch.rs");
    const META_LAUNCH_SRC: &str = include_str!("commands/meta/launch.rs");

    for (name, src, expected_calls) in [
        ("commands/launch.rs", LAUNCH_SRC, 2usize),
        ("commands/meta/launch.rs", META_LAUNCH_SRC, 1usize),
    ] {
        assert!(
            !src.contains("FrameworkPaths::default()"),
            "{name} spawns `claude` with a `--setting-sources` flag \
             (`{}` when it cannot relocate `CLAUDE_CONFIG_DIR`, `{}` when it can \
             — #4181), and the non-relocated one excludes the `user` tier \
             `FrameworkPaths::default()` deploys into — deploy via \
             `session_launch::prepare_isolated_session` instead (issue #4203)",
            trusty_mpm::core::model_inject::SETTING_SOURCES_FLAG,
            trusty_mpm::core::model_inject::SETTING_SOURCES_FLAG_RELOCATED
        );
        assert_eq!(
            src.matches("prepare_isolated_session(").count(),
            expected_calls,
            "{name} must deploy through `prepare_isolated_session` exactly \
             {expected_calls}x (issue #4203); if a call site was added or removed, \
             confirm its spawn's `--setting-sources` posture and update this count"
        );
    }

    // `commands/session/start.rs` is deliberately NOT in the list above: it
    // spawns a bare `claude {PERMISSION_MODE_FLAG}` with no `--setting-sources`,
    // so Claude Code reads its default tiers INCLUDING `user` and
    // `FrameworkPaths::default()` is correct there. Pin that premise — if
    // `start.rs` ever gains the flag, it acquires this defect and this
    // assertion is what says so.
    const START_SRC: &str = include_str!("commands/session/start.rs");
    assert!(
        !START_SRC.contains("SETTING_SOURCES_FLAG"),
        "commands/session/start.rs now carries a `--setting-sources` flag, so its \
         `FrameworkPaths::default()` deploy has become the #4203 defect — route it \
         through `prepare_isolated_session` and add it to the list above"
    );
}

/// The default must touch NO binary and clobber NOTHING — a `tm reinstall`
/// typed with no flags refreshes assets only.
#[test]
fn cli_parses_reinstall() {
    let cli = Cli::try_parse_from(["trusty-mpm", "reinstall"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Reinstall(crate::cli::ReinstallArgs {
            force: false,
            binary: false,
            yes: false
        })
    ));
}

/// `--yes` only means "skip the binary prompt", so clap must reject it on its
/// own rather than let it read as blanket consent.
#[test]
fn cli_parses_reinstall_binary() {
    let cli = Cli::try_parse_from(["trusty-mpm", "reinstall", "--binary", "--yes"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Reinstall(crate::cli::ReinstallArgs {
            binary: true,
            yes: true,
            ..
        })
    ));
    assert!(Cli::try_parse_from(["trusty-mpm", "reinstall", "--yes"]).is_err());
}
