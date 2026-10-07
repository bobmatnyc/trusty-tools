//! The real `trusty-secrets` binary: spawn, one call, idle exit (#9065).
//!
//! Why: owner ruling 28 — a lazily started socket is not a daemon. The proof
//! is the built binary exiting on its own after one call, with its socket
//! removed, and a second instance refusing to clobber a live first one.
//! What: every path the binary uses is a flag pointing into a `TempDir`, so
//! nothing touches `~/.trusty-tools`; the calls are `secrets.doctor`, which
//! opens no keychain item.
//! Test: itself.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use trusty_common::uds::server::RpcResponse;
use trusty_common::uds::{connect_hardened, peer_pid, send_framed_request, socket_is_serving};
use trusty_secrets::server::OnDemandSecrets;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-secrets");

struct Paths {
    tmp: TempDir,
    socket: PathBuf,
    index: PathBuf,
    machine: PathBuf,
}

fn paths() -> Paths {
    let tmp = TempDir::new().unwrap();
    Paths {
        socket: tmp.path().join("run").join("s.sock"),
        index: tmp.path().join("index"),
        machine: tmp.path().join("machine.yaml"),
        tmp,
    }
}

fn server_command(p: &Paths, idle_secs: u64) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(["serve", "--socket"])
        .arg(&p.socket)
        .arg("--index-dir")
        .arg(&p.index)
        .arg("--machine-config")
        .arg(&p.machine)
        .args(["--idle-timeout-secs", &idle_secs.to_string()])
        .env_remove("TRUSTY_SECRETS_SOCKET")
        .env_remove("TRUSTY_SECRETS_INDEX_DIR")
        .env_remove("TRUSTY_SECRETS_IDLE_TIMEOUT_SECS")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn spawn_server(p: &Paths, idle_secs: u64) -> Child {
    server_command(p, idle_secs).spawn().unwrap()
}

async fn wait_serving(socket: &Path) {
    let started = Instant::now();
    while !socket_is_serving(socket, Duration::from_millis(200)).await {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "socket {} never served",
            socket.display()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn doctor(socket: &Path) -> Value {
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "secrets.doctor", "params": null});
    let response: RpcResponse = send_framed_request(socket, &request, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(response.error.is_none(), "{:?}", response.error);
    response.result.unwrap()
}

/// Wait for `child` to exit on its own, inside `bound`.
fn wait_exit(child: &mut Child, bound: Duration) -> std::process::ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if started.elapsed() > bound {
            let _ = child.kill();
            panic!(
                "trusty-secrets pid {} still running after {bound:?}",
                child.id()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `pid` names a live, non-zombie process.
fn pid_is_running(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Why: the acceptance test — one call, then the process exits by itself
/// after the idle window and its socket file is gone.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_exits_after_idle_and_removes_its_socket() {
    let p = paths();
    let mut child = spawn_server(&p, 1);
    wait_serving(&p.socket).await;
    let report = doctor(&p.socket).await;
    assert_eq!(report["socket"], p.socket.display().to_string());

    let status = wait_exit(&mut child, Duration::from_secs(15));
    assert!(status.success(), "{status:?}");
    assert!(!pid_is_running(child.id()));
    assert!(!p.socket.exists(), "socket file removed on exit");
}

/// Why: a second binary on a live socket exits non-zero and the first keeps
/// serving on an untouched socket.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_binary_is_refused_while_the_first_serves() {
    let p = paths();
    let mut first = spawn_server(&p, 30);
    wait_serving(&p.socket).await;
    let mut second = spawn_server(&p, 30);
    let status = wait_exit(&mut second, Duration::from_secs(15));
    assert_eq!(status.code(), Some(1));
    doctor(&p.socket).await;
    assert!(first.try_wait().unwrap().is_none(), "first instance alive");
    let _ = first.kill();
    let _ = first.wait();
}

/// Why: ruling 31 — a client spawns the server on first call through the
/// on-demand helper; the server then exits when idle.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_demand_client_spawns_the_server_and_it_exits_when_idle() {
    let p = paths();
    assert!(!p.socket.exists());
    let client = OnDemandSecrets::at(&p.socket)
        .with_program(BIN)
        .with_server_args([
            OsString::from("--index-dir"),
            p.index.clone().into_os_string(),
            OsString::from("--machine-config"),
            p.machine.clone().into_os_string(),
            OsString::from("--idle-timeout-secs"),
            // #9065: 3 s, so the liveness asserts below beat the idle exit under CI load.
            OsString::from("3"),
        ]);
    let report = client.call("secrets.doctor", Value::Null).await.unwrap();
    assert_eq!(report["index_root"], p.index.display().to_string());

    let stream = connect_hardened(&p.socket).await.unwrap();
    let pid = peer_pid(&stream).expect("server pid");
    drop(stream);
    assert!(pid_is_running(pid));

    let started = Instant::now();
    while p.socket.exists() || pid_is_running(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "server pid {pid} or its socket outlived the idle window"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A checkout at `dir` whose `origin` is `url`.
fn repo(dir: &Path, url: &str) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", url]] {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(&args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}

/// Why: the detached server keeps its first spawner's environment for its
/// whole life. An inherited `GIT_DIR` or a `GIT_CONFIG_KEY_n` override of
/// `remote.origin.url` must not make every project resolve to one vault.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inherited_git_redirect_env_never_reaches_the_servers_git_calls() {
    let p = paths();
    let decoy = p.tmp.path().join("decoy");
    let alpha = p.tmp.path().join("alpha");
    let beta = p.tmp.path().join("beta");
    repo(&decoy, "git@github.com:evil/decoy.git");
    repo(&alpha, "git@github.com:acme/alpha.git");
    repo(&beta, "git@github.com:acme/beta.git");

    let mut child = server_command(&p, 30)
        .env("GIT_DIR", decoy.join(".git"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "remote.origin.url")
        .env("GIT_CONFIG_VALUE_0", "git@github.com:evil/override.git")
        .spawn()
        .unwrap();
    wait_serving(&p.socket).await;
    let mut vaults = Vec::new();
    for project in [&alpha, &beta] {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "secrets.scopes",
                             "params": {"project": project.display().to_string()}});
        let response: RpcResponse =
            send_framed_request(&p.socket, &request, Duration::from_secs(10))
                .await
                .unwrap();
        vaults.push(match response.result {
            Some(result) => result["scopes"][0]["vault"].clone(),
            None => json!(format!("error: {:?}", response.error)),
        });
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        vaults,
        [json!("trusty/acme/alpha"), json!("trusty/acme/beta")]
    );
}

/// Why: #7519 P4 A10, owner ruling Q1 — the real binary takes
/// `OP_SERVICE_ACCOUNT_TOKEN` out of its environment at start, and doctor
/// reports only that it was present (on a `cli-backends` build, the one
/// that reads it). The token reaches neither the reply nor the server's
/// stderr.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_doctor_reports_token_presence_and_never_the_token() {
    const TOKEN: &str = "ops_binary_canary_7519_p4_fedcba9876543210";
    let p = paths();
    let log = p.tmp.path().join("stderr.log");
    let mut child = server_command(&p, 1)
        .env("OP_SERVICE_ACCOUNT_TOKEN", TOKEN)
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    wait_serving(&p.socket).await;
    let report = doctor(&p.socket).await;
    assert_eq!(
        report["headless"]["onepassword_token"],
        json!(cfg!(feature = "cli-backends"))
    );
    assert!(!report.to_string().contains(TOKEN), "{report}");
    let status = wait_exit(&mut child, Duration::from_secs(15));
    assert!(status.success(), "{status:?}");
    let stderr = std::fs::read_to_string(&log).unwrap();
    assert!(!stderr.is_empty(), "the server wrote no stderr at all");
    assert!(!stderr.contains(TOKEN), "{stderr}");
}

/// Why: #7519 P4, DOC-74 §7 — the real binary detects an unsupported tool
/// on the `PATH` it started with, and runs nothing it finds.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_doctor_detects_tools_on_its_start_path() {
    use std::os::unix::fs::PermissionsExt;
    let p = paths();
    let bin = p.tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = p.tmp.path().join("doppler-ran");
    let doppler = bin.join("doppler");
    std::fs::write(&doppler, format!("#!/bin/sh\n: > '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&doppler, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = server_command(&p, 1)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .spawn()
        .unwrap();
    wait_serving(&p.socket).await;
    let report = doctor(&p.socket).await;
    let found = report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "doppler")
        .cloned()
        .unwrap();
    assert_eq!(found["installed"], json!(true));
    assert_eq!(found["path"], json!(doppler.display().to_string()));
    assert!(wait_exit(&mut child, Duration::from_secs(15)).success());
    assert!(!marker.exists(), "the server ran doppler");
}
