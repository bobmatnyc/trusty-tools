//! CLI parse tests and formatter unit tests for `tm` / `trusty-mpm`.
//!
//! Why: tests live in dedicated files so main.rs stays thin. This file
//! covers short-id / banner / formatter tests and the first half of the
//! CLI parse round-trips. Install / hook / session / services / compose
//! tests live in `tests_behavior_a.rs` and `tests_behavior_b.rs`.
//! What: parse round-trips for short_id, banners, daemon, project, session
//! subcommands (up through daemon flag parsing).
//! Test: `cargo test -p trusty-mpm` to run all fast tests.

use clap::Parser;

use crate::cli::{
    CatalogAction, Cli, Command, DEFAULT_URL, MetaAction, ProjectAction, SessionAction, TelegramCmd,
};
use crate::formatters::banner::{
    fallback_session_name, normalize_workdir, print_launch_banner,
    print_launch_banner_reconnecting, render_launch_banner, terminal_width,
};
use crate::formatters::session::short_id;

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
    assert!(matches!(cli.command, Command::Status));
}

#[test]
fn cli_parses_start() {
    let cli = Cli::try_parse_from(["trusty-mpm", "start"]).unwrap();
    assert!(matches!(cli.command, Command::Start));
}

#[test]
fn cli_parses_serve() {
    // Bare `serve` defaults to the non-stdio (start-like) behaviour.
    let cli = Cli::try_parse_from(["trusty-mpm", "serve"]).unwrap();
    assert!(matches!(cli.command, Command::Serve { stdio: false }));
}

#[test]
fn cli_parses_serve_stdio() {
    // `serve --stdio` selects the MCP stdio bridge (#1221).
    let cli = Cli::try_parse_from(["trusty-mpm", "serve", "--stdio"]).unwrap();
    assert!(matches!(cli.command, Command::Serve { stdio: true }));
}

#[test]
fn cli_parses_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "stop"]).unwrap();
    assert!(matches!(cli.command, Command::Stop));
}

#[test]
fn cli_parses_doctor() {
    let cli = Cli::try_parse_from(["trusty-mpm", "doctor"]).unwrap();
    assert!(matches!(cli.command, Command::Doctor));
}

#[test]
fn cli_parses_project_init() {
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "init"]).unwrap();
    match cli.command {
        Command::Project {
            action: ProjectAction::Init { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected project init, got {other:?}"),
    }
}

#[test]
fn cli_parses_project_init_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "init", "--dir", "/work/p"]).unwrap();
    match cli.command {
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
        cli.command,
        Command::Project {
            action: ProjectAction::List
        }
    ));
}

#[test]
fn cli_parses_project_info() {
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "info"]).unwrap();
    match cli.command {
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
    // Bare `meta run` defaults to no demo and no explicit project (#1045 WI-1).
    let cli = Cli::try_parse_from(["trusty-mpm", "meta", "run"]).unwrap();
    match cli.command {
        Command::Meta {
            action: MetaAction::Run { demo, project },
        } => {
            assert!(!demo);
            assert_eq!(project, None);
        }
        other => panic!("expected meta run, got {other:?}"),
    }
}

#[test]
fn cli_parses_meta_run_demo() {
    // `meta run --demo` sets the demo flag.
    let cli = Cli::try_parse_from(["trusty-mpm", "meta", "run", "--demo"]).unwrap();
    match cli.command {
        Command::Meta {
            action: MetaAction::Run { demo, project },
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
    match cli.command {
        Command::Meta {
            action: MetaAction::Run { demo, project },
        } => {
            assert!(demo);
            assert_eq!(project.as_deref(), Some(std::path::Path::new("/tmp")));
        }
        other => panic!("expected meta run --project, got {other:?}"),
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
    match cli.command {
        Command::Launch { dir } => assert_eq!(dir, None),
        other => panic!("expected launch, got {other:?}"),
    }
}

#[test]
fn cli_parses_launch_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "launch", "/work/p"]).unwrap();
    match cli.command {
        Command::Launch { dir } => assert_eq!(dir.as_deref(), Some("/work/p")),
        other => panic!("expected launch, got {other:?}"),
    }
}

#[test]
fn fallback_session_name_has_tmpm_prefix() {
    let name = fallback_session_name(std::path::Path::new("/work/p"));
    assert!(name.starts_with("tmpm-"), "got {name}");
}

#[test]
fn fallback_session_name_uses_folder() {
    // The offline fallback name reflects the project folder, not a UUID.
    let name = fallback_session_name(std::path::Path::new("/Users/x/trusty-mpm"));
    assert_eq!(name, "tmpm-trusty-mpm");
}

#[test]
fn launch_banner_does_not_panic() {
    // Why: the banner does width math and probes PATH; exercise it to catch
    // arithmetic underflow or formatting regressions.
    print_launch_banner("/Users/test/project", "tmpm-quiet-falcon", None);
    print_launch_banner(
        "/Users/test/project",
        "tmpm-quiet-falcon",
        Some(std::path::Path::new("/tmp/system-prompt.txt")),
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
fn launch_banner_contains_session_fields() {
    // Why: the banner is the operator's only summary of what was wired up;
    // the project, session, and prompt fields must all appear, the screen
    // must be cleared, and the launch action line must be present.
    let banner = render_launch_banner(
        "/Users/test/project",
        "tmpm-quiet-falcon",
        Some(std::path::Path::new("/tmp/INSTRUCTIONS.md")),
        None,
    );
    assert!(banner.starts_with("\x1B[2J\x1B[1;1H"), "screen not cleared");
    assert!(banner.contains("/Users/test/project"));
    assert!(banner.contains("tmpm-quiet-falcon"));
    assert!(banner.contains("/tmp/INSTRUCTIONS.md"));
    assert!(banner.contains("Launching claude..."));
    assert!(banner.contains("TRUSTY") || banner.contains("M U L T I"));
}

#[test]
fn launch_banner_marks_reconnect() {
    // Why: a reconnecting launch must not claim it started a new session;
    // the banner must show a Status row and the "Reconnecting..." action.
    let banner = render_launch_banner(
        "/Users/test/project",
        "tmpm-quiet-falcon",
        None,
        Some("tmpm-quiet-falcon"),
    );
    assert!(banner.contains("reconnecting to existing session"));
    assert!(banner.contains("Reconnecting..."));
    assert!(
        !banner.contains("Launching claude..."),
        "reconnect banner must not claim a fresh launch"
    );
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
    match cli.command {
        Command::Session {
            action: SessionAction::Start { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected session start, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_start_with_dir() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "start", "--dir", "/work/p"]).unwrap();
    match cli.command {
        Command::Session {
            action: SessionAction::Start { dir },
        } => assert_eq!(dir.as_deref(), Some("/work/p")),
        other => panic!("expected session start, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "stop", "tmpm-quiet-falcon"]).unwrap();
    match cli.command {
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
fn cli_parses_session_list() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "list"]).unwrap();
    match cli.command {
        Command::Session {
            action: SessionAction::List { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected session list, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_clean() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "clean"]).unwrap();
    match cli.command {
        Command::Session {
            action: SessionAction::Clean { dir },
        } => assert_eq!(dir, None),
        other => panic!("expected session clean, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_info() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "info", "abc-123"]).unwrap();
    match cli.command {
        Command::Session {
            action: SessionAction::Info { id_or_name },
        } => assert_eq!(id_or_name, "abc-123"),
        other => panic!("expected session info, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_instructions() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "instructions"]).unwrap();
    match cli.command {
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
    match cli.command {
        Command::Session {
            action: SessionAction::Instructions { dir },
        } => assert_eq!(dir.as_deref(), Some("/tmp")),
        other => panic!("expected session instructions, got {other:?}"),
    }
}

#[test]
fn cli_parses_events() {
    let cli = Cli::try_parse_from(["trusty-mpm", "events"]).unwrap();
    assert!(matches!(cli.command, Command::Events));
}

#[test]
fn cli_url_flag_overrides_default() {
    let cli = Cli::try_parse_from(["trusty-mpm", "--url", "http://x:9", "status"]).unwrap();
    assert_eq!(cli.url, "http://x:9");
}

#[test]
fn cli_rejects_no_subcommand() {
    // A subcommand is mandatory; bare invocation must error.
    assert!(Cli::try_parse_from(["trusty-mpm"]).is_err());
}

#[test]
fn cli_parses_tui_defaults() {
    // The `--url` flag is passed explicitly: the `tui` subcommand's `url`
    // arg carries `env = "TRUSTY_MPM_URL"`, so an ambient `TRUSTY_MPM_URL`
    // in the test runner's environment would otherwise override the clap
    // `default_value` and make this assertion environment-dependent.
    // `interval_ms` has no env binding, so its default is asserted bare.
    let cli = Cli::try_parse_from(["trusty-mpm", "tui", "--url", DEFAULT_URL]).unwrap();
    match cli.command {
        Command::Tui { url, interval_ms } => {
            assert_eq!(url, DEFAULT_URL);
            assert_eq!(interval_ms, 1000);
        }
        other => panic!("expected Tui, got {other:?}"),
    }
}

#[test]
fn cli_parses_tui_with_interval() {
    let cli = Cli::try_parse_from(["trusty-mpm", "tui", "--interval-ms", "500"]).unwrap();
    match cli.command {
        Command::Tui { interval_ms, .. } => assert_eq!(interval_ms, 500),
        other => panic!("expected Tui, got {other:?}"),
    }
}

#[test]
fn cli_parses_coordinator_tui() {
    // #1272 Child #1: the coordinator TUI is its own subcommand (the
    // `coordinator` name is taken by the SM chat command). Its `--interval-ms`
    // defaults to 1500. The `url` arg carries `env = "TRUSTY_MPM_URL"`, so an
    // ambient `TRUSTY_MPM_URL` in the test runner would override the (absent)
    // default — assert only the env-independent interval default here, and the
    // url plumbing in `cli_parses_coordinator_tui_with_flags`.
    let cli = Cli::try_parse_from(["trusty-mpm", "coordinator-tui"]).unwrap();
    match cli.command {
        Command::CoordinatorTui { interval_ms, .. } => {
            assert_eq!(interval_ms, 1500);
        }
        other => panic!("expected CoordinatorTui, got {other:?}"),
    }
}

#[test]
fn cli_parses_coordinator_tui_with_flags() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "coordinator-tui",
        "--url",
        "http://127.0.0.1:9999",
        "--interval-ms",
        "750",
    ])
    .unwrap();
    match cli.command {
        Command::CoordinatorTui { url, interval_ms } => {
            assert_eq!(url.as_deref(), Some("http://127.0.0.1:9999"));
            assert_eq!(interval_ms, 750);
        }
        other => panic!("expected CoordinatorTui, got {other:?}"),
    }
}

#[test]
fn cli_parses_telegram_pair() {
    let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "pair"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Telegram {
            cmd: TelegramCmd::Pair
        }
    ));
}

#[test]
fn cli_parses_telegram_status() {
    let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "status"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Telegram {
            cmd: TelegramCmd::Status
        }
    ));
}

#[test]
fn cli_parses_telegram_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "stop"]).unwrap();
    assert!(matches!(
        cli.command,
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
fn cli_parses_daemon_defaults() {
    let cli = Cli::try_parse_from(["trusty-mpm", "daemon"]).unwrap();
    match cli.command {
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
    match cli.command {
        Command::Daemon { tailscale, .. } => assert!(tailscale),
        other => panic!("expected Daemon, got {other:?}"),
    }
}

#[test]
fn cli_parses_supervisor() {
    let cli = Cli::try_parse_from(["trusty-mpm", "supervisor"]).unwrap();
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
        Command::Session {
            action: SessionAction::Ls { json },
        } => assert!(json),
        other => panic!("expected session ls, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_activity() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "activity", "abc"]).unwrap();
    match cli.command {
        Command::Session {
            action: SessionAction::Activity { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session activity, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_send() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "send", "abc", "hello"]).unwrap();
    match cli.command {
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
    match cli.command {
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
    match cli.command {
        Command::Session {
            action: SessionAction::Attach { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session attach, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_managed_stop() {
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "managed-stop", "abc"]).unwrap();
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
        Command::Session {
            action: SessionAction::Decommission { id },
        } => assert_eq!(id, "abc"),
        other => panic!("expected session decommission, got {other:?}"),
    }
}

#[test]
fn cli_parses_session_prune_idle() {
    // `prune-idle` (#1313) accepts both flags; defaults are false.
    let cli = Cli::try_parse_from(["trusty-mpm", "session", "prune-idle", "--dry-run", "--json"])
        .unwrap();
    match cli.command {
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
    match cli.command {
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

#[test]
fn classify_managed_target_matches_by_id() {
    // #1218: the canonical `stop`/`resume` verbs must recognize a managed
    // session by its UUID and route to the managed endpoint, not project-sessions.
    use crate::commands::managed::{ManagedSummary, classify_managed_target};
    let uuid = "11111111-2222-3333-4444-555555555555";
    let sessions: Vec<ManagedSummary> = serde_json::from_value(serde_json::json!([
        { "id": uuid, "name": "tmpm-quiet-falcon", "state": "running" }
    ]))
    .unwrap();
    assert_eq!(
        classify_managed_target(&sessions, uuid),
        Some(uuid.to_string()),
        "a managed UUID must resolve to the managed id (routes to managed endpoint)"
    );
}

#[test]
fn classify_managed_target_matches_by_name_returns_id() {
    // A friendly managed name must resolve to the canonical UUID the managed
    // endpoints require, so `tm session stop <name>` works for managed sessions.
    use crate::commands::managed::{ManagedSummary, classify_managed_target};
    let uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let sessions: Vec<ManagedSummary> = serde_json::from_value(serde_json::json!([
        { "id": uuid, "name": "tmpm-brave-otter", "state": "stopped" }
    ]))
    .unwrap();
    assert_eq!(
        classify_managed_target(&sessions, "tmpm-brave-otter"),
        Some(uuid.to_string()),
        "a managed name must resolve to its id, not be passed through verbatim"
    );
}

#[test]
fn classify_managed_target_unknown_falls_back_to_none() {
    // A non-managed id/name must NOT match — the caller then falls back to the
    // project-session path, preserving the local/project-session family (#1218).
    use crate::commands::managed::{ManagedSummary, classify_managed_target};
    let sessions: Vec<ManagedSummary> = serde_json::from_value(serde_json::json!([
        { "id": "11111111-2222-3333-4444-555555555555", "name": "tmpm-quiet-falcon", "state": "running" }
    ]))
    .unwrap();
    assert_eq!(
        classify_managed_target(&sessions, "some-local-session-name"),
        None,
        "an unknown id/name must yield None so `stop`/`resume` fall back to project-sessions"
    );
}

#[test]
fn classify_managed_target_empty_list_is_none() {
    // With no managed sessions, every argument falls back to project-sessions.
    use crate::commands::managed::{ManagedSummary, classify_managed_target};
    let sessions: Vec<ManagedSummary> = Vec::new();
    assert_eq!(classify_managed_target(&sessions, "anything"), None);
}

#[test]
fn cli_parses_catalog_sync() {
    let cli = Cli::try_parse_from(["trusty-mpm", "catalog", "sync", "--force"]).unwrap();
    match cli.command {
        Command::Catalog {
            action: CatalogAction::Sync { force },
        } => assert!(force),
        other => panic!("expected catalog sync, got {other:?}"),
    }
}

#[test]
fn cli_parses_catalog_ls() {
    let cli = Cli::try_parse_from(["trusty-mpm", "catalog", "ls"]).unwrap();
    match cli.command {
        Command::Catalog {
            action: CatalogAction::Ls { json },
        } => assert!(!json),
        other => panic!("expected catalog ls, got {other:?}"),
    }
}

#[test]
fn cli_parses_ticket() {
    // Why: the minimal form `tm ticket <issue#>` must parse with the `gh`
    // backend as the default and an empty notes list (#1237).
    use crate::commands::ticket::system::TicketSystemKind;
    let cli = Cli::try_parse_from(["trusty-mpm", "ticket", "1232"]).unwrap();
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
        current.command,
        Command::Issue {
            cmd: IssueCmd::Current { issue: 7, .. },
            ..
        }
    ));
    let states = Cli::try_parse_from(["trusty-mpm", "issue", "states"]).unwrap();
    assert!(matches!(
        states.command,
        Command::Issue {
            cmd: IssueCmd::States { .. },
            ..
        }
    ));
    let repair = Cli::try_parse_from(["trusty-mpm", "issue", "repair", "9"]).unwrap();
    assert!(matches!(
        repair.command,
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
        cli.command,
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
    match cli.command {
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
    match cli.command {
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
    match cli.command {
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
