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
    _tmp: TempDir,
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
        _tmp: tmp,
    }
}

fn spawn_server(p: &Paths, idle_secs: u64) -> Child {
    Command::new(BIN)
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
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
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
            OsString::from("1"),
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
