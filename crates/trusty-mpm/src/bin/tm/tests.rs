//! CLI parse tests and formatter unit tests for `tm` / `trusty-mpm`.
//!
//! Why: tests live in dedicated files so main.rs stays thin. This file
//! covers short-id / banner / formatter tests and the first half of the
//! CLI parse round-trips. Install / hook / session / services / compose
//! tests live in `tests_behavior_a.rs` and `tests_behavior_b_tests.rs`; the
//! managed session-manager verb surface (`session new`/`ls`/`activity`/
//! `send`/`answer`/`attach`/lifecycle aliases/`prune*`/`catchup`) lives in
//! `tests_behavior_d_tests.rs` (split out under the #1916 line-cap rebalance).
//! What: parse round-trips for short_id, banners, daemon, project, session
//! subcommands (up through daemon flag parsing).
//! Test: `cargo test -p trusty-mpm` to run all fast tests.

use clap::Parser;

use crate::cli::{
    CatalogAction, Cli, Command, DEFAULT_URL, McpCmd, McpTransportArg, MetaAction, ProjectAction,
    SessionAction, SlackCmd, TelegramCmd,
};
use crate::commands::misc::{CATALOG_STALE_MSG, CATALOG_UNKNOWN_MSG, NON_GIT_FALLBACK_HINT};
use crate::formatters::banner::{
    fallback_session_name, normalize_workdir, print_launch_banner,
    print_launch_banner_reconnecting, terminal_width,
};
use crate::formatters::session::{deploy_summary_line, short_id};

#[test]
fn deploy_summary_line_formats_counts() {
    // #1917: `session start` prints this exact shape for both the Agents and
    // the (previously missing) Skills deploy summary lines.
    assert_eq!(
        deploy_summary_line("Skills", 3, 1, 2),
        "Skills: 3 deployed, 1 skipped, 2 unchanged"
    );
    assert_eq!(
        deploy_summary_line("Agents", 0, 0, 0),
        "Agents: 0 deployed, 0 skipped, 0 unchanged"
    );
}

#[test]
fn short_id_extracts_uuid_prefix() {
    // SessionId newtype shape `{"0": "<uuid>"}` → first 8 chars of the uuid.
    let value = serde_json::json!({"0": "abcd1234-5678-90ab-cdef-1234567890ab"});
    assert_eq!(short_id(&value), "abcd1234");
}

#[test]
fn short_id_truncates_to_eight_chars() {
    // Any inner uuid string must collapse to exactly 8 characters.
    let value = serde_json::json!({"0": "0123456789abcdef-rest-ignored"});
    assert_eq!(short_id(&value).chars().count(), 8);
}

#[test]
fn short_id_falls_back_when_field_missing() {
    // Missing `0` key or a scalar value → the placeholder.
    assert_eq!(short_id(&serde_json::json!({})), "--------");
    assert_eq!(short_id(&serde_json::json!("scalar")), "--------");
}

#[test]
fn short_id_falls_back_when_value_not_str() {
    // `0` present but not a string → the placeholder.
    assert_eq!(short_id(&serde_json::json!({"0": 42})), "--------");
}

#[test]
fn cli_parses_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "status"]).unwrap();
    assert!(matches!(cli.command.unwrap(), Command::Status));
}

#[test]
fn cli_parses_mcp_add() {
    // stdio (default transport): `tm mcp add echo -- echo hi`
    let cli =
        Cli::try_parse_from(["trusty-mpm", "mcp", "add", "echo", "--", "echo", "hi"]).unwrap();
    let Some(Command::Mcp {
        cmd:
            McpCmd::Add {
                name,
                transport,
                env,
                header,
                command_and_args,
                root,
            },
    }) = cli.command
    else {
        panic!("expected mcp add");
    };
    assert_eq!(name, "echo");
    assert_eq!(transport, McpTransportArg::Stdio);
    assert!(env.is_empty() && header.is_empty() && root.is_none());
    assert_eq!(command_and_args, vec!["echo", "hi"]);
}

#[test]
fn cli_parses_mcp_add_env_and_root() {
    // `tm mcp add srv -e A=1 -e B=2 --root /x -- npx pkg`
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "mcp",
        "add",
        "srv",
        "-e",
        "A=1",
        "-e",
        "B=2",
        "--root",
        "/x",
        "--",
        "npx",
        "pkg",
    ])
    .unwrap();
    let Some(Command::Mcp {
        cmd:
            McpCmd::Add {
                env,
                root,
                command_and_args,
                ..
            },
    }) = cli.command
    else {
        panic!("expected mcp add");
    };
    assert_eq!(env, vec!["A=1", "B=2"]);
    assert_eq!(root.as_deref(), Some("/x"));
    assert_eq!(command_and_args, vec!["npx", "pkg"]);
}

#[test]
fn cli_parses_mcp_add_http() {
    // `tm mcp add sentry -t http -H "Authorization: Bearer t" https://x/mcp`
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "mcp",
        "add",
        "sentry",
        "-t",
        "http",
        "-H",
        "Authorization: Bearer t",
        "https://x/mcp",
    ])
    .unwrap();
    let Some(Command::Mcp {
        cmd:
            McpCmd::Add {
                transport,
                header,
                command_and_args,
                ..
            },
    }) = cli.command
    else {
        panic!("expected mcp add http");
    };
    assert_eq!(transport, McpTransportArg::Http);
    assert_eq!(header, vec!["Authorization: Bearer t"]);
    assert_eq!(command_and_args, vec!["https://x/mcp"]);
}

#[test]
fn cli_parses_mcp_remove_list_get() {
    let rm = Cli::try_parse_from(["trusty-mpm", "mcp", "remove", "echo"]).unwrap();
    assert!(matches!(
        rm.command,
        Some(Command::Mcp {
            cmd: McpCmd::Remove { .. }
        })
    ));
    let ls = Cli::try_parse_from(["trusty-mpm", "mcp", "list", "--json"]).unwrap();
    let Some(Command::Mcp {
        cmd: McpCmd::List { json, .. },
    }) = ls.command
    else {
        panic!("expected mcp list");
    };
    assert!(json);
    let get = Cli::try_parse_from(["trusty-mpm", "mcp", "get", "echo"]).unwrap();
    assert!(matches!(
        get.command,
        Some(Command::Mcp {
            cmd: McpCmd::Get { .. }
        })
    ));
}

#[test]
fn cli_parses_mcp_test() {
    // Bare sweep: no name, no flags.
    let bare = Cli::try_parse_from(["trusty-mpm", "mcp", "test"]).unwrap();
    let Some(Command::Mcp {
        cmd: McpCmd::Test { name, json, root },
    }) = bare.command
    else {
        panic!("expected mcp test");
    };
    assert!(name.is_none() && !json && root.is_none());

    // Named + --json + --root.
    let named = Cli::try_parse_from([
        "trusty-mpm",
        "mcp",
        "test",
        "trusty-memory",
        "--json",
        "--root",
        "/x",
    ])
    .unwrap();
    let Some(Command::Mcp {
        cmd: McpCmd::Test { name, json, root },
    }) = named.command
    else {
        panic!("expected mcp test named");
    };
    assert_eq!(name.as_deref(), Some("trusty-memory"));
    assert!(json);
    assert_eq!(root.as_deref(), Some("/x"));
}

#[test]
fn cli_parses_start() {
    let cli = Cli::try_parse_from(["trusty-mpm", "start"]).unwrap();
    assert!(matches!(cli.command.unwrap(), Command::Start));
}

#[test]
fn cli_parses_serve() {
    // Bare `serve` defaults to the non-stdio (start-like) behaviour.
    let cli = Cli::try_parse_from(["trusty-mpm", "serve"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Serve { stdio: false }
    ));
}

#[test]
fn cli_parses_serve_stdio() {
    // `serve --stdio` selects the MCP stdio bridge (#1221).
    let cli = Cli::try_parse_from(["trusty-mpm", "serve", "--stdio"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Serve { stdio: true }
    ));
}

#[test]
fn cli_parses_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "stop"]).unwrap();
    assert!(matches!(cli.command.unwrap(), Command::Stop));
}

#[test]
fn cli_parses_doctor() {
    let cli = Cli::try_parse_from(["trusty-mpm", "doctor"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Doctor {
            prune_stale_skills: false
        }
    ));
}

#[test]
fn cli_parses_doctor_prune_stale_skills() {
    let cli = Cli::try_parse_from(["trusty-mpm", "doctor", "--prune-stale-skills"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Doctor {
            prune_stale_skills: true
        }
    ));
}

#[test]
fn cli_parses_validate() {
    let cli = Cli::try_parse_from(["trusty-mpm", "validate"]).unwrap();
    match cli.command.unwrap() {
        Command::Validate { path, repair } => {
            assert_eq!(path, None);
            assert!(!repair);
        }
        other => panic!("expected Command::Validate, got {other:?}"),
    }
}

#[test]
fn cli_parses_validate_with_path_and_repair() {
    let cli =
        Cli::try_parse_from(["trusty-mpm", "validate", "--path", "/tmp/ws", "--repair"]).unwrap();
    match cli.command.unwrap() {
        Command::Validate { path, repair } => {
            assert_eq!(path, Some(std::path::PathBuf::from("/tmp/ws")));
            assert!(repair);
        }
        other => panic!("expected Command::Validate, got {other:?}"),
    }
}

#[test]
fn cli_parses_health() {
    let cli = Cli::try_parse_from(["trusty-mpm", "health"]).unwrap();
    assert!(matches!(cli.command.unwrap(), Command::Health));
}

#[test]
fn cli_parses_project_init() {
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "init"]).unwrap();
    match cli.command.unwrap() {
        Command::Project {
            action: ProjectAction::Init { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected project init, got {other:?}"),
    }
}

#[test]
fn cli_parses_project_init_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "init", "--dir", "/work/p"]).unwrap();
    match cli.command.unwrap() {
        Command::Project {
            action: ProjectAction::Init { dir },
        } => assert_eq!(dir.as_deref(), Some("/work/p")),
        other => panic!("expected project init, got {other:?}"),
    }
}

#[test]
fn cli_parses_project_list() {
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "list"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Project {
            action: ProjectAction::List
        }
    ));
}

#[test]
fn cli_parses_project_info() {
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "info"]).unwrap();
    match cli.command.unwrap() {
        Command::Project {
            action: ProjectAction::Info { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected project info, got {other:?}"),
    }
}

#[test]
fn cli_project_requires_action() {
    // `project` with no action is an error.
    assert!(Cli::try_parse_from(["trusty-mpm", "project"]).is_err());
}

#[test]
fn cli_parses_meta_run() {
    // Bare `meta run` defaults to no demo, no explicit project, no-provision off,
    // and no explicit timeout (#1049/#1051).
    let cli = Cli::try_parse_from(["trusty-mpm", "meta", "run"]).unwrap();
    match cli.command.unwrap() {
        Command::Meta {
            action:
                MetaAction::Run {
                    demo,
                    project,
                    no_provision,
                    timeout_secs,
                },
        } => {
            assert!(!demo);
            assert_eq!(project, None);
            assert!(!no_provision);
            assert_eq!(timeout_secs, None);
        }
        other => panic!("expected meta run, got {other:?}"),
    }
}

#[test]
fn cli_parses_meta_run_demo() {
    // `meta run --demo` sets the demo flag.
    let cli = Cli::try_parse_from(["trusty-mpm", "meta", "run", "--demo"]).unwrap();
    match cli.command.unwrap() {
        Command::Meta {
            action: MetaAction::Run { demo, project, .. },
        } => {
            assert!(demo);
            assert_eq!(project, None);
        }
        other => panic!("expected meta run --demo, got {other:?}"),
    }
}

#[test]
fn cli_parses_meta_run_project() {
    // `meta run --demo --project <PATH>` captures both the flag and the path.
    let cli =
        Cli::try_parse_from(["trusty-mpm", "meta", "run", "--demo", "--project", "/tmp"]).unwrap();
    match cli.command.unwrap() {
        Command::Meta {
            action: MetaAction::Run { demo, project, .. },
        } => {
            assert!(demo);
            assert_eq!(project.as_deref(), Some(std::path::Path::new("/tmp")));
        }
        other => panic!("expected meta run --project, got {other:?}"),
    }
}

#[test]
fn cli_parses_meta_run_no_provision() {
    // `meta run --no-provision` sets the clone-skip flag (#1049).
    let cli = Cli::try_parse_from(["trusty-mpm", "meta", "run", "--no-provision"]).unwrap();
    match cli.command.unwrap() {
        Command::Meta {
            action: MetaAction::Run { no_provision, .. },
        } => assert!(no_provision),
        other => panic!("expected meta run --no-provision, got {other:?}"),
    }
}

#[test]
fn cli_parses_meta_run_timeout() {
    // `meta run --timeout-secs 30` overrides the poll budget (#1051).
    let cli = Cli::try_parse_from(["trusty-mpm", "meta", "run", "--timeout-secs", "30"]).unwrap();
    match cli.command.unwrap() {
        Command::Meta {
            action: MetaAction::Run { timeout_secs, .. },
        } => assert_eq!(timeout_secs, Some(30)),
        other => panic!("expected meta run --timeout-secs, got {other:?}"),
    }
}

#[test]
fn cli_meta_requires_action() {
    // `meta` with no action is an error.
    assert!(Cli::try_parse_from(["trusty-mpm", "meta"]).is_err());
}

#[test]
fn cli_parses_launch() {
    let cli = Cli::try_parse_from(["trusty-mpm", "launch"]).unwrap();
    match cli.command.unwrap() {
        Command::Launch { dir, style } => {
            assert_eq!(dir, None);
            assert_eq!(style, None);
        }
        other => panic!("expected launch, got {other:?}"),
    }
}

#[test]
fn cli_parses_launch_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "launch", "/work/p"]).unwrap();
    match cli.command.unwrap() {
        Command::Launch { dir, style } => {
            assert_eq!(dir.as_deref(), Some("/work/p"));
            assert_eq!(style, None);
        }
        other => panic!("expected launch, got {other:?}"),
    }
}

#[test]
fn cli_parses_launch_with_style() {
    // HR-4: `--style <id>` selects the active output style for this launch.
    let cli =
        Cli::try_parse_from(["trusty-mpm", "launch", "--style", "trusty-mpm-teacher"]).unwrap();
    match cli.command.unwrap() {
        Command::Launch { dir, style } => {
            assert_eq!(dir, None);
            assert_eq!(style.as_deref(), Some("trusty-mpm-teacher"));
        }
        other => panic!("expected launch, got {other:?}"),
    }
}

#[test]
fn fallback_session_name_has_tm_prefix() {
    let name = fallback_session_name(std::path::Path::new("/work/p"));
    assert!(name.starts_with("tm-"), "got {name}");
}

#[test]
fn fallback_session_name_uses_folder() {
    // The offline fallback name reflects the project folder, not a UUID.
    let name = fallback_session_name(std::path::Path::new("/Users/x/trusty-mpm"));
    assert_eq!(name, "tm-trusty-mpm");
}

#[test]
fn launch_banner_does_not_panic() {
    // Why: the banner does width math and probes PATH; exercise it to catch
    // arithmetic underflow or formatting regressions.
    print_launch_banner("/Users/test/project", "tmpm-quiet-falcon", None, None);
    print_launch_banner(
        "/Users/test/project",
        "tmpm-quiet-falcon",
        Some(std::path::Path::new("/tmp/system-prompt.txt")),
        None,
    );
}

#[test]
fn launch_reconnect_banner_does_not_panic() {
    // Why: the reconnect banner shares the render path with the launch
    // banner; exercise it to catch formatting regressions.
    print_launch_banner_reconnecting("/Users/test/project", "tmpm-quiet-falcon");
}

#[test]
fn terminal_width_is_positive() {
    // Why: callers use the width for layout; a zero or absurd value would
    // break formatting. The probe must always yield a usable column count.
    assert!(terminal_width() > 0);
}

#[test]
fn banner_title_bar_contains_version() {
    // Why: the single-box banner embeds the crate version in the title bar;
    // this ensures the version is always visible to the operator at a glance.
    use crate::formatters::banner::two_panel::{render_two_panel_banner, strip_ansi};
    use crate::formatters::info_box::{DaemonInfo, WelcomeData};
    colored::control::set_override(false);
    let data = WelcomeData {
        project: "my-project".to_string(),
        workspace: "/Users/test/project".to_string(),
        user: "operator".to_string(),
        reconnecting: false,
        session_name: "tmpm-my-project".to_string(),
        daemon: DaemonInfo::default(),
        recent_commits: vec![],
        memory_status: "(not detected)".to_string(),
        search_status: "(not detected)".to_string(),
        review_status: "(not detected)".to_string(),
    };
    let out = render_two_panel_banner(&data, 120, false).expect("wide banner");
    let first_line = out.lines().next().unwrap_or("");
    let bare = strip_ansi(first_line);
    assert!(
        bare.contains(env!("CARGO_PKG_VERSION")),
        "title bar must contain CARGO_PKG_VERSION: {bare:?}"
    );
    assert!(
        bare.starts_with('╭'),
        "title bar must start with outer box corner ╭: {bare:?}"
    );
    colored::control::unset_override();
}

#[test]
fn banner_reconnect_label_in_wide_mode() {
    // Why: the reconnect label in the wide single-box banner must say
    // "Reconnecting..." so the operator knows no new session was started.
    use crate::formatters::banner::two_panel::{render_two_panel_banner, strip_ansi};
    use crate::formatters::info_box::{DaemonInfo, WelcomeData};
    colored::control::set_override(false);
    let data = WelcomeData {
        project: "my-project".to_string(),
        workspace: "/Users/test/project".to_string(),
        user: "operator".to_string(),
        reconnecting: true,
        session_name: "tmpm-quiet-falcon".to_string(),
        daemon: DaemonInfo::default(),
        recent_commits: vec![],
        memory_status: "(not detected)".to_string(),
        search_status: "(not detected)".to_string(),
        review_status: "(not detected)".to_string(),
    };
    let out = render_two_panel_banner(&data, 120, true).expect("wide reconnect banner");
    let bare = strip_ansi(&out);
    assert!(
        bare.contains("Reconnecting..."),
        "wide reconnect banner must contain 'Reconnecting...': {bare:.200}"
    );
    colored::control::unset_override();
}

#[test]
fn normalize_workdir_strips_trailing_slash() {
    // Why: a daemon-stored workdir and the resolved cwd may differ only by a
    // trailing slash; reconnect detection must treat them as equal. Use a
    // path that does not exist so canonicalize falls through to the
    // string-normalization branch deterministically.
    let with = normalize_workdir("/no/such/dir/");
    let without = normalize_workdir("/no/such/dir");
    assert_eq!(with, without);
    assert_eq!(without, "/no/such/dir");
}

#[test]
fn cli_parses_session_start() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "start"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Start { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected session start, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_start_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "start", "--dir", "/work/p"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Start { dir },
        } => assert_eq!(dir.as_deref(), Some("/work/p")),
        other => panic!("expected session start, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "stop", "tmpm-quiet-falcon"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Stop { id_or_name },
        } => assert_eq!(id_or_name, "tmpm-quiet-falcon"),
        other => panic!("expected session stop, got {other:?}"),
    }
}

#[test]
fn cli_session_stop_requires_arg() {
    // `session stop` without an id-or-name is an error.
    assert!(Cli::try_parse_from(["trusty-mpm", "session", "stop"]).is_err());
}

#[test]
fn cli_parses_session_kill() {
    // `kill` is a visible alias (#2191) for `stop` — it parses to the same
    // `SessionAction::Stop` variant so it dispatches through the identical
    // managed-aware handler with zero other changes.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "kill", "tmpm-quiet-falcon"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Stop { id_or_name },
        } => assert_eq!(id_or_name, "tmpm-quiet-falcon"),
        other => panic!("expected session kill (stop), got {other:?}"),
    }
}

#[test]
fn cli_parses_session_list() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "list"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::List { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected session list, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_clean() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "clean"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Clean { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected session clean, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_info() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "info", "abc-123"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Info { id_or_name },
        } => assert_eq!(id_or_name, "abc-123"),
        other => panic!("expected session info, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_instructions() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "instructions"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Instructions { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected session instructions, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_instructions_with_dir() {
    let cli =
        Cli::try_parse_from(["trusty-mpm", "session", "instructions", "--dir", "/tmp"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Instructions { dir },
        } => assert_eq!(dir.as_deref(), Some("/tmp")),
        other => panic!("expected session instructions, got {other:?}"),
    }
}

#[test]
fn cli_parses_events() {
    let cli = Cli::try_parse_from(["trusty-mpm", "events"]).unwrap();
    assert!(matches!(cli.command.unwrap(), Command::Events));
}

#[test]
fn cli_url_flag_overrides_default() {
    let cli = Cli::try_parse_from(["trusty-mpm", "--url", "http://x:9", "status"]).unwrap();
    assert_eq!(cli.url.as_deref(), Some("http://x:9"));
}

#[test]
fn cli_url_flag_equal_to_default_is_still_some() {
    // Regression test for issue #2487: `cli.url` must distinguish "the operator
    // explicitly passed --url <DEFAULT_URL>" from "nothing was passed at all".
    // Before the fix, `cli.url` was a plain `String` with
    // `default_value = DEFAULT_URL`, so both cases produced the identical
    // string and every downstream resolver had to guess — incorrectly, in
    // this exact case — which one actually happened.
    let cli = Cli::try_parse_from(["trusty-mpm", "--url", DEFAULT_URL, "status"]).unwrap();
    assert_eq!(
        cli.url.as_deref(),
        Some(DEFAULT_URL),
        "an explicit --url equal to the default must still parse as Some, not be lost"
    );
}

#[test]
fn cli_bare_invocation_uses_guided_default() {
    // Since #1708, a bare `tm` invocation is valid: clap parses it with
    // command = None and the guided default fires at runtime.
    let cli = Cli::try_parse_from(["trusty-mpm"]).expect("bare tm must parse");
    assert!(
        cli.command.is_none(),
        "bare tm should produce command = None"
    );
}

#[test]
fn cli_parses_tui_defaults() {
    // The `--url` flag is passed explicitly: the `tui` subcommand's `url`
    // arg carries `env = "TRUSTY_MPM_URL"`, so an ambient `TRUSTY_MPM_URL`
    // in the test runner's environment would otherwise override the clap
    // `default_value` and make this assertion environment-dependent.
    // `interval_ms` has no env binding, so its default is asserted bare.
    let cli = Cli::try_parse_from(["trusty-mpm", "tui", "--url", DEFAULT_URL]).unwrap();
    match cli.command.unwrap() {
        Command::Tui { url, interval_ms } => {
            assert_eq!(url.as_deref(), Some(DEFAULT_URL));
            assert_eq!(interval_ms, 1000);
        }
        other => panic!("expected Tui, got {other:?}"),
    }
}

#[test]
fn cli_parses_tui_with_interval() {
    let cli = Cli::try_parse_from(["trusty-mpm", "tui", "--interval-ms", "500"]).unwrap();
    match cli.command.unwrap() {
        Command::Tui { interval_ms, .. } => assert_eq!(interval_ms, 500),
        other => panic!("expected Tui, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_tui() {
    // #1392: the coordinator TUI is the `tm session tui` subcommand (renamed
    // from the former top-level `tm coordinator-tui`). Its `--interval-ms`
    // defaults to 1500. The `url` arg carries `env = "TRUSTY_MPM_URL"`, so an
    // ambient `TRUSTY_MPM_URL` in the test runner would override the (absent)
    // default — assert only the env-independent interval default here, and the
    // url plumbing in `cli_parses_session_tui_with_flags`.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "tui"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Tui { interval_ms, .. },
        } => {
            assert_eq!(interval_ms, 1500);
        }
        other => panic!("expected Session::Tui, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_tui_with_flags() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "tui",
        "--url",
        "http://127.0.0.1:9999",
        "--interval-ms",
        "750",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::Tui { url, interval_ms },
        } => {
            assert_eq!(url.as_deref(), Some("http://127.0.0.1:9999"));
            assert_eq!(interval_ms, 750);
        }
        other => panic!("expected Session::Tui, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_singular() {
    // #1916 made `session` (singular) canonical with no plural alias. #2116
    // (DOC-35 §2.2/§3.2) reverses that: `sessions` (plural) is now canonical,
    // sibling to `tm projects`, and `session` is kept as a hidden deprecated
    // alias rather than retired — so this exact invocation must keep parsing
    // identically to before, into the same `Command::Session` variant.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "list"]).unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action: SessionAction::List { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected session list, got {other:?}"),
    }
}

// #2116 note: the mirror-plural parse test, the full-verb-surface parity
// sweep, and the alias-notice message-text assertion moved to
// `tests_behavior_e_tests.rs` to keep this file under the 1500-SLOC test cap
// (`cli_parses_sessions_plural_canonical`, `cli_session_and_sessions_agree_for_every_verb`,
// `top_level_alias_notice_message`).

#[test]
fn cli_rejects_removed_coordinator_tui() {
    // #1392: the old top-level `tm coordinator-tui` was deleted (not aliased).
    // Parsing it must now fail with an unrecognized-subcommand error.
    let err = Cli::try_parse_from(["trusty-mpm", "coordinator-tui"]).unwrap_err();
    assert_eq!(
        err.kind(),
        clap::error::ErrorKind::InvalidSubcommand,
        "expected InvalidSubcommand, got {:?}",
        err.kind()
    );
}

#[test]
fn cli_parses_telegram_pair() {
    let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "pair"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Telegram {
            cmd: TelegramCmd::Pair
        }
    ));
}

#[test]
fn cli_parses_telegram_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "status"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Telegram {
            cmd: TelegramCmd::Status
        }
    ));
}

#[test]
fn cli_parses_telegram_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "stop"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Telegram {
            cmd: TelegramCmd::Stop
        }
    ));
}

#[test]
fn cli_telegram_requires_subcommand() {
    // `telegram` with no action is an error now that it is a group.
    assert!(Cli::try_parse_from(["trusty-mpm", "telegram"]).is_err());
}

#[test]
fn cli_parses_telegram_start_with_check() {
    let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "start", "--check"]).unwrap();
    match cli.command.unwrap() {
        Command::Telegram {
            cmd: TelegramCmd::Start { check, token, .. },
        } => {
            assert!(check);
            assert_eq!(token, None);
        }
        other => panic!("expected Telegram start, got {other:?}"),
    }
}

#[test]
fn cli_parses_telegram_start_with_token() {
    let cli =
        Cli::try_parse_from(["trusty-mpm", "telegram", "start", "--token", "secret"]).unwrap();
    match cli.command.unwrap() {
        Command::Telegram {
            cmd: TelegramCmd::Start { token, check, .. },
        } => {
            assert_eq!(token.as_deref(), Some("secret"));
            assert!(!check);
        }
        other => panic!("expected Telegram start, got {other:?}"),
    }
}

#[test]
fn cli_parses_telegram_start_alert_and_user_flags() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "telegram",
        "start",
        "--alert-chat-id",
        "12345",
        "--allowed-user-id",
        "67890",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Telegram {
            cmd:
                TelegramCmd::Start {
                    alert_chat_id,
                    allowed_user_id,
                    ..
                },
        } => {
            assert_eq!(alert_chat_id, Some(12345));
            assert_eq!(allowed_user_id, Some(67890));
        }
        other => panic!("expected Telegram start, got {other:?}"),
    }
}

#[test]
fn cli_parses_slack_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "slack", "stop"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Slack {
            cmd: SlackCmd::Stop
        }
    ));
}

#[test]
fn cli_slack_requires_subcommand() {
    // `slack` with no action is an error — it is a command group.
    assert!(Cli::try_parse_from(["trusty-mpm", "slack"]).is_err());
}

#[test]
fn cli_parses_slack_start_with_check() {
    let cli = Cli::try_parse_from(["trusty-mpm", "slack", "start", "--check"]).unwrap();
    match cli.command.unwrap() {
        Command::Slack {
            cmd:
                SlackCmd::Start {
                    check,
                    bot_token,
                    app_token,
                    ..
                },
        } => {
            assert!(check);
            assert_eq!(bot_token, None);
            assert_eq!(app_token, None);
        }
        other => panic!("expected Slack start, got {other:?}"),
    }
}

#[test]
fn cli_parses_slack_start_with_tokens() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "slack",
        "start",
        "--bot-token",
        "xoxb-secret",
        "--app-token",
        "xapp-secret",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Slack {
            cmd:
                SlackCmd::Start {
                    bot_token,
                    app_token,
                    check,
                    ..
                },
        } => {
            assert_eq!(bot_token.as_deref(), Some("xoxb-secret"));
            assert_eq!(app_token.as_deref(), Some("xapp-secret"));
            assert!(!check);
        }
        other => panic!("expected Slack start, got {other:?}"),
    }
}

#[test]
fn cli_parses_daemon_defaults() {
    let cli = Cli::try_parse_from(["trusty-mpm", "daemon"]).unwrap();
    match cli.command.unwrap() {
        Command::Daemon {
            addr,
            tailscale,
            mcp,
        } => {
            assert_eq!(addr.to_string(), "127.0.0.1:7880");
            assert!(!tailscale);
            assert!(!mcp);
        }
        other => panic!("expected Daemon, got {other:?}"),
    }
}

#[test]
fn cli_parses_daemon_tailscale() {
    let cli = Cli::try_parse_from(["trusty-mpm", "daemon", "--tailscale"]).unwrap();
    match cli.command.unwrap() {
        Command::Daemon { tailscale, .. } => assert!(tailscale),
        other => panic!("expected Daemon, got {other:?}"),
    }
}

#[test]
fn cli_parses_supervisor() {
    let cli = Cli::try_parse_from(["trusty-mpm", "supervisor"]).unwrap();
    match cli.command.unwrap() {
        Command::Supervisor {
            interval,
            auto_resume,
            no_classify,
            ..
        } => {
            assert_eq!(interval, None);
            assert!(!auto_resume);
            assert!(!no_classify);
        }
        other => panic!("expected Supervisor, got {other:?}"),
    }
}

#[test]
fn cli_parses_supervisor_flags() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "supervisor",
        "--interval",
        "60",
        "--auto-resume",
        "--no-classify",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Supervisor {
            interval,
            auto_resume,
            no_classify,
            ..
        } => {
            assert_eq!(interval, Some(60));
            assert!(auto_resume);
            assert!(no_classify);
        }
        other => panic!("expected Supervisor, got {other:?}"),
    }
}

#[test]
fn deprecation_notice_format() {
    // The deprecation helper renders a stable, single-line message (#1205).
    // We assert the pure message builder since the eprintln side-effect itself
    // is untestable without capturing process stderr.
    use crate::commands::managed::deprecation_message;
    assert_eq!(
        deprecation_message("runtime-stop", "stop"),
        "warning: 'runtime-stop' is deprecated; use 'stop'"
    );
    assert_eq!(
        deprecation_message("managed-resume", "resume"),
        "warning: 'managed-resume' is deprecated; use 'resume'"
    );
}

// The bespoke `classify_managed_target` resolver was retired in Phase 1B
// (refs #1283); the managed-vs-project routing decision for `stop`/`resume` now
// goes through the ONE canonical `client::resolve_target`. These tests assert the
// SAME routing semantics they did before, but against that shared resolver — the
// `.map(|s| s.id.clone())` mirroring what `managed_route::resolve_managed_match`
// returns to the dispatcher.

#[test]
fn resolve_managed_target_matches_by_id() {
    // #1218: the canonical `stop`/`resume` verbs must recognize a managed
    // session by its UUID and route to the managed endpoint, not project-sessions.
    use trusty_mpm::client::{ManagedSessionSummary, resolve_target};
    let uuid = "11111111-2222-3333-4444-555555555555";
    let sessions: Vec<ManagedSessionSummary> = serde_json::from_value(serde_json::json!([
        { "id": uuid, "name": "tmpm-quiet-falcon", "state": "running" }
    ]))
    .unwrap();
    assert_eq!(
        resolve_target(&sessions, uuid).map(|s| s.id.clone()),
        Some(uuid.to_string()),
        "a managed UUID must resolve to the managed id (routes to managed endpoint)"
    );
}

#[test]
fn resolve_managed_target_matches_by_name_returns_id() {
    // A friendly managed name must resolve to the canonical UUID the managed
    // endpoints require, so `tm session stop <name>` works for managed sessions.
    use trusty_mpm::client::{ManagedSessionSummary, resolve_target};
    let uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let sessions: Vec<ManagedSessionSummary> = serde_json::from_value(serde_json::json!([
        { "id": uuid, "name": "tmpm-brave-otter", "state": "stopped" }
    ]))
    .unwrap();
    assert_eq!(
        resolve_target(&sessions, "tmpm-brave-otter").map(|s| s.id.clone()),
        Some(uuid.to_string()),
        "a managed name must resolve to its id, not be passed through verbatim"
    );
}

#[test]
fn resolve_managed_target_unknown_falls_back_to_none() {
    // A non-managed id/name must NOT match — the caller then falls back to the
    // project-session path, preserving the local/project-session family (#1218).
    use trusty_mpm::client::{ManagedSessionSummary, resolve_target};
    let sessions: Vec<ManagedSessionSummary> = serde_json::from_value(serde_json::json!([
        { "id": "11111111-2222-3333-4444-555555555555", "name": "tmpm-quiet-falcon", "state": "running" }
    ]))
    .unwrap();
    assert_eq!(
        resolve_target(&sessions, "some-local-session-name").map(|s| s.id.clone()),
        None,
        "an unknown id/name must yield None so `stop`/`resume` fall back to project-sessions"
    );
}

#[test]
fn resolve_managed_target_empty_list_is_none() {
    // With no managed sessions, every argument falls back to project-sessions.
    use trusty_mpm::client::{ManagedSessionSummary, resolve_target};
    let sessions: Vec<ManagedSessionSummary> = Vec::new();
    assert_eq!(
        resolve_target(&sessions, "anything").map(|s| s.id.clone()),
        None
    );
}

#[test]
fn cli_parses_catalog_sync() {
    let cli = Cli::try_parse_from(["trusty-mpm", "catalog", "sync", "--force"]).unwrap();
    match cli.command.unwrap() {
        Command::Catalog {
            action: CatalogAction::Sync { force },
        } => assert!(force),
        other => panic!("expected catalog sync, got {other:?}"),
    }
}

#[test]
fn cli_parses_catalog_ls() {
    let cli = Cli::try_parse_from(["trusty-mpm", "catalog", "ls"]).unwrap();
    match cli.command.unwrap() {
        Command::Catalog {
            action: CatalogAction::Ls { json },
        } => assert!(!json),
        other => panic!("expected catalog ls, got {other:?}"),
    }
}

#[test]
fn cli_parses_catalog_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "catalog", "status", "--json"]).unwrap();
    match cli.command.unwrap() {
        Command::Catalog {
            action: CatalogAction::Status { json },
        } => assert!(json),
        other => panic!("expected catalog status, got {other:?}"),
    }
}

#[test]
fn cli_parses_catalog_apply() {
    let cli =
        Cli::try_parse_from(["trusty-mpm", "catalog", "apply", "--force", "--prune"]).unwrap();
    match cli.command.unwrap() {
        Command::Catalog {
            action: CatalogAction::Apply { force, prune },
        } => {
            assert!(force);
            assert!(prune);
        }
        other => panic!("expected catalog apply, got {other:?}"),
    }
}

#[test]
fn cli_parses_catalog_apply_defaults() {
    // Without flags, both `force` and `prune` default to false (no surprise
    // network refetch, no surprise removals).
    let cli = Cli::try_parse_from(["trusty-mpm", "catalog", "apply"]).unwrap();
    match cli.command.unwrap() {
        Command::Catalog {
            action: CatalogAction::Apply { force, prune },
        } => {
            assert!(!force);
            assert!(!prune);
        }
        other => panic!("expected catalog apply, got {other:?}"),
    }
}

#[test]
fn cli_parses_ticket() {
    // Why: the minimal form `tm ticket <issue#>` must parse with the `gh`
    // backend as the default and an empty notes list (#1237).
    use crate::commands::ticket::system::TicketSystemKind;
    let cli = Cli::try_parse_from(["trusty-mpm", "ticket", "1232"]).unwrap();
    match cli.command.unwrap() {
        Command::Ticket {
            issue,
            system,
            notes,
            runtime,
        } => {
            assert_eq!(issue, "1232");
            assert_eq!(system, TicketSystemKind::Gh);
            assert!(notes.is_empty());
            assert_eq!(runtime, trusty_mpm::runtime::RuntimeKind::ClaudeCode);
        }
        other => panic!("expected ticket, got {other:?}"),
    }
}

#[test]
fn cli_parses_ticket_with_hash_and_system() {
    // Why: the leading `#` is allowed and the `[system]` positional selects the
    // backend (#1237).
    use crate::commands::ticket::system::TicketSystemKind;
    let cli = Cli::try_parse_from(["trusty-mpm", "ticket", "#1232", "jira"]).unwrap();
    match cli.command.unwrap() {
        Command::Ticket { issue, system, .. } => {
            assert_eq!(issue, "#1232");
            assert_eq!(system, TicketSystemKind::Jira);
        }
        other => panic!("expected ticket, got {other:?}"),
    }
}

#[test]
fn cli_parses_ticket_with_notes() {
    // Why: `--note/-m` is repeatable and each value is posted as an issue comment
    // for the audit trail (#1237).
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "ticket",
        "42",
        "-m",
        "starting work",
        "--note",
        "second note",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Ticket { issue, notes, .. } => {
            assert_eq!(issue, "42");
            assert_eq!(notes, vec!["starting work", "second note"]);
        }
        other => panic!("expected ticket, got {other:?}"),
    }
}

#[test]
fn cli_rejects_ticket_unknown_system() {
    // Why: an unknown `[system]` value must be rejected at parse time with a
    // "possible values" hint, not silently forwarded (#1237).
    let err = Cli::try_parse_from(["trusty-mpm", "ticket", "1", "bogus"]);
    assert!(err.is_err(), "expected unknown system to be rejected");
}

#[test]
fn cli_parses_issue_seed_labels() {
    // Why: `tm issue seed-labels [--dry-run]` must parse with the gh default
    // and surface the dry-run flag (#1246).
    use crate::cli::IssueCmd;
    let cli = Cli::try_parse_from(["trusty-mpm", "issue", "seed-labels", "--dry-run"]).unwrap();
    match cli.command.unwrap() {
        Command::Issue { cmd, .. } => match cmd {
            IssueCmd::SeedLabels { dry_run, config } => {
                assert!(dry_run);
                assert!(config.is_none());
            }
            other => panic!("expected seed-labels, got {other:?}"),
        },
        other => panic!("expected issue, got {other:?}"),
    }
}

#[test]
fn cli_parses_issue_transition() {
    // Why: `tm issue transition <issue#> <to-state> [--note]` must parse the
    // numeric issue, the target state, and the optional note (#1246).
    use crate::cli::IssueCmd;
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "issue",
        "transition",
        "1232",
        "approved",
        "--note",
        "approved by reviewer",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Issue { cmd, .. } => match cmd {
            IssueCmd::Transition {
                issue,
                to_state,
                note,
                ..
            } => {
                assert_eq!(issue, 1232);
                assert_eq!(to_state, "approved");
                assert_eq!(note.as_deref(), Some("approved by reviewer"));
            }
            other => panic!("expected transition, got {other:?}"),
        },
        other => panic!("expected issue, got {other:?}"),
    }
}

#[test]
fn cli_parses_issue_current_and_states_and_repair() {
    // Why: the read/introspection/recovery verbs must all parse (#1246).
    use crate::cli::IssueCmd;
    let current = Cli::try_parse_from(["trusty-mpm", "issue", "current", "7"]).unwrap();
    assert!(matches!(
        current.command.unwrap(),
        Command::Issue {
            cmd: IssueCmd::Current { issue: 7, .. },
            ..
        }
    ));
    let states = Cli::try_parse_from(["trusty-mpm", "issue", "states"]).unwrap();
    assert!(matches!(
        states.command.unwrap(),
        Command::Issue {
            cmd: IssueCmd::States { .. },
            ..
        }
    ));
    let repair = Cli::try_parse_from(["trusty-mpm", "issue", "repair", "9"]).unwrap();
    assert!(matches!(
        repair.command.unwrap(),
        Command::Issue {
            cmd: IssueCmd::Repair { issue: 9, .. },
            ..
        }
    ));
}

#[test]
fn gui_not_found_error_has_install_hint() {
    // Why: when `trusty-mpm-gui` is not installed, `tm gui` must fail with an
    // actionable message telling the user how to install it (gui decoupling,
    // publish-prep). A bare "No such file" from the OS is not acceptable.
    // What: feeds a synthetic `NotFound` spawn error into the result mapper and
    // asserts the error string carries the `cargo install trusty-mpm-gui` hint.
    let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
    let err = crate::gui_status_to_result(Err(not_found))
        .expect_err("NotFound spawn error must map to an Err");
    let msg = err.to_string();
    assert!(
        msg.contains("cargo install trusty-mpm-gui"),
        "expected install hint, got: {msg}"
    );
    assert!(
        msg.contains("not installed"),
        "expected 'not installed' phrasing, got: {msg}"
    );
}

#[test]
fn gui_binary_resolution_falls_back_to_bare_name() {
    // Why: when no `trusty-mpm-gui` sibling exists next to the running binary,
    // the resolver must fall back to the bare name so the OS resolves it on PATH
    // (Single-Install convention). The test binary's dir has no such sibling.
    // What: asserts the resolved path's file name is exactly `trusty-mpm-gui`.
    let resolved = crate::resolve_gui_binary();
    assert_eq!(
        resolved.file_name().and_then(|n| n.to_str()),
        Some("trusty-mpm-gui"),
        "resolver must yield a trusty-mpm-gui path, got: {resolved:?}"
    );
}

#[test]
fn cli_parses_issue_seed_config_force() {
    // Why: `tm issue seed-config --force` must parse the overwrite flag (#1246).
    use crate::cli::IssueCmd;
    let cli = Cli::try_parse_from(["trusty-mpm", "issue", "seed-config", "--force"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Issue {
            cmd: IssueCmd::SeedConfig { force: true },
            ..
        }
    ));
}

#[test]
fn cli_parses_watch_poll_minimal() {
    // Why: the minimal `tm watch poll <project>` must parse with safety defaults
    // (dry-run off but execute also off → dry-run behaviour) and claude-code.
    use crate::cli::WatchCmd;
    let cli = Cli::try_parse_from(["trusty-mpm", "watch", "poll", "owner/repo"]).unwrap();
    match cli.command.unwrap() {
        Command::Watch {
            cmd: WatchCmd::Poll { args },
        } => {
            assert_eq!(args.project, "owner/repo");
            assert!(args.label.is_none());
            assert!(!args.dry_run);
            assert!(!args.execute, "execute must default off (safe)");
            assert!(args.interval_secs.is_none());
            assert!(args.state.is_none());
            assert_eq!(args.runtime, trusty_mpm::runtime::RuntimeKind::ClaudeCode);
        }
        other => panic!("expected watch poll, got {other:?}"),
    }
}

#[test]
fn cli_parses_watch_poll_with_flags() {
    // Why: `--label`, `--state`, and `--execute` must all parse on poll.
    use crate::cli::WatchCmd;
    use crate::commands::watch::github::IssueState;
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "watch",
        "poll",
        "acme/widget",
        "--label",
        "ship-it",
        "--state",
        "all",
        "--execute",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Watch {
            cmd: WatchCmd::Poll { args },
        } => {
            assert_eq!(args.project, "acme/widget");
            assert_eq!(args.label.as_deref(), Some("ship-it"));
            assert_eq!(args.state, Some(IssueState::All));
            assert!(args.execute);
        }
        other => panic!("expected watch poll, got {other:?}"),
    }
}

#[test]
fn cli_parses_watch_listen_with_interval() {
    // Why: `tm watch listen <project> --interval-secs N --dry-run` must parse the
    // listen-specific interval and the explicit safe flag.
    use crate::cli::WatchCmd;
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "watch",
        "listen",
        "owner/repo",
        "--interval-secs",
        "30",
        "--dry-run",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Watch {
            cmd: WatchCmd::Listen { args },
        } => {
            assert_eq!(args.project, "owner/repo");
            assert_eq!(args.interval_secs, Some(30));
            assert!(args.dry_run);
        }
        other => panic!("expected watch listen, got {other:?}"),
    }
}

// WI-10: `tm login` must parse to Command::Login.
#[test]
fn cli_parses_login_standalone() {
    // Why: `tm login` is the one-time keychain auth command (WI-10). It must
    // parse cleanly to the Login variant with no required arguments.
    // What: assert that `tm login` maps to Command::Login.
    // Test: direct parse round-trip.
    let cli = Cli::try_parse_from(["trusty-mpm", "login"]).unwrap();
    assert!(matches!(cli.command.unwrap(), Command::Login { .. }));
}

// ── Image-banner color tests ──────────────────────────────────────────────────

// Issue #1858: `shade_image` used to probe the process-global
// `colored::control` override internally, so these tests had to serialize on
// COLOR_LOCK and mutate/restore that global to exercise the color-on and
// color-off paths — and the sibling test in `two_panel.rs` that did the same
// without the lock was the actual flaky test. `shade_image` now takes
// `use_color` as an explicit parameter, so every test below passes it
// directly; no global mutation or lock is needed anywhere in this file.

#[test]
fn image_clip_has_correct_dimensions() {
    // Why: auto-sizing must produce rows that match the art's actual line count
    // and all rows must have the same display width (the art's max width).
    // What: call shade_image on the embedded default and check row count + col
    // consistency.
    // Test: direct call to shade_image with DEFAULT_BANNER_ART.
    use crate::formatters::banner::{shade_image, source::DEFAULT_BANNER_ART};
    let (lines, cols) = shade_image(DEFAULT_BANNER_ART, false);
    let raw_count = DEFAULT_BANNER_ART.lines().count();
    assert_eq!(
        lines.len(),
        raw_count,
        "shade_image row count must match art line count"
    );
    assert!(cols > 0, "auto-sized cols must be > 0");
    assert!(cols >= 20, "compact-splash art is at least 20 columns wide");
}

#[test]
fn art_char_shading_emits_truecolor() {
    // Why: shade_image must emit truecolor escapes when the caller asks for
    // color (#1839 Fix 3), independent of any process-global state.
    // What: call shade_image with `use_color = true` and verify at least one
    // line contains a truecolor ANSI escape (`\x1B[38;2;`).
    // Test: pure function call, no shared state to race.
    use crate::formatters::banner::{shade_image, source::DEFAULT_BANNER_ART};
    let (lines, _) = shade_image(DEFAULT_BANNER_ART, true);
    let any_colored = lines.iter().any(|l| l.contains("\x1B[38;2;"));
    assert!(
        any_colored,
        "shaded art must emit truecolor escapes when color is enabled"
    );
}

#[test]
fn art_shading_no_ansi_when_color_disabled() {
    // Why: shade_image must suppress ANSI escapes when color is disabled so that
    // banner output piped to a file or CI log is clean plain text (#1839 Fix 3).
    // What: call shade_image with `use_color = false` and assert no line
    // contains an ANSI escape byte.
    // Test: pure function call, no shared state to race.
    use crate::formatters::banner::{shade_image, source::DEFAULT_BANNER_ART};
    let (lines, _) = shade_image(DEFAULT_BANNER_ART, false);
    for line in &lines {
        assert!(
            !line.contains('\x1B'),
            "shade_image must not emit ANSI escapes when color is disabled: {line:?}"
        );
    }
}

#[test]
fn art_shading_spaces_are_uncolored() {
    // Why: spaces in the art should be transparent (no color escape), so
    // the terminal background shows through rather than applying a tint.
    // What: verify that space chars in shaded image lines have no escape prefix.
    // Test: check that strip_ansi still finds spaces in the output.
    use crate::formatters::banner::{
        shade_image, source::DEFAULT_BANNER_ART, two_panel::strip_ansi,
    };
    let (lines, _) = shade_image(DEFAULT_BANNER_ART, true);
    let has_space = lines.iter().any(|l| strip_ansi(l).contains(' '));
    assert!(has_space, "shaded art must contain some uncolored spaces");
}

#[test]
fn narrow_fallback_reconnect_box_includes_reconnecting_text() {
    // Why: the single-box narrow fallback (< MIN_WIDTH_TWO_PANEL) must include
    // "Reconnecting..." when reconnecting=true — matching the wide layout's
    // reconnect indicator. This verifies the fix from commit 54690868 is
    // preserved under the new single-box design.
    // What: render via `render_two_panel_banner` with a narrow width and
    // reconnecting=true; assert the label appears.
    // Test: pure string assertion via the public compositor.
    use crate::formatters::banner::two_panel::{render_two_panel_banner, strip_ansi};
    use crate::formatters::info_box::{DaemonInfo, WelcomeData};
    colored::control::set_override(false);
    let data = WelcomeData {
        project: "my-project".to_string(),
        workspace: "/tmp/my-project".to_string(),
        user: String::new(),
        reconnecting: true,
        session_name: "tmpm-my-project".to_string(),
        daemon: DaemonInfo::default(),
        recent_commits: vec![],
        memory_status: "(not detected)".to_string(),
        search_status: "(not detected)".to_string(),
        review_status: "(not detected)".to_string(),
    };
    // 80-col: narrow stacked box.
    let out = render_two_panel_banner(&data, 80, true).expect("narrow box");
    let bare = strip_ansi(&out);
    assert!(
        bare.contains("Reconnecting..."),
        "narrow reconnect box must contain 'Reconnecting...' label"
    );
    // Also verify the outer box is intact.
    let first = bare.lines().next().unwrap_or("");
    assert!(first.starts_with('╭'), "must have outer box top border");
    colored::control::unset_override();
}

#[test]
fn narrow_fallback_normal_box_does_not_include_reconnecting_text() {
    // Why: normal launch (reconnecting=false) narrow box must NOT show
    // "Reconnecting..." — the operator should see the launch context only.
    use crate::formatters::banner::two_panel::{render_two_panel_banner, strip_ansi};
    use crate::formatters::info_box::{DaemonInfo, WelcomeData};
    colored::control::set_override(false);
    let data = WelcomeData {
        project: "my-project".to_string(),
        workspace: "/tmp/my-project".to_string(),
        user: String::new(),
        reconnecting: false,
        session_name: String::new(),
        daemon: DaemonInfo::default(),
        recent_commits: vec![],
        memory_status: "(not detected)".to_string(),
        search_status: "(not detected)".to_string(),
        review_status: "(not detected)".to_string(),
    };
    let out = render_two_panel_banner(&data, 80, false).expect("narrow box");
    let bare = strip_ansi(&out);
    assert!(
        !bare.contains("Reconnecting..."),
        "normal launch narrow box must not contain 'Reconnecting...' label"
    );
    colored::control::unset_override();
}

#[test]
fn cli_parses_banner() {
    // Why: `tm banner` must parse and default `--reconnecting` to false.
    // What: parse "banner" and assert the variant and flag are correct.
    // Test: direct clap parse.
    use crate::cli::{Cli, Command};
    use clap::Parser;
    let cli = Cli::try_parse_from(["tm", "banner"]).expect("banner must parse");
    assert!(
        matches!(
            cli.command,
            Some(Command::Banner {
                reconnecting: false
            })
        ),
        "expected Banner {{ reconnecting: false }}"
    );
}

#[test]
fn cli_parses_banner_reconnecting() {
    // Why: `tm banner --reconnecting` must set the flag to true.
    // What: parse "banner --reconnecting" and assert the reconnecting flag.
    // Test: direct clap parse.
    use crate::cli::{Cli, Command};
    use clap::Parser;
    let cli = Cli::try_parse_from(["tm", "banner", "--reconnecting"])
        .expect("banner --reconnecting must parse");
    assert!(
        matches!(cli.command, Some(Command::Banner { reconnecting: true })),
        "expected Banner {{ reconnecting: true }}"
    );
}

#[test]
fn banner_preview_no_clear_escape() {
    // Why: the `tm banner` preview must NOT begin with the full-screen clear
    // sequence — that would wipe the user's scrollback.
    // What: capture whether the print_banner_preview output starts with the
    // clear escape. Since it writes to stdout, we verify indirectly via the
    // single-box compositor which is used for the preview path.
    use crate::formatters::banner::two_panel::{render_two_panel_banner, strip_ansi};
    use crate::formatters::info_box::{DaemonInfo, WelcomeData};
    colored::control::set_override(false);
    let data = WelcomeData {
        project: "p".to_string(),
        workspace: "/tmp/p".to_string(),
        user: String::new(),
        reconnecting: false,
        session_name: String::new(),
        daemon: DaemonInfo::default(),
        recent_commits: vec![],
        memory_status: "(not detected)".to_string(),
        search_status: "(not detected)".to_string(),
        review_status: "(not detected)".to_string(),
    };
    let out = render_two_panel_banner(&data, 120, false).expect("banner");
    // The compositor never emits a screen-clear — that is added by the print
    // wrappers only, and only for the launch (not preview) path.
    assert!(
        !out.starts_with("\x1B[2J"),
        "compositor output must not start with screen-clear escape"
    );
    // Version appears in the title bar.
    let bare = strip_ansi(&out);
    assert!(
        bare.contains(env!("CARGO_PKG_VERSION")),
        "compositor output must contain version in title bar"
    );
    colored::control::unset_override();
}

#[test]
fn banner_preview_does_not_panic() {
    // Why: `print_banner_preview` probes env/files that may or may not be present;
    // it must never panic in a clean environment.
    // What: call print_banner_preview with both reconnecting states; assert no panic.
    // Test: if the function returns, the assertion is trivially met.
    // Note: output goes to stdout; suppress by redirecting in the test environment.
    crate::formatters::banner::print_banner_preview(false);
    crate::formatters::banner::print_banner_preview(true);
}

#[test]
fn health_catalog_stale_message_contains_action() {
    // Why: operators should see an actionable command when the catalog is stale
    // or never synced — "updates available" without a fix is unhelpful (#1839 Fix 4).
    // What: asserts against the production constants from misc.rs so any change to
    // the actual output string is caught here rather than silently diverging.
    // Test: if `CATALOG_STALE_MSG` or `CATALOG_UNKNOWN_MSG` in misc.rs no longer
    // contains "tm catalog sync", this test fails and flags the regression.
    assert!(
        CATALOG_STALE_MSG.contains("tm catalog sync"),
        "CATALOG_STALE_MSG must name the fix command: {CATALOG_STALE_MSG:?}"
    );
    assert!(
        CATALOG_UNKNOWN_MSG.contains("tm catalog sync"),
        "CATALOG_UNKNOWN_MSG must name the fix command: {CATALOG_UNKNOWN_MSG:?}"
    );
}

#[test]
fn non_git_dir_fallback_prints_help_hint() {
    // Why: `tm` run from a non-git directory should exit cleanly with a help hint
    // rather than an error exit code (#1839 Fix 2).
    // What: asserts against `NON_GIT_FALLBACK_HINT` from misc.rs — the constant
    // that `fallback_protected` in guided.rs actually passes to `eprintln!`. If
    // the hint string is changed to drop "--help", this test catches it immediately.
    // Test: if `NON_GIT_FALLBACK_HINT` no longer mentions "--help", this fails.
    assert!(
        NON_GIT_FALLBACK_HINT.contains("--help"),
        "NON_GIT_FALLBACK_HINT must reference --help for discoverability: \
         {NON_GIT_FALLBACK_HINT:?}"
    );
}
