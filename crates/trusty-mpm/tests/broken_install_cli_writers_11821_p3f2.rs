//! A broken `install.toml` fails the `tm` CLI's state writers closed (#11821
//! P3-f2 PR 2).
//!
//! Why: these writers resolved their root with the lenient
//! `FrameworkPaths::default()`, so a broken `~/.trusty-tools/install.toml`
//! sent every write under `$HOME/.trusty-mpm`, stranded once the file is
//! fixed. The Architect ruled that writers fail closed, with no `$HOME`
//! fallback (critic H3 on #11842).
//! What: each case spawns the built `tm` with `$HOME` at a scratch dir holding
//! a broken install file and asserts the writer refuses and that its state
//! never lands under `$HOME/.trusty-mpm`. Only the child's environment is
//! set, so the module runs in the parallel `integration` target.
//! Test: this file IS the test.

use crate::common;

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The phrase every refusal in this PR carries (the lenient path's own
/// warning names `install.toml` too, so the file name alone proves nothing).
const REFUSAL: &str = "config dir is unresolved";

/// A scratch `$HOME` whose `install.toml` is broken.
struct BrokenHome {
    _tmp: tempfile::TempDir,
    home: PathBuf,
}

impl BrokenHome {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let home = tmp.path().canonicalize().expect("canonical").join("home");
        let install = trusty_common::install_paths::install_file_path(&home);
        std::fs::create_dir_all(install.parent().expect("parent")).expect("mkdir");
        std::fs::write(&install, "config_dir = \"/elsewhere\"\nbogus = 1\n").expect("write");
        Self { _tmp: tmp, home }
    }

    /// `$HOME/.trusty-mpm`, the fallback root no writer may use.
    fn fallback_root(&self) -> PathBuf {
        self.home.join(".trusty-mpm")
    }

    /// A `tm` confined to this home, reaching no daemon.
    fn tm(&self) -> Command {
        let mut cmd = common::tm_command_in(&self.home);
        common::socket::aim(&mut cmd, "http://127.0.0.1:1");
        cmd.env_remove("TRUSTY_MPM_DISABLE_HOOKS")
            .env_remove("CLAUDE_MPM_SUB_AGENT")
            .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
            .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT");
        cmd
    }
}

/// Run `cmd` with `stdin` written and closed.
fn run_with_stdin(mut cmd: Command, stdin: &str) -> Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tm");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for tm")
}

/// Both output streams, for assertion messages.
fn text(out: &Output) -> String {
    format!(
        "status={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `tm doctor --quarantine-mcp --yes` refuses; before the fix it renamed the
/// file and released its claim in the `$HOME` ledger.
#[test]
fn doctor_quarantine_mcp_refuses_on_a_broken_install_file() {
    let fx = BrokenHome::new();
    let stray_dir = fx.home.join("above");
    std::fs::create_dir_all(&stray_dir).expect("mkdir");
    let stray = stray_dir.join(".mcp.json");
    std::fs::write(&stray, "{}").expect("write stray");
    let out = fx
        .tm()
        .current_dir(&fx.home)
        .args(["doctor", "--quarantine-mcp"])
        .arg(&stray)
        .arg("--yes")
        .output()
        .expect("spawn tm doctor");
    let all = text(&out);
    assert!(all.contains(REFUSAL), "the step must refuse: {all}");
    assert!(stray.exists(), "the stray must stay in place: {all}");
}

/// `tm project unadopt` refuses; before the fix it rewrote the `$HOME`
/// adoption ledger.
#[test]
fn project_unadopt_refuses_on_a_broken_install_file() {
    let fx = BrokenHome::new();
    let tree = fx.home.join("wt-a");
    std::fs::create_dir_all(&tree).expect("mkdir tree");
    let ledger = fx.fallback_root().join("project-registry/worktrees.json");
    std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
    let rows = serde_json::json!([{
        "path": tree, "project": "legacy", "checkout": fx.home,
        "branch": "old", "adopted_at": "2026-10-31T00:58:00Z"
    }]);
    let bytes = serde_json::to_vec_pretty(&rows).expect("encode");
    std::fs::write(&ledger, &bytes).expect("seed ledger");
    let out = fx
        .tm()
        .args(["--url", "http://127.0.0.1:1", "project", "unadopt"])
        .arg(&tree)
        .output()
        .expect("spawn tm project unadopt");
    let all = text(&out);
    assert!(!out.status.success(), "unadopt must fail: {all}");
    assert!(all.contains(REFUSAL), "must name the cause: {all}");
    assert_eq!(
        std::fs::read(&ledger).expect("read ledger"),
        bytes,
        "the $HOME ledger must be untouched: {all}"
    );
}

/// The PM guard's file-change budget is never reached: the trust-anchor floor
/// denies every write first (`a_broken_install_file_denies_every_write`), so
/// no counter lands under `$HOME/.trusty-mpm/state`. This pins why the budget
/// writer needs no fail-closed arm of its own; it passed before PR 2 too.
#[test]
fn pm_guard_budget_is_never_reached_on_a_broken_install_file() {
    let fx = BrokenHome::new();
    let outside = tempfile::tempdir().expect("non-checkout cwd");
    let payload = r#"{"hook_event_name":"PreToolUse","session_id":"issue-11821","tool_name":"Bash","tool_input":{"command":"python3 <<'PY' > src/lib.rs\nprint(1)\nPY"}}"#;
    let mut cmd = fx.tm();
    cmd.current_dir(outside.path()).args(["hook", "--pm-guard"]);
    let out = run_with_stdin(cmd, payload);
    let all = text(&out);
    assert!(out.status.success(), "the guard exits 0: {all}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"deny\""),
        "the floor must deny the edit: {all}"
    );
    assert!(
        !fx.fallback_root()
            .join("state/pm_guard_turn_budget")
            .exists(),
        "no budget counter under $HOME: {all}"
    );
}

/// An undelivered `SubagentStop` fails the hook; before the fix it was
/// parked under `$HOME/.trusty-mpm`, where the daemon never drains it.
#[test]
fn subagent_stop_is_not_parked_on_a_broken_install_file() {
    let fx = BrokenHome::new();
    let payload = serde_json::json!({
        "session_id": "d6066d66-8c7f-41f1-8914-8d0710d563aa",
        "hook_event_name": "SubagentStop",
        "agent_id": "a403cdbc078b5c474",
        "agent_type": "general-purpose",
        "last_assistant_message": "ALPHA"
    })
    .to_string();
    let mut cmd = fx.tm();
    cmd.arg("hook");
    let out = run_with_stdin(cmd, &payload);
    let all = text(&out);
    assert!(!out.status.success(), "a lost stop fails the hook: {all}");
    assert!(all.contains(REFUSAL), "must name the cause: {all}");
    assert!(
        !trusty_mpm::core::stop_spool::unposted_stops_dir(&fx.fallback_root()).exists(),
        "no stop parked under $HOME: {all}"
    );
}

/// The prompt-feedback capture writes no row; before the fix the row landed
/// in the `$HOME` ledger.
#[test]
fn prompt_feedback_hook_writes_no_row_on_a_broken_install_file() {
    let fx = BrokenHome::new();
    let transcript = fx.home.join("transcript.jsonl");
    let line = serde_json::json!({
        "type": "assistant",
        "message": { "role": "assistant", "id": "msg-1", "content": [
            {"type": "text", "text": "Done.\n\n## Prompt feedback\n\nThe rung was ambiguous.\n"}
        ]}
    });
    std::fs::write(&transcript, format!("{line}\n")).expect("transcript");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "sess-1",
        "cwd": "/tmp/proj",
        "transcript_path": transcript,
    })
    .to_string();
    let mut cmd = fx.tm();
    cmd.args(["hook", "--prompt-feedback"]);
    let out = run_with_stdin(cmd, &payload);
    let all = text(&out);
    assert!(out.status.success(), "the capture always exits 0: {all}");
    assert!(
        !trusty_mpm::core::prompt_feedback::ledger_path(&fx.fallback_root()).exists(),
        "no prompt-feedback row under $HOME: {all}"
    );
}

/// `tm repair savings-ledger --apply` refuses; before the fix it planned and
/// applied against the `$HOME` ledger.
#[test]
fn repair_savings_ledger_refuses_on_a_broken_install_file() {
    let fx = BrokenHome::new();
    let out = fx
        .tm()
        .args(["repair", "savings-ledger", "--apply"])
        .output()
        .expect("spawn tm repair");
    let all = text(&out);
    assert!(!out.status.success(), "the repair must refuse: {all}");
    assert!(all.contains(REFUSAL), "must name the cause: {all}");
}

/// Wait for `child` up to `limit`; kill it and return `None` past the bound.
fn wait_bounded(mut child: std::process::Child, limit: Duration) -> Option<Output> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if child.try_wait().expect("try_wait").is_some() {
            return Some(child.wait_with_output().expect("collect"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// `tm supervisor` refuses at start; before the fix it created
/// `$HOME/.trusty-mpm/session-manager` and kept running.
#[test]
fn supervisor_refuses_on_a_broken_install_file() {
    let fx = BrokenHome::new();
    let empty_path = fx.home.join("empty-path");
    std::fs::create_dir_all(&empty_path).expect("mkdir");
    let child = fx
        .tm()
        .env("PATH", &empty_path)
        .arg("supervisor")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tm supervisor");
    let out = wait_bounded(child, Duration::from_secs(30));
    let created = fx.fallback_root().join("session-manager").exists();
    let out = out.expect("the supervisor must exit, not run on the $HOME fallback");
    let all = text(&out);
    assert!(!out.status.success(), "the supervisor must refuse: {all}");
    assert!(all.contains(REFUSAL), "must name the cause: {all}");
    assert!(!created, "no session store under $HOME: {all}");
}

/// `tm meta run` refuses before its first write; before the fix it created
/// the project's meta-session dir and launched on the `$HOME` root.
#[test]
fn meta_run_refuses_on_a_broken_install_file() {
    let fx = BrokenHome::new();
    let project = fx.home.join("proj");
    std::fs::create_dir_all(&project).expect("mkdir project");
    let empty_path = fx.home.join("empty-path");
    std::fs::create_dir_all(&empty_path).expect("mkdir");
    let child = fx
        .tm()
        .env("PATH", &empty_path)
        .args(["meta", "run", "--no-provision", "--timeout-secs", "1"])
        .arg("--project")
        .arg(&project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tm meta run");
    let out = wait_bounded(child, Duration::from_secs(30)).expect("meta run must exit");
    let all = text(&out);
    assert!(!out.status.success(), "meta run must refuse: {all}");
    assert!(all.contains(REFUSAL), "must name the cause: {all}");
    assert!(
        !project.join(".trusty-mpm").exists(),
        "nothing written before the refusal: {all}"
    );
}
