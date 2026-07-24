//! REPL unit tests, split from `mod.rs` (#357). `super` resolves to the
//! `repl` module, so the original `use super::*` imports are unchanged.

#![cfg(test)]

use super::agent_commands::detect_agent_switch;
use super::*;

#[test]
fn new_creates_history_parent_dir() {
    let repl = TrustyAgentsRepl::new(None);
    assert!(repl.is_ok(), "REPL construction should succeed: {repl:?}");
}

#[test]
fn default_history_path_under_trusty_agents() {
    if let Some(p) = default_history_path() {
        let s = p.to_string_lossy().into_owned();
        // Accepts both new (.trusty-agents) and legacy (.trusty-agents) paths.
        assert!(
            s.ends_with("/.trusty-agents/repl_history.txt")
                || s.ends_with("/.trusty-agents/repl_history.txt"),
            "{s}"
        );
    }
}

#[tokio::test]
async fn try_handle_slash_returns_none_for_non_slash() {
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    let r = repl.try_handle_slash("hello world").await;
    assert!(r.is_none());
}

#[tokio::test]
async fn try_handle_slash_help_returns_continue() {
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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
/// What: Assert the listing mentions all personas by their #3738 display
/// names (Assistant default, Izzie, CTO Bot) and still lists ctrl.
/// Test: Self-explanatory.
#[tokio::test]
async fn try_handle_slash_switch_empty_lists_choices() {
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    let r = repl.try_handle_slash("/switch").await.unwrap().unwrap();
    assert!(r.0);
    assert!(r.1.contains("Assistant"));
    assert!(r.1.contains("Izzie"));
    assert!(r.1.contains("CTO Bot"));
    assert!(r.1.contains("ctrl"));
}

/// Why: Garbage args must not silently activate a persona; surface a
/// clear error so the user can retry with a valid alias.
#[tokio::test]
async fn try_handle_slash_switch_unknown_alias_errors() {
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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

/// Write a minimal directory-package agent fixture (`<dir>/<name>/agent.toml`
/// + `persona.md`) with `role = "assistant"`, for the `#3303` tests below.
fn write_package_fixture(parent: &Path, name: &str, display_name: &str) {
    let pkg = parent.join(name);
    std::fs::create_dir(&pkg).unwrap();
    std::fs::write(
        pkg.join("agent.toml"),
        format!(
            r#"
[agent]
name = "{name}"
role = "assistant"
model = "anthropic/claude-haiku-4-5"
display_name = "{display_name}"
description = "test fixture"

[llm]
temperature = 0.5
max_tokens = 1024
"#
        ),
    )
    .unwrap();
    std::fs::write(pkg.join("persona.md"), format!("You are {display_name}.")).unwrap();
}

/// Why (#3303): `handle_agent_command_into` must resolve a directory-package
/// agent (`<name>/agent.toml` + `persona.md`, #482) — not just a flat
/// `<name>.toml` — since the bundled `assistant` agent ships exactly that
/// way. Regression test for the bug: the old code only ever checked
/// `agents_dir.join("{name}.toml")`.
/// What: Points `repl.agents_dir` (NOT `TAGENT_CONFIG_DIR` — resolution is
/// anchored to the REPL's own resolved dir per the code-critic HIGH-1 fix,
/// see `candidate_agent_dirs`) at a tempdir containing a `foo/` package,
/// dispatches `/agent foo`, and asserts the persona activated. No env
/// mutation, so no lock needed.
/// Test: Self-explanatory.
#[test]
fn handle_agent_command_resolves_directory_package_fixture() {
    let tmp = tempfile::tempdir().unwrap();
    write_package_fixture(tmp.path(), "foo", "Foo Persona");

    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    repl.agents_dir = tmp.path().to_path_buf();
    let mut out = String::new();
    repl.handle_agent_command_into("foo", &mut out);

    assert!(
        out.contains("Switched to: Foo Persona"),
        "directory-package agent should activate: {out:?}"
    );
    assert_eq!(repl.active_persona, Some("foo".to_string()));
}

/// Why (#3303 code-critic HIGH-1): `/agent <name>` must activate an agent
/// that `list_assistant_agents_into` just listed, even when the process CWD
/// (what the pre-fix `AgentConfig::by_name` searched by default) has NO
/// `.trusty-agents/agents/` of its own — the exact "standalone launch"
/// scenario `repl/mod.rs::new`'s `detect_self_project`/`current_exe` walk-up
/// exists to handle. Before the fix, `handle_agent_command_into` called
/// `AgentConfig::by_name` directly, which ignores `self.agents_dir` entirely
/// and re-derives its own dirs from `TAGENT_CONFIG_DIR`/CWD — so a name only
/// resolvable via the REPL's own resolved `agents_dir` would list but not
/// activate.
/// What: Proves the CWD-side has nothing named `standalone-fixture` (so a
/// CWD-anchored resolver could not find it), sets `repl.agents_dir` to a
/// tempdir package fixture, and asserts `/agent standalone-fixture` still
/// activates purely via `self.agents_dir`.
/// Test: Self-explanatory.
#[test]
fn handle_agent_command_resolves_when_standalone_cwd_lacks_agents_dir() {
    let cwd_shadow = std::env::current_dir()
        .unwrap()
        .join(".trusty-agents")
        .join("agents")
        .join("standalone-fixture");
    assert!(
        !cwd_shadow.exists(),
        "fixture name must not collide with a real bundled agent at {}",
        cwd_shadow.display()
    );

    let tmp = tempfile::tempdir().unwrap();
    write_package_fixture(tmp.path(), "standalone-fixture", "Standalone Fixture");

    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    repl.agents_dir = tmp.path().to_path_buf();
    let mut out = String::new();
    repl.handle_agent_command_into("standalone-fixture", &mut out);

    assert!(
        out.contains("Switched to: Standalone Fixture"),
        "self.agents_dir-resolved agent should activate even though CWD lacks it: {out:?}"
    );
    assert_eq!(repl.active_persona, Some("standalone-fixture".to_string()));
}

/// Why (#3303): the concrete regression from the issue — `/agent assistant`
/// against the REAL bundled `assistant` directory package (shipped at
/// `crates/trusty-agents/.trusty-agents/agents/assistant/`, no flat
/// `assistant.toml` sibling) must activate the persona. `cargo test`'s
/// working directory is the crate root, so `TrustyAgentsRepl::new`'s own
/// project-dir detection resolves `agents_dir` to the bundled dir with no
/// setup needed.
/// What: Dispatches `/agent assistant`, asserts activation.
/// Test: Self-explanatory.
#[test]
fn handle_agent_command_activates_bundled_assistant_package() {
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    let mut out = String::new();
    repl.handle_agent_command_into("assistant", &mut out);

    assert!(
        out.contains("Switched to:"),
        "bundled assistant package should activate: {out:?}"
    );
    assert_eq!(repl.active_persona, Some("assistant".to_string()));
}

/// Why (#3303): `/agent` with no argument must list the bundled `assistant`
/// directory-package agent, not just flat `*.toml` files — otherwise a user
/// has no way to discover the name to type.
/// What: Calls `list_assistant_agents_into` directly and asserts the
/// SUCCESS-path header plus a padded `assistant` row are present — not just
/// a bare `out.contains("assistant")`, which the empty/failure message
/// ("No assistant-role agents found...") also happens to contain (code-critic
/// MEDIUM finding).
/// Test: Self-explanatory.
#[test]
fn list_assistant_agents_into_surfaces_directory_package() {
    let repl = TrustyAgentsRepl::new(None).unwrap();
    let mut out = String::new();
    repl.list_assistant_agents_into(&mut out);

    assert!(
        out.contains("Available assistant agents:"),
        "expected the success-path header: {out:?}"
    );
    assert!(
        !out.contains("No assistant-role agents found"),
        "must not hit the empty-listing path: {out:?}"
    );
    let has_assistant_row = out
        .lines()
        .any(|line| line.split_whitespace().next() == Some("assistant"));
    assert!(
        has_assistant_row,
        "expected a padded 'assistant' row in the listing: {out:?}"
    );
}

/// Why (Bob directive, agent-picker role filter): `/agent` (no arg) must
/// exclude both a coding-role agent AND a `*.stale.bak` directory-package
/// backup (produced by the bundled reprovision, e.g. `izzie.stale.bak/
/// agent.toml`) — the latter previously leaked in as a bogus separate entry
/// literally named `izzie.stale.bak` because the old scan only checked flat
/// `<name>.toml` extensions, never a directory entry's full name, for the
/// backup suffix.
/// What: A tempdir with a real assistant package, a coding-role flat TOML,
/// and a `izzie.stale.bak/agent.toml` backup package; asserts the listing
/// keeps the assistant, and excludes both the coding agent and the
/// `.stale.bak` entry by name.
#[test]
fn list_assistant_agents_into_excludes_coding_role_and_stale_bak() {
    let tmp = tempfile::tempdir().unwrap();
    write_package_fixture(tmp.path(), "izzie", "Izzie");

    // Coding-role agent: must never surface in the assistant picker.
    std::fs::write(
        tmp.path().join("engineer.toml"),
        "[agent]\nname = \"engineer\"\nrole = \"engineer\"\ndescription = \"coding\"\n",
    )
    .unwrap();

    // Archived backup package: must never surface as a bogus separate agent.
    let stale = tmp.path().join("izzie.stale.bak");
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(
        stale.join("agent.toml"),
        "[agent]\nname = \"izzie\"\nrole = \"assistant\"\ndisplay_name = \"STALE\"\n",
    )
    .unwrap();

    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    repl.agents_dir = tmp.path().to_path_buf();
    let mut out = String::new();
    repl.list_assistant_agents_into(&mut out);

    assert!(
        out.lines().any(|l| l.trim_start().starts_with("izzie")),
        "the real assistant package must still be listed: {out:?}"
    );
    assert!(
        !out.contains("engineer"),
        "a coding-role agent must never appear in the assistant listing: {out:?}"
    );
    assert!(
        !out.contains("stale.bak"),
        "a *.stale.bak backup must never appear as a bogus separate agent: {out:?}"
    );
}

/// Why (Bob directive, agent-picker role filter): `/agents` used to bypass
/// role filtering entirely ([`discover_agent_names`], flat non-recursive
/// glob) and disagree with `/agent`. It now delegates to the same filtered
/// listing, so the two commands can never drift again.
/// What: Same fixture as the `/agent` test above; asserts `/agents`
/// (`print_agents_into`) produces byte-identical output to
/// `list_assistant_agents_into` and excludes the coding agent.
#[test]
fn print_agents_into_matches_list_assistant_agents_filtering() {
    let tmp = tempfile::tempdir().unwrap();
    write_package_fixture(tmp.path(), "izzie", "Izzie");
    std::fs::write(
        tmp.path().join("engineer.toml"),
        "[agent]\nname = \"engineer\"\nrole = \"engineer\"\n",
    )
    .unwrap();

    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    repl.agents_dir = tmp.path().to_path_buf();

    let mut agents_out = String::new();
    repl.list_assistant_agents_into(&mut agents_out);
    let mut slash_agents_out = String::new();
    repl.print_agents_into(&mut slash_agents_out);

    assert_eq!(
        agents_out, slash_agents_out,
        "/agents must delegate to the same filtered listing as /agent"
    );
    assert!(!slash_agents_out.contains("engineer"));
}

/// Why (Bob directive, agent-picker role filter): filtering the PICKER/
/// listing surface must never break direct by-name activation. A
/// coding-role agent is invisible in `/agent` / `/agents` output but must
/// still be reachable via `/agent <name>` for hidden-but-functional
/// dispatch (mirrors ctrl/pm staying dispatchable though never listed).
/// What: A coding-role flat TOML that `list_assistant_agents_into` excludes;
/// asserts `handle_agent_command_into` still activates it by exact name.
#[test]
fn handle_agent_command_into_activates_non_assistant_role_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("engineer.toml"),
        "[agent]\nname = \"engineer\"\nrole = \"engineer\"\ndisplay_name = \"Engineer\"\nmodel = \"anthropic/claude-haiku-4-5\"\ndescription = \"coding\"\n\n[llm]\ntemperature = 0.5\nmax_tokens = 1024\n\n[system_prompt]\ncontent = \"You are an engineer.\"\n",
    )
    .unwrap();

    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    repl.agents_dir = tmp.path().to_path_buf();

    // Confirm it's excluded from the listing first.
    let mut listing = String::new();
    repl.list_assistant_agents_into(&mut listing);
    assert!(!listing.contains("engineer"));

    // But by-name activation still works.
    let mut out = String::new();
    repl.handle_agent_command_into("engineer", &mut out);
    assert!(
        out.contains("Switched to: Engineer"),
        "a filtered (non-assistant-role) agent must still activate by name: {out:?}"
    );
    assert_eq!(repl.active_persona, Some("engineer".to_string()));
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
/// What: Construct an TrustyAgentsRepl whose project_dir holds a ctrl.toml
/// declaring haiku, assert resolve_active_model returns haiku.
/// Test: Self-explanatory.
#[test]
fn resolve_active_model_reads_project_ctrl_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join(".trusty-agents").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("ctrl.toml"),
        br#"[agent]
name = "ctrl"
model = "anthropic/claude-haiku-4-5"
"#,
    )
    .unwrap();

    let mut repl = TrustyAgentsRepl::new(None).unwrap();
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
    let mut repl = TrustyAgentsRepl::new(None).unwrap();
    repl.project_dir = tmp.path().to_path_buf();
    repl.project_name = "ctrl".to_string();
    // Note: this test still consults ~/.trusty-agents/agents/ctrl.toml as the
    // 2nd-priority slot, so it asserts the fallback only when that file
    // is also absent. Skip when present so CI on a dev box with a
    // user-level ctrl.toml doesn't fail.
    let user_ctrl =
        dirs::home_dir().map(|h| h.join(".trusty-agents").join("agents").join("ctrl.toml"));
    if user_ctrl.as_ref().map(|p| p.is_file()).unwrap_or(false) {
        return;
    }
    assert_eq!(repl.resolve_active_model(), "anthropic/claude-haiku-4-5");
}
