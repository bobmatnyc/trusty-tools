//! `tm hook --pm-guard` and `tm hook` for the build-lease rule (#8261).
//!
//! Through the real binary: a dispatched agent's heavy build is rewritten to
//! `tm build-lease`, a wrapped one is leased whole, `tm hook` emits the
//! identical rewrite, and a dispatch — builder or not — is never refused by the
//! retired dispatch-time cap, even with the daemon down.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};

const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

fn scratch_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("tm-test-lease-hook-")
        .tempdir_in("/tmp")
        .expect("scratch home")
}

/// Run `tm hook <args>` with `payload` on stdin; return stdout.
fn run_hook(home: &Path, args: &[&str], payload: &Value) -> String {
    run_hook_against(home, args, payload, UNREACHABLE_DAEMON)
}

/// [`run_hook`] against the daemon at `url`.
fn run_hook_against(home: &Path, args: &[&str], payload: &Value, url: &str) -> String {
    let mut child = common::tm_command_in(home)
        .args(["--url", url, "hook"])
        .args(args)
        .current_dir(home)
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tm hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("hook exits");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn bash_payload(cwd: &Path, command: &str, subagent: bool) -> Value {
    let mut payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-8261",
        "tool_use_id": "tu-1",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command, "run_in_background": true, "timeout": 600000 },
    });
    if subagent {
        payload["agent_id"] = json!("agent-8261");
        payload["agent_type"] = json!("rust-engineer");
    }
    payload
}

fn updated_command(stdout: &str) -> (String, Value) {
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected one JSON object on stdout: {e}: {stdout:?}"));
    let input = parsed["hookSpecificOutput"]["updatedInput"].clone();
    (
        input["command"].as_str().unwrap_or_default().to_string(),
        input,
    )
}

#[test]
fn a_subagent_heavy_build_is_rewritten_to_a_lease() {
    let home = scratch_home();
    let stdout = run_hook(
        home.path(),
        &["--pm-guard"],
        &bash_payload(
            home.path(),
            "cd crates/x && CARGO_BUILD_JOBS=6 cargo test -p x",
            true,
        ),
    );
    let (command, input) = updated_command(&stdout);
    assert!(
        command.starts_with("cd crates/x && CARGO_BUILD_JOBS=6 "),
        "the composition is kept: {command}"
    );
    assert!(
        command.ends_with(" build-lease -- cargo test -p x"),
        "{command}"
    );
    assert_eq!(
        input["run_in_background"], true,
        "a background build stays backgrounded"
    );
    assert_eq!(input["timeout"], 600000);
}

#[test]
fn a_wrapped_heavy_build_is_leased_whole() {
    let home = scratch_home();
    let stdout = run_hook(
        home.path(),
        &["--pm-guard"],
        &bash_payload(home.path(), "bash -c 'cd x && cargo build'", true),
    );
    let (command, _) = updated_command(&stdout);
    assert!(
        command.ends_with(" build-lease -- bash -c 'cd x && cargo build'"),
        "{command}"
    );
}

#[test]
fn hook_emits_the_guards_lease_rewrite_for_a_heavy_build() {
    let home = scratch_home();
    let payload = bash_payload(home.path(), "cargo test -p x", true);
    let (guard, _) = updated_command(&run_hook(home.path(), &["--pm-guard"], &payload));
    let (hook, _) = updated_command(&run_hook(home.path(), &[], &payload));
    assert_eq!(
        guard, hook,
        "both PreToolUse hooks must emit the same command"
    );
    assert!(guard.contains(" build-lease -- cargo test -p x"), "{guard}");
}

/// The brief's case (a) at the dispatch: no dispatch is refused by a builder
/// cap any more — not a builder-typed one, not with the daemon down.
#[test]
fn a_dispatch_is_never_refused_by_a_builder_cap() {
    let home = scratch_home();
    for agent in ["rust-engineer", "typescript-engineer", "local-ops"] {
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s-8261",
            "tool_use_id": format!("tu-{agent}"),
            "cwd": home.path(),
            "tool_name": "Agent",
            "tool_input": {
                "subagent_type": agent,
                "description": "poll CI",
                "prompt": "check the run",
                "isolation": "worktree",
            },
        });
        let stdout = run_hook(home.path(), &["--pm-guard"], &payload);
        assert!(
            !stdout.to_lowercase().contains("builder cap"),
            "{agent} must not meet a dispatch-time builder cap: {stdout}"
        );
        assert!(
            !stdout.contains("\"deny\""),
            "{agent} dispatch denied: {stdout}"
        );
    }
}

/// A daemon that REFUSES every builder-slot claim with `refusal`, and answers
/// every other route with an empty agent list.
///
/// Why: the two 2026-09-25 reports were refusals the daemon measured — a
/// loaded host, low free memory — so the regression cases replay those exact
/// answers. Before option D the guard asked this route at dispatch and denied.
fn spawn_refusing_daemon(refusal: &'static str) -> String {
    use std::io::Read;
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        while let Ok((mut socket, _)) = listener.accept() {
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let head = loop {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => break None,
                    Ok(n) => raw.extend_from_slice(&buf[..n]),
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let Some((head, rest)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                if rest.len() >= len {
                    break Some(head.to_string());
                }
            };
            let Some(head) = head else { continue };
            let body = if head
                .lines()
                .next()
                .is_some_and(|l| l.contains("/builder-slot"))
            {
                refusal
            } else {
                r#"{"agents":[],"total":0}"#
            };
            let _ = socket.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    url
}

/// The PM's `Agent` dispatch of `agent`, unisolated, as the reports ran it.
fn dispatch_payload(cwd: &Path, agent: &str, description: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": "5f0e2c1a-1111-4222-8333-944445555666",
        "tool_use_id": format!("tu-{agent}"),
        "cwd": cwd,
        "tool_name": "Agent",
        "tool_input": { "subagent_type": agent, "description": description, "prompt": "go" },
    })
}

/// A dispatched `agent`'s Bash call.
fn agent_bash_payload(cwd: &Path, agent: &str, command: &str) -> Value {
    let mut payload = bash_payload(cwd, command, true);
    payload["agent_type"] = json!(agent);
    payload
}

/// 2026-09-25 report (duettoresearch/cto-reports): a read-only `local-ops`
/// dispatch — process and service checks, no compiler — was refused by the
/// builder cap at a measured load of 71.9. Admission now follows the build
/// command, so the dispatch is admitted on the same loaded host.
#[test]
fn a_read_only_local_ops_dispatch_is_admitted_on_a_loaded_host() {
    let home = scratch_home();
    let url = spawn_refusing_daemon(
        r#"{"claimed":false,"cap":1,"ceiling":12,"holders":[{"agent":"rust-engineer","elapsed_secs":600}],
            "capacity_reason":"1-minute load 71.9 is above 32.0 (16 logical cores x builders.load_factor 2.0)"}"#,
    );
    let payload = dispatch_payload(home.path(), "local-ops", "check launchd services");
    let stdout = run_hook_against(home.path(), &["--pm-guard"], &payload, &url);
    assert!(
        !stdout.contains("\"deny\""),
        "a read-only local-ops dispatch must not be refused: {stdout}"
    );
    assert!(!stdout.contains("71.9"), "{stdout}");
}

/// 2026-09-24 trigger (mac-duetto): a light `typescript-engineer` dispatch was
/// refused as "builder 2 of 1" on free memory 5059 MB. It is admitted now.
#[test]
fn a_light_typescript_engineer_dispatch_is_admitted_under_memory_pressure() {
    let home = scratch_home();
    let url = spawn_refusing_daemon(
        r#"{"claimed":false,"cap":1,"ceiling":4,"holders":[{"agent":"rust-engineer","elapsed_secs":60}],
            "capacity_reason":"free memory 5059 MB is below builders.free_memory_floor_mb 8192"}"#,
    );
    let payload = dispatch_payload(home.path(), "typescript-engineer", "fix a lint");
    let stdout = run_hook_against(home.path(), &["--pm-guard"], &payload, &url);
    assert!(
        !stdout.contains("\"deny\""),
        "a light typescript-engineer dispatch must not be refused: {stdout}"
    );
    assert!(!stdout.contains("5059"), "{stdout}");
}

/// The two reported agents' own commands: none compiles, so none is leased —
/// a lease is what waits for, or is refused, a slot. A heavy build by the same
/// agents is leased like anyone's.
#[test]
fn the_reported_agents_commands_take_no_build_lease() {
    let home = scratch_home();
    for (agent, command) in [
        ("local-ops", "ps aux | grep -c '[c]argo build'"),
        ("local-ops", "pgrep -fl 'cargo test'"),
        ("local-ops", "launchctl list | grep trusty"),
        ("local-ops", "cargo install --list"),
        ("local-ops", "cargo build --help"),
        ("typescript-engineer", "pnpm test"),
        ("typescript-engineer", "npx tsc --noEmit"),
        ("typescript-engineer", "npm run build"),
    ] {
        let payload = agent_bash_payload(home.path(), agent, command);
        let stdout = run_hook(home.path(), &["--pm-guard"], &payload);
        assert!(!stdout.contains("\"deny\""), "{agent}: {command}: {stdout}");
        assert!(
            !stdout.contains("build-lease"),
            "{agent}: {command} compiles nothing and takes no lease: {stdout}"
        );
    }
    for agent in ["local-ops", "typescript-engineer"] {
        let payload = agent_bash_payload(home.path(), agent, "cargo check -p x");
        let (command, _) = updated_command(&run_hook(home.path(), &["--pm-guard"], &payload));
        assert!(
            command.contains(" build-lease -- cargo check -p x"),
            "{agent}: {command}"
        );
    }
}

/// #8261 round 3 (critic finding 3): a command matching an `ask` rule keeps
/// its lease and asks, instead of silently dropping the lease.
#[test]
fn an_ask_rule_keeps_the_lease_and_asks() {
    let home = scratch_home();
    std::fs::create_dir_all(home.path().join(".claude")).expect("mkdir");
    std::fs::write(
        home.path().join(".claude/settings.json"),
        r#"{"permissions":{"ask":["Bash(git push:*)"]}}"#,
    )
    .expect("settings");
    let stdout = run_hook(
        home.path(),
        &["--pm-guard"],
        &bash_payload(home.path(), "cargo test && git push", true),
    );
    assert!(stdout.contains("build-lease -- cargo test"), "{stdout}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("one JSON object");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "ask",
        "{stdout}"
    );
}
