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

use crate::cli::{Cli, Command, DEFAULT_URL, ProjectAction, SessionAction, TelegramCmd};
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
    let cli = Cli::try_parse_from(["trusty-mpm", "serve"]).unwrap();
    assert!(matches!(cli.command, Command::Serve));
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
