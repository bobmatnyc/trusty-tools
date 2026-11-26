//! `tm hook --pm-guard` and `tm hook` for the build-lease rule (#8261).
//!
//! Through the real binary: a dispatched agent's heavy build is rewritten to
//! `tm build-lease`, a wrapped one is leased whole, `tm hook` emits the
//! identical rewrite, and a dispatch — builder or not — is never refused by the
//! retired dispatch-time cap, even with the daemon down.

use crate::common;

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

/// Record a non-Fable parent model for `session_id` under `home` (#11353).
///
/// Why: a capped role's `inherit` dispatch is refused while the parent model
/// is unknown; these cases assert the builder cap, not the Fable cap.
fn record_opus_parent(home: &Path, session_id: &str) {
    trusty_mpm::core::session_model::record_session_model(
        &home.join(".trusty-mpm"),
        session_id,
        "claude-opus-5-5",
    );
}

/// Run `tm hook <args>` with `payload` on stdin; return stdout.
fn run_hook(home: &Path, args: &[&str], payload: &Value) -> String {
    run_hook_against(home, args, payload, UNREACHABLE_DAEMON)
}

/// [`run_hook`] against the daemon at `url`.
fn run_hook_against(home: &Path, args: &[&str], payload: &Value, url: &str) -> String {
    // #9011: the child runs outside any checkout; its agents come from here.
    common::stage_repo_content(home);
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
    // D60: a single leading `cd` no longer blocks compression, so the leased
    // build sits inside the compression group with the `cd` kept in front.
    assert!(
        command.starts_with("cd crates/x && { CARGO_BUILD_JOBS=6 "),
        "the composition is kept: {command}"
    );
    assert!(
        command.ends_with("} | tm compress --tool \"cargo test\""),
        "{command}"
    );
    // #8969: the relative `cd` is resolved against the payload's cwd.
    let expect = format!(
        " build-lease --expect-cwd {}/crates/x -- cargo test -p x; printf ",
        home.path().display()
    );
    assert!(command.contains(&expect), "{command}");
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
    record_opus_parent(home.path(), "s-8261");
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

/// 2026-09-25 report (acme-labs/cto-reports): a read-only `local-ops`
/// dispatch — process and service checks, no compiler — was refused by the
/// builder cap at a measured load of 71.9. Admission now follows the build
/// command, so the dispatch is admitted on the same loaded host.
#[test]
fn a_read_only_local_ops_dispatch_is_admitted_on_a_loaded_host() {
    let home = scratch_home();
    record_opus_parent(home.path(), "5f0e2c1a-1111-4222-8333-944445555666");
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

/// A daemon that would report the machine FULL is never asked at dispatch: an
/// unisolated `rust-engineer` dispatch is allowed and carries no slot notice.
// #8261: round 4 — moved from `tm_hook_pm_guard.rs`, whose mocks no longer
// serve the builder-slot route.
#[test]
fn a_rust_engineer_dispatch_never_consults_the_builder_cap() {
    let home = scratch_home();
    let url = spawn_refusing_daemon(
        r#"{"claimed":false,"cap":2,"holders":[{"agent":"rust-engineer","elapsed_secs":754}]}"#,
    );
    let payload = dispatch_payload(home.path(), "rust-engineer", "fix a bug");
    let stdout = run_hook_against(home.path(), &["--pm-guard"], &payload, &url);
    assert!(!stdout.contains("\"deny\""), "{stdout}");
    assert!(!stdout.contains("Machine-wide builder cap"), "{stdout}");
    assert!(
        !stdout.contains("CARGO_TARGET_DIR"),
        "no dispatch-time slot notice: {stdout}"
    );
}

/// 2026-09-24 trigger (mac-acme): a light `typescript-engineer` dispatch was
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
    // #8878 S5: the D4 floor denies an operand-less push where it cannot read
    // the repository, or (round 2) where the remote it picks is not
    // configured, so the hook cwd becomes a repository with `origin`.
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", "https://example.invalid/r.git"],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(home.path())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
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

/// A scratch home whose `[builders]` config leaves only the lease mechanics
/// live (the pressure, load and census gates are unit-tested elsewhere) and
/// whose slot pool is `<home>/pool`; plus an isolation worktree of
/// `acme/widget` at `<home>/widget/.claude/worktrees/agent-8261`.
fn leasing_home_and_worktree() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = scratch_home();
    let config = home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[builders]\nmax_concurrent = 2\ncount_foreign_builds = false\n\
             memory_pressure_max = \"critical\"\nmin_available_pct = 0\nload_factor = 64\n\
             slot_pool_root = \"{}\"\n",
            home.path().join("pool").display()
        ),
    )
    .expect("config");
    let worktree = home.path().join("widget/.claude/worktrees/agent-8261");
    std::fs::create_dir_all(&worktree).expect("worktree dir");
    for args in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(&args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
    (home, worktree)
}

/// #8261 round 5 (supervisor's slot-relay requirement): a dispatched agent's
/// own `cargo test`, run with the SHARED `CARGO_TARGET_DIR` its `.envrc`
/// exports, builds in its leased slot. The chain is the real one: `tm hook
/// --pm-guard` rewrites the subagent's Bash call, the rewritten command runs
/// in a shell, and a stand-in `cargo` records the `CARGO_TARGET_DIR` it got.
/// The same slot path is printed to the agent on stderr.
#[test]
fn a_subagents_leased_build_runs_in_its_slot_directory() {
    let (home, worktree) = leasing_home_and_worktree();
    let shared = home.path().join(".trusty-tools/cargo-target/acme/widget");
    let record = home.path().join("cargo-saw");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    // `cargo metadata` (the stale-build guard) fails; `cargo test` records.
    // See #10248
    trusty_common::exec_stub::write_executable(
        &bin.join("cargo"),
        format!(
            "#!/bin/sh\n[ \"$1\" = test ] || exit 1\nprintf %s \"$CARGO_TARGET_DIR\" > '{}'\n",
            record.display()
        ),
    )
    .expect("stand-in cargo");

    let payload = bash_payload(&worktree, "cargo test -p widget", true);
    let (command, _) = updated_command(&run_hook(home.path(), &["--pm-guard"], &payload));
    assert!(
        command.ends_with(" build-lease -- cargo test -p widget"),
        "the subagent's build is leased: {command}"
    );

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut shell = std::process::Command::new("/bin/sh");
    common::isolate_spawned_tm(&mut shell, home.path());
    let out = shell
        .arg("-c")
        .arg(&command)
        .current_dir(&worktree)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", &shared)
        .env("TRUSTY_MPM_URL", UNREACHABLE_DAEMON)
        .env(
            "TRUSTY_MPM_TEST_BUILD_SLOT_FALLBACK",
            home.path().join("fallback-store"),
        )
        .output()
        .expect("run the rewritten command");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(0), "{err}");
    let slot = home.path().join("pool/acme/widget/slot-0");
    let saw = std::fs::read_to_string(&record).expect("the build recorded its target");
    assert_eq!(
        Path::new(&saw),
        slot,
        "cargo builds in the leased slot: {err}"
    );
    assert!(
        err.contains(&format!("slot 0 — CARGO_TARGET_DIR={}", slot.display())),
        "the agent is told the slot path: {err}"
    );
    // #8831: the notice restates the CLAUDE.md rule's one exception, or an
    // agent reading both sees two contradictory instructions.
    assert!(
        err.contains("Never override CARGO_TARGET_DIR EXCEPT with this granted slot"),
        "the notice must carry the rule's slot exception: {err}"
    );
}

/// #8969: Claude Code strips a leading `cd <its cwd> &&` and runs the rest in
/// the directory it tracks. On 2026-09-30 that left `tm build-lease -- cargo
/// test` building another agent's worktree, green. This drives the real hook
/// and the real lease: the rewrite, minus its `cd`, run from ANOTHER directory,
/// must still build in the `cd`'s directory. Before the fix it built where the
/// shell stood.
#[test]
fn a_cd_prefixed_build_runs_in_its_directory_when_the_cd_is_lost_8969() {
    let home = scratch_home();
    let config = home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[builders]\nmax_concurrent = 2\ncount_foreign_builds = false\n\
         memory_pressure_max = \"critical\"\nmin_available_pct = 0\nload_factor = 64\n\
         heavy_build_commands = [\"sh\"]\n",
    )
    .expect("config");
    let ours = home.path().join("agent-ours");
    let theirs = home.path().join("agent-theirs");
    for dir in [&ours, &theirs] {
        std::fs::create_dir_all(dir).expect("worktree");
    }
    let ran_in = home.path().join("ran-in");
    let cd = format!("cd {} && ", ours.display());
    let command = format!("{cd}sh -c 'pwd -P > {}'", ran_in.display());
    let (rewritten, _) = updated_command(&run_hook(
        home.path(),
        &["--pm-guard"],
        &bash_payload(&ours, &command, true),
    ));
    let lost_cd = rewritten
        .strip_prefix(&cd)
        .unwrap_or_else(|| panic!("the rewrite keeps the `cd`: {rewritten}"));
    let mut shell = std::process::Command::new("/bin/sh");
    common::isolate_spawned_tm(&mut shell, home.path());
    let out = shell
        .args(["-c", lost_cd])
        .current_dir(&theirs)
        .env("TRUSTY_MPM_URL", UNREACHABLE_DAEMON)
        .env(
            "TRUSTY_MPM_TEST_BUILD_SLOT_FALLBACK",
            home.path().join("fallback-store"),
        )
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("run the rewrite");
    assert!(
        out.status.success(),
        "{lost_cd}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let built_in = std::fs::read_to_string(&ran_in).expect("the build ran");
    assert_eq!(
        built_in.trim(),
        ours.canonicalize().expect("ours").display().to_string(),
        "the build must run in the `cd`'s directory, not where the shell stood: {lost_cd}"
    );
}

/// What the pm-guard hook did with a dispatched agent's Bash `command`.
#[derive(Debug, PartialEq, Eq)]
enum Verdict11886 {
    /// Rewritten to run under `tm build-lease`.
    Leased,
    /// Refused by the build-lease rule for #11886.
    Refused,
    /// Anything else, including a pass-through with no output.
    Other(String),
}

fn verdict_11886(command: &str) -> Verdict11886 {
    let home = scratch_home();
    let stdout = run_hook(
        home.path(),
        &["--pm-guard"],
        &bash_payload(home.path(), command, true),
    );
    if stdout.contains("\"deny\"") && stdout.contains("#11886") {
        return Verdict11886::Refused;
    }
    let (rewritten, _) = match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(_) => updated_command(&stdout),
        Err(_) => return Verdict11886::Other(stdout),
    };
    if rewritten.contains(" build-lease ") {
        Verdict11886::Leased
    } else {
        Verdict11886::Other(stdout)
    }
}

fn assert_all_11886(commands: &[&str], want: Verdict11886) {
    let wrong: Vec<String> = commands
        .iter()
        .map(|c| (c, verdict_11886(c)))
        .filter(|(_, got)| *got != want)
        .map(|(c, got)| format!("{c:?} => {got:?}"))
        .collect();
    assert!(wrong.is_empty(), "want {want:?}:\n{}", wrong.join("\n"));
}

/// #11886: a shell wrapper behind a `{`, `do` or `then` — the shape the
/// compression rewrite and every loop produce — ran its build unleased.
#[test]
fn a_shell_wrapper_behind_a_keyword_or_group_is_leased_11886() {
    assert_all_11886(
        &[
            "bash --login -c 'cargo test'",
            "for p in a b; do bash -c \"cargo test -p $p\"; done",
            "if true; then sh -c 'cargo test'; fi",
        ],
        Verdict11886::Leased,
    );
}

/// #11886: a wrapper operand (`flock FILE`, `taskset MASK`) or a runner
/// (`watch`, `parallel`, BSD `script FILE`, `coproc`) hid the build.
#[test]
fn a_build_behind_a_wrapper_operand_or_runner_is_leased_11886() {
    assert_all_11886(
        &[
            "flock /tmp/l cargo test",
            "taskset 0x3 cargo test",
            "watch -n 5 cargo test",
            "parallel cargo test -p ::: a b",
            "script -q /dev/null cargo test",
            "coproc cargo test",
        ],
        Verdict11886::Leased,
    );
}

/// #11886: a shell reading its commands from a here-document, a `<<<`
/// string or a pipe runs a build the rewrite cannot reach, so it is refused.
#[test]
fn a_shell_reading_a_build_from_stdin_is_refused_11886() {
    assert_all_11886(
        &[
            "bash <<'EOF'\ncargo test\nEOF",
            "bash <<EOF\ncd x && cargo test -p x\nEOF",
            "bash <<< 'cargo test'",
            "echo 'cargo test' | bash",
            "printf 'cargo test\\n' | sh -s",
        ],
        Verdict11886::Refused,
    );
}

/// #11886: `eval` and Linux `script -c` run a string the lease cannot wrap.
#[test]
fn an_eval_or_script_string_build_is_refused_11886() {
    assert_all_11886(
        &[
            "eval cargo test",
            "eval 'cd x && cargo test'",
            "script -q -c 'cargo test' /dev/null",
        ],
        Verdict11886::Refused,
    );
}

/// #11886: a program or subcommand named by an expansion is a build the
/// hook cannot classify; it is refused with the literal, leased form.
#[test]
fn a_build_named_by_an_expansion_is_refused_11886() {
    assert_all_11886(
        &[
            "$CARGO test",
            "${CARGO:-cargo} test",
            "\"$(which cargo)\" test",
            "`which cargo` test",
            "x=test; cargo $x",
            "cargo $(echo test)",
            "for v in check test; do cargo $v; done",
        ],
        Verdict11886::Refused,
    );
}

/// #11886 Fail-Open Check: a command whose quoting the hook cannot parse
/// is refused when its words name a heavy build, never passed through.
#[test]
fn an_unparseable_build_is_refused_not_passed_through_11886() {
    assert_all_11886(
        &["bash -c 'cargo test -p x", "eval \"cargo test"],
        Verdict11886::Refused,
    );
}

/// #11886 critic round 1: tool paths under an expanded directory and a build
/// after a `#` comment holding an apostrophe are not refused; the build leases.
#[test]
fn expanded_tool_paths_and_commented_builds_pass_11886() {
    for command in [
        "$CARGO_TARGET_DIR/debug/tm doctor",
        "\"$CARGO_TARGET_DIR/debug/tm\" fleet status --tmux",
        "$HOME/.cargo/bin/cargo fmt",
        "\"$CARGO_HOME/bin/cargo\" metadata",
        "$HOME/.cargo/bin/rustup show",
        // Leased by the classifier (unit: `expansions_are_refused_only_in_a_heavy_position`);
        // here the #8879 script guard refuses it, as the sandbox has no such file.
        "$HOME/.cargo/bin/cargo test",
    ] {
        assert_ne!(verdict_11886(command), Verdict11886::Refused, "{command}");
    }
    assert_all_11886(
        &[
            "# run the crate's tests\ncargo test -p trusty-mpm --no-fail-fast",
            "watch -n 5 'cargo check'",
            "parallel 'cargo test -p {}' ::: a b",
            "flock /tmp/l -c 'cargo test'",
        ],
        Verdict11886::Leased,
    );
}

/// #11886 critic round 2: a comment's apostrophe cannot hide a build.
#[test]
fn a_comments_apostrophe_cannot_hide_a_build_11886() {
    assert_all_11886(
        &[
            "# it's\nbash -c 'cargo fmt; cargo test'",
            "# don't cache\ncargo test -p x\n# it's done",
        ],
        Verdict11886::Leased,
    );
}

/// #11886 critic round 2: an `_`-joined expansion naming cargo is refused.
#[test]
fn an_underscored_expansion_naming_cargo_is_refused_11886() {
    assert_all_11886(
        &["$CARGO_BIN test", "${CARGO_CMD} build"],
        Verdict11886::Refused,
    );
}

/// #11886 fail-closed rule: a heavy build the hook could not lease is
/// refused; a leased build still leases; a `make`-driven build passes.
#[test]
fn an_unleased_heavy_build_is_refused_and_a_make_build_is_not_11886() {
    assert_all_11886(
        &["entr -c cargo test -p x", "cargo check && entr cargo test"],
        Verdict11886::Refused,
    );
    assert_all_11886(&["cargo test -p x"], Verdict11886::Leased);
    assert_ne!(verdict_11886("make test"), Verdict11886::Refused);
}

/// #11886 option (a): an EVO or ssh build is not refused; a local wrapped
/// build still is.
#[test]
fn a_remote_runner_build_is_not_refused_11886() {
    for command in [
        "$EVO cargo +1.99.0 test -p trusty-mpm --no-fail-fast",
        "ssh host cargo test",
    ] {
        assert_ne!(verdict_11886(command), Verdict11886::Refused, "{command}");
    }
    assert_all_11886(&["entr cargo test -p x"], Verdict11886::Refused);
}

/// #11886 critic round 3: a runner word that is not the exact program word,
/// and a lease inside a nested `$(…)`, no longer let a build run bare; a `-c`
/// string the rule would refuse is leased whole.
#[test]
fn critic_round_3_shapes_are_leased_or_refused_11886() {
    assert_all_11886(
        &[
            "GIT_SSH=/usr/bin/ssh entr cargo test",
            "entr cargo test $(cargo check)",
        ],
        Verdict11886::Refused,
    );
    assert_all_11886(&["bash -c 'entr cargo test'"], Verdict11886::Leased);
}
