//! `disk_survey` end to end through the `tm` binary (#8985).
//!
//! Why: the unit and daemon-state tests call the tool's Rust function. Neither
//! proves that the BUILT daemon answers an MCP `tools/call` with the fields
//! #8985 added — `freshness`, `age_seconds`, `background_pass` — beside
//! `budget_clamped`. That surface is what the console and every MCP client
//! read.
//! What: builds a one-worktree git fleet inside a scratch `$HOME`, starts a
//! real `tm daemon` confined to that home with `TRUSTY_MPM_WORKSPACE_ROOT`
//! pointed at the fleet, and POSTs `tools/call disk_survey` to its `/rpc` —
//! the endpoint `tm serve --stdio` forwards every MCP request to — once with
//! a budget and once without. Only the CHILD's environment is set, so this
//! module leaves the process environment alone.
//! Test: run with
//! `cargo test -p trusty-mpm --test integration disk_survey_mcp_smoke:: -- --nocapture`.

use crate::common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Run `git -C <dir> <args>`, panicking with git's stderr on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("`git {}` could not run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `<home>/repos/owner/repo` with one commit and one registered worktree.
fn fleet(home: &Path) -> PathBuf {
    let root = home.join("repos");
    let repo = root.join("owner").join("repo");
    std::fs::create_dir_all(&repo).expect("create the repo dir");
    git(&repo, &["init", "--initial-branch=main"]);
    git(&repo, &["config", "user.email", "ci@test.invalid"]);
    git(&repo, &["config", "user.name", "CI"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README.md"), "base\n").expect("write README");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "base"]);
    let wt = repo.join(".worktrees").join("a");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "session/a",
            wt.to_str().expect("utf8 worktree path"),
        ],
    );
    root
}

/// A `tm daemon` that is killed when this value drops, pass or panic.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Whether `GET /health` on `port` answers 200.
fn healthy(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    let mut reply = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut reply).is_ok()
        && reply.starts_with("HTTP/1.1 200")
}

/// A real `tm daemon` on `port`, confined to `home`, surveying `repos_root`.
///
/// What: the same isolation `tm_build_lease::spawn_daemon` uses — `--force`
/// because no launchd supervises it, orphan GC and the Telegram bot off.
fn spawn_daemon(home: &Path, repos_root: &Path, port: u16) -> Daemon {
    let child = common::tm_command_in(home)
        .current_dir(home)
        .env("TRUSTY_MPM_ORPHAN_GC", "0")
        .env("TRUSTY_MPM_WORKSPACE_ROOT", repos_root)
        .env_remove("TELEGRAM_BOT_TOKEN")
        .args(["daemon", "--force", "--addr"])
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a daemon");
    let daemon = Daemon(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !healthy(port) {
        assert!(Instant::now() < deadline, "no daemon answered on {port}");
        std::thread::sleep(Duration::from_millis(100));
    }
    daemon
}

/// `tools/call disk_survey` with `arguments`, decoded from the MCP envelope.
fn call_disk_survey(client: &reqwest::blocking::Client, url: &str, arguments: Value) -> Value {
    let envelope = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "disk_survey", "arguments": arguments },
    });
    let reply: Value = client
        .post(format!("{url}/rpc"))
        .json(&envelope)
        .send()
        .expect("POST /rpc")
        .json()
        .expect("a JSON reply");
    let result = &reply["result"];
    assert_eq!(result["isError"], false, "{reply}");
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content: {reply}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("survey is not JSON ({e}): {text}"))
}

/// Assert the #8985 fields on one survey response.
fn assert_freshness_fields(survey: &Value) {
    let freshness = survey["freshness"].as_str().unwrap_or_default();
    assert!(
        ["live", "cached", "partial"].contains(&freshness),
        "freshness: {survey}"
    );
    assert!(
        survey["age_seconds"].as_u64().is_some(),
        "age_seconds: {survey}"
    );
    let pass = survey["background_pass"].as_str().unwrap_or_default();
    assert!(
        ["running", "idle"].contains(&pass),
        "background_pass: {survey}"
    );
    assert_eq!(survey["budget_clamped"], false, "{survey}");
}

/// #8985 smoke: the built daemon answers `disk_survey` with the new fields,
/// budgeted and unbudgeted, and the unbudgeted call is a live pass.
#[test]
fn the_built_daemon_answers_disk_survey_with_the_freshness_fields() {
    let home = tempfile::Builder::new()
        .prefix("tm-test-disk-smoke-")
        .tempdir_in("/tmp")
        .expect("scratch home");
    // git records resolved paths; `/tmp` is a symlink on macOS.
    let home_path = std::fs::canonicalize(home.path()).expect("canonical home");
    let repos_root = fleet(&home_path);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let _daemon = spawn_daemon(&home_path, &repos_root, port);
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("an HTTP client");

    let budgeted = call_disk_survey(&client, &url, json!({ "budget_seconds": 30 }));
    println!("budgeted disk_survey: {budgeted}");
    assert_freshness_fields(&budgeted);

    let unbudgeted = call_disk_survey(&client, &url, json!({}));
    println!("unbudgeted disk_survey: {unbudgeted}");
    assert_freshness_fields(&unbudgeted);
    assert_eq!(unbudgeted["freshness"], "live", "{unbudgeted}");
    assert_eq!(unbudgeted["partial"], false, "{unbudgeted}");
    assert_eq!(
        unbudgeted["root"]["path"],
        repos_root.to_str().expect("utf8 root"),
        "the daemon surveyed the scratch fleet: {unbudgeted}"
    );
    let worktrees = unbudgeted["root"]["projects"][0]["worktrees"]
        .as_array()
        .unwrap_or_else(|| panic!("no worktree rows: {unbudgeted}"));
    // The main checkout is listed as a row too, beside the one worktree.
    assert_eq!(worktrees.len(), 2, "{unbudgeted}");
    assert!(
        worktrees.iter().any(|w| w["path"]
            .as_str()
            .is_some_and(|p| p.ends_with(".worktrees/a"))),
        "the registered worktree is listed: {unbudgeted}"
    );
}
