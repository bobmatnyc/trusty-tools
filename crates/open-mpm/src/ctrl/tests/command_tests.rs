//! Tests for the REPL slash-command dispatcher (`handle_command`).

use super::super::repl::handle_command;
use super::super::state::Ctrl;

#[tokio::test]
async fn cmd_quit_returns_false() {
    let mut c = Ctrl::new();
    for cmd in ["/quit", "/exit", "/q"] {
        assert!(!handle_command(&mut c, cmd).await.unwrap(), "{cmd}");
    }
}

#[tokio::test]
async fn cmd_status_help_disconnect_unknown_return_true() {
    let mut c = Ctrl::new();
    for cmd in ["/status", "/help", "/disconnect", "/bogus"] {
        assert!(handle_command(&mut c, cmd).await.unwrap(), "{cmd}");
    }
}

#[tokio::test]
async fn cmd_connect_no_arg_errors() {
    assert!(handle_command(&mut Ctrl::new(), "/connect").await.is_err());
}

#[tokio::test]
async fn cmd_connect_valid_dir() {
    let mut c = Ctrl::new();
    let tmp = tempfile::tempdir().unwrap();
    let cmd = format!("/connect {}", tmp.path().display());
    assert!(handle_command(&mut c, &cmd).await.unwrap());
    assert_eq!(c.pms.len(), 1);
    c.shutdown_all().await;
}
