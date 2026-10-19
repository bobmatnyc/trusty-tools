//! `disk_survey` end to end through the `tm` binary (#8985).
//!
//! Why: the unit and daemon-state tests call the tool's Rust function. Neither
//! proves that the BUILT daemon answers an MCP `tools/call` with the fields
//! #8985 added — `freshness`, `age_seconds`, `background_pass` — beside
//! `budget_clamped`. That surface is what the console and every MCP client
//! read.
//! What: builds a one-worktree git fleet inside a scratch `$HOME`, starts a
//! real `tm daemon` confined to that home with `TRUSTY_MPM_WORKSPACE_ROOT`
//! pointed at the fleet, and sends `tools/call disk_survey` as
//! `mpm.mcp.dispatch` over its socket (#6288 S3a; formerly `POST /rpc`) —
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

/// Send one JSON-RPC frame to the daemon socket and read the one-line answer.
fn socket_call(socket: &Path, method: &str, params: Value) -> Option<Value> {
    use std::io::{BufRead as _, Write as _};
    let mut stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    stream.write_all(format!("{frame}\n").as_bytes()).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// A real `tm daemon`, confined to `home`, surveying `repos_root`, serving the
/// socket it returns.
///
/// What: `common::daemon_command` — a cleared environment, so no operator
/// secret reaches the daemon (#9121) — plus a data-dir override under `home`,
/// so the socket path is known. #6288 S3a: the daemon has no TCP port; it is
/// reached on its socket only.
fn spawn_daemon(home: &Path, repos_root: &Path) -> (Daemon, PathBuf) {
    let data = home.join("data");
    let child = common::daemon_command(home, repos_root)
        .env("TRUSTY_DATA_DIR_OVERRIDE", &data)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a daemon");
    let daemon = Daemon(child);
    let socket = data.join("trusty-mpm").join("trusty-mpm.sock");
    let deadline = Instant::now() + Duration::from_secs(30);
    while socket_call(&socket, "mpm.health", json!({})).is_none_or(|r| r.get("result").is_none()) {
        assert!(
            Instant::now() < deadline,
            "no daemon answered on {}",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    (daemon, socket)
}

/// `tools/call disk_survey` with `arguments`, decoded from the MCP envelope.
///
/// #6288 S3a: sent as `mpm.mcp.dispatch` over the socket, the method that
/// replaced `POST /rpc` for `tm serve --stdio`.
fn call_disk_survey(socket: &Path, arguments: Value) -> Value {
    let envelope = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "disk_survey", "arguments": arguments },
    });
    let frame = socket_call(socket, "mpm.mcp.dispatch", envelope).expect("mpm.mcp.dispatch");
    let reply = &frame["result"];
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
    let (_daemon, socket) = spawn_daemon(&home_path, &repos_root);

    let budgeted = call_disk_survey(&socket, json!({ "budget_seconds": 30 }));
    println!("budgeted disk_survey: {budgeted}");
    assert_freshness_fields(&budgeted);

    let unbudgeted = call_disk_survey(&socket, json!({}));
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
