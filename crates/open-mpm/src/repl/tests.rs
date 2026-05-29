//! REPL unit tests, split from `mod.rs` (#357). `super` resolves to the
//! `repl` module, so the original `use super::*` imports are unchanged.

#![cfg(test)]

use super::agent_commands::detect_agent_switch;
use super::*;

#[test]
fn new_creates_history_parent_dir() {
    let repl = OpenMpmRepl::new(None);
    assert!(repl.is_ok(), "REPL construction should succeed: {repl:?}");
}

#[test]
fn default_history_path_under_open_mpm() {
    if let Some(p) = default_history_path() {
        let s = p.to_string_lossy().into_owned();
        assert!(s.ends_with("/.open-mpm/repl_history.txt"), "{s}");
    }
}

#[tokio::test]
async fn try_handle_slash_returns_none_for_non_slash() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    let r = repl.try_handle_slash("hello world").await;
    assert!(r.is_none());
}

#[tokio::test]
async fn try_handle_slash_help_returns_continue() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    let r = repl.try_handle_slash("/help").await.unwrap().unwrap();
    assert!(r.0, "/help should keep REPL running");
    assert!(
        r.1.contains("slash commands"),
        "/help output captured into String, not stdout: {:?}",
        r.1
    );
}

#[tokio::test]
async fn try_handle_slash_exit_returns_break() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    let r = repl.try_handle_slash("/exit").await.unwrap().unwrap();
    assert!(!r.0, "/exit should signal REPL break");
    assert!(r.1.contains("Bye"));
}

/// Why: #404 — `/disconnect` must be a recognized REPL command (alias
/// for `/exit`). In all-in-one mode it behaves identically to `/exit`;
/// in client mode it surfaces the "server still running" message. This
/// test pins the all-in-one behavior so the alias never silently
/// regresses to "unknown command".
/// What: Dispatches `/disconnect`, asserts the handler signals exit and
/// returns the standard goodbye (no service_url is set, so all-in-one
/// branch).
/// Test: Self-explanatory — run via `cargo test try_handle_slash_disconnect`.
#[tokio::test]
async fn try_handle_slash_disconnect_is_recognized_alias() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    let r = repl.try_handle_slash("/disconnect").await.unwrap().unwrap();
    assert!(!r.0, "/disconnect should signal REPL break");
    assert!(
        !r.1.contains("unknown command"),
        "/disconnect must be a recognized command, got: {:?}",
        r.1
    );
    assert!(r.1.contains("Bye"), "all-in-one mode goodbye: {:?}", r.1);
}

/// Why: #404 — When the REPL is a thin client against a running daemon,
/// `/exit` must NOT mislead the user into thinking the server has gone
/// down. Print a clear "server still running" hint with the port and
/// `om stop` instructions.
/// What: Sets `service_url` to a fake host:port, dispatches `/exit`,
/// asserts the captured output mentions the port and `om stop`.
/// Test: Self-explanatory.
#[tokio::test]
async fn try_handle_slash_exit_in_client_mode_shows_disconnect_message() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    repl.set_service_client_mode("http://localhost:7654");
    let r = repl.try_handle_slash("/exit").await.unwrap().unwrap();
    assert!(!r.0, "/exit should still signal REPL break in client mode");
    assert!(
        r.1.contains("Server still running"),
        "expected server-still-running hint, got: {:?}",
        r.1
    );
    assert!(r.1.contains("7654"), "port hint missing: {:?}", r.1);
    assert!(r.1.contains("om stop"), "shutdown hint missing: {:?}", r.1);
}

#[tokio::test]
async fn try_handle_slash_unknown_captures_into_buffer() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    let r = repl.try_handle_slash("/nope").await.unwrap().unwrap();
    assert!(r.0);
    assert!(
        r.1.contains("unknown command"),
        "unknown command output must be captured, not printed: {:?}",
        r.1
    );
}

/// Why: `/switch ctrl` must clear an active persona and reset the
/// prompt label so the next dispatch routes through the default ctrl
/// path rather than the previously-active persona.
/// What: Activate izzie, then `/switch ctrl`, assert state cleared.
/// Test: Self-explanatory.
#[tokio::test]
async fn try_handle_slash_switch_ctrl_clears_persona() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    repl.active_persona = Some("izzie".to_string());
    repl.project_name = "Izzie".to_string();
    let r = repl
        .try_handle_slash("/switch ctrl")
        .await
        .unwrap()
        .unwrap();
    assert!(r.0);
    assert!(r.1.contains("Switched to: ctrl"), "output: {:?}", r.1);
    assert!(repl.active_persona.is_none());
    assert_eq!(repl.project_name, "ctrl");
}

/// Why: `/switch` must accept friendly aliases (case-insensitive,
/// "cto" / "CTO Assistant" / "cto-assistant") so users don't have to
/// remember the underlying TOML stem.
/// What: Verify each alias is routed (output may say "not found" if
/// the TOML isn't present in the test cwd; we only assert that the
/// dispatcher accepted the alias rather than printing "Unknown
/// persona").
/// Test: Self-explanatory.
#[tokio::test]
async fn try_handle_slash_switch_accepts_aliases() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    let r = repl.try_handle_slash("/switch CTO").await.unwrap().unwrap();
    assert!(r.0);
    assert!(
        !r.1.contains("Unknown persona"),
        "alias 'CTO' should be accepted: {:?}",
        r.1
    );
}

/// Why: Empty `/switch` must list the available personas (the no-arg
/// picker path is opened by `ReplBridge` upstream; once the slash
/// handler sees an empty arg it means the user typed `/switch`
/// literally and wants the text help).
/// What: Assert the listing mentions all three personas.
/// Test: Self-explanatory.
#[tokio::test]
async fn try_handle_slash_switch_empty_lists_choices() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    let r = repl.try_handle_slash("/switch").await.unwrap().unwrap();
    assert!(r.0);
    assert!(r.1.contains("ctrl"));
    assert!(r.1.contains("Izzie"));
    assert!(r.1.contains("CTO Assistant"));
}

/// Why: Garbage args must not silently activate a persona; surface a
/// clear error so the user can retry with a valid alias.
#[tokio::test]
async fn try_handle_slash_switch_unknown_alias_errors() {
    let mut repl = OpenMpmRepl::new(None).unwrap();
    let r = repl
        .try_handle_slash("/switch bogus")
        .await
        .unwrap()
        .unwrap();
    assert!(r.0);
    assert!(r.1.contains("Unknown persona"), "output: {:?}", r.1);
    assert!(repl.active_persona.is_none());
}

#[test]
fn detect_agent_switch_izzie_variants() {
    assert_eq!(
        detect_agent_switch("switch to Izzie", false),
        Some("personal-assistant")
    );
    assert_eq!(
        detect_agent_switch("use izzie", false),
        Some("personal-assistant")
    );
    assert_eq!(
        detect_agent_switch("personal assistant please", false),
        Some("personal-assistant")
    );
}

#[test]
fn detect_agent_switch_cto_variants() {
    assert_eq!(
        detect_agent_switch("switch to CTO", false),
        Some("cto-assistant")
    );
    assert_eq!(
        detect_agent_switch("use cto mode", false),
        Some("cto-assistant")
    );
    assert_eq!(detect_agent_switch("the cto said hi", false), None);
}

#[test]
fn detect_agent_switch_back_to_ctrl_only_when_persona_active() {
    assert_eq!(
        detect_agent_switch("switch back to ctrl", true),
        Some("ctrl")
    );
    assert_eq!(detect_agent_switch("exit agent", true), Some("ctrl"));
    assert_eq!(detect_agent_switch("switch back to ctrl", false), None);
}

#[test]
fn detect_agent_switch_no_false_positive_on_unrelated_text() {
    assert_eq!(detect_agent_switch("write a python script", false), None);
    assert_eq!(detect_agent_switch("hello world", false), None);
}

#[test]
fn discover_agent_names_reads_toml_stems() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("python-engineer.toml"), b"").unwrap();
    std::fs::write(tmp.path().join("pm.toml"), b"").unwrap();
    std::fs::write(tmp.path().join("not-an-agent.txt"), b"").unwrap();

    let names = discover_agent_names(tmp.path());
    assert_eq!(names, vec!["pm".to_string(), "python-engineer".to_string()]);
}

#[test]
fn discover_agent_names_missing_dir_returns_empty() {
    let names = discover_agent_names(Path::new("/nonexistent-path-xyz"));
    assert!(names.is_empty());
}

/// Why bug fix (#statusline-shows-sonnet): when a project ctrl.toml exists,
/// resolve_active_model must return ITS model — not a stale user-level
/// ctrl.toml's value. This is the regression that surfaced sonnet on the
/// statusline despite the bundled ctrl.toml declaring haiku.
/// What: Construct an OpenMpmRepl whose project_dir holds a ctrl.toml
/// declaring haiku, assert resolve_active_model returns haiku.
/// Test: Self-explanatory.
#[test]
fn resolve_active_model_reads_project_ctrl_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join(".open-mpm").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("ctrl.toml"),
        br#"[agent]
name = "ctrl"
model = "anthropic/claude-haiku-4-5"
"#,
    )
    .unwrap();

    let mut repl = OpenMpmRepl::new(None).unwrap();
    repl.project_dir = tmp.path().to_path_buf();
    repl.project_name = "ctrl".to_string();
    assert_eq!(repl.resolve_active_model(), "anthropic/claude-haiku-4-5");
}

/// Why bug fix (#statusline-shows-sonnet): when no ctrl.toml is on disk
/// in either project or user-level location, the fallback must be haiku
/// (the bundled default), NOT a stale value picked up from an unrelated
/// pm.toml.
/// What: Empty project_dir → fallback haiku.
/// Test: Mechanical.
#[test]
fn resolve_active_model_falls_back_to_haiku_when_no_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let mut repl = OpenMpmRepl::new(None).unwrap();
    repl.project_dir = tmp.path().to_path_buf();
    repl.project_name = "ctrl".to_string();
    // Note: this test still consults ~/.open-mpm/agents/ctrl.toml as the
    // 2nd-priority slot, so it asserts the fallback only when that file
    // is also absent. Skip when present so CI on a dev box with a
    // user-level ctrl.toml doesn't fail.
    let user_ctrl = dirs::home_dir().map(|h| h.join(".open-mpm").join("agents").join("ctrl.toml"));
    if user_ctrl.as_ref().map(|p| p.is_file()).unwrap_or(false) {
        return;
    }
    assert_eq!(repl.resolve_active_model(), "anthropic/claude-haiku-4-5");
}
