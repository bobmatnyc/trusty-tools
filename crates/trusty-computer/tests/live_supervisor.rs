//! Live check: a bounded cua-driver daemon on a private socket, scoped to the SignedCheck fixture.
//!
//! Why: the fake driver cannot prove LaunchServices launch, the pin, bounded
//! scoping or the restart cost (issue #11460, Phase 2).
//! What: builds and opens `tests/fixtures/signedapp` (it writes its pid and
//! window id to a state file; #11699: cua-driver 0.23.2 sees no AXWindow in
//! the osascript `fixture.js`), writes `<dir>/state/scope.json`, starts `trusty-computer
//! serve` through `scripts/sandbox_computer_daemon.sh`, then over the private
//! socket does one `get_window_state` and one element click confirmed by the
//! fixture's state file. Two SIGHUP rescopes (window +1, then back) time the
//! restart. Teardown stops the sandbox (which stops its daemon) and the
//! fixture by pid. It never names, signals or connects to any other daemon.
//! Run: `cargo test -p trusty-computer --test live_supervisor -- --ignored --nocapture`.
//! Test: this file.

mod common;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use common::live::wait_for;
use serde_json::{Value, json};
use trusty_computer::driver::DriverClient;

struct Live {
    dir: PathBuf,
    _apps: common::live::Live,
    sandbox: Option<Child>,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = Command::new(script())
            .args(["--stop", &self.dir.to_string_lossy()])
            .status();
        if let Some(mut s) = self.sandbox.take() {
            let _ = s.wait();
        }
        // Dropping `_apps` after this terminates the fixture by pid.
    }
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sandbox_computer_daemon.sh")
}

fn clicks(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|t| t.lines().filter(|l| l.starts_with("click ")).count())
        .unwrap_or(0)
}

fn next_line(out: &mut BufReader<ChildStdout>, prefix: &str) -> String {
    let mut line = String::new();
    loop {
        line.clear();
        let n = out.read_line(&mut line).expect("read sandbox stdout");
        assert!(n > 0, "sandbox exited before printing '{prefix}'");
        print!("sandbox| {line}");
        if line.starts_with(prefix) {
            return line.trim().to_owned();
        }
    }
}

fn write_scope(dir: &Path, pid: u64, window_id: u64) {
    let body = json!({"pid": pid, "window_id": window_id}).to_string();
    std::fs::write(dir.join("state/scope.json"), body).expect("scope.json");
}

fn hup(dir: &Path) {
    let pid = std::fs::read_to_string(dir.join("sandbox.pid")).expect("sandbox.pid");
    let pid: i32 = pid.trim().parse().expect("pid");
    // SAFETY: the pid is the sandbox's own `trusty-computer serve` child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGHUP) }, 0);
}

#[test]
#[ignore = "live: launches CuaDriver.app bounded and the SignedCheck fixture; run explicitly"]
fn live_bounded_daemon_drives_fixture() {
    let mut apps = common::live::Live::new("tcomp-live");
    let dir = apps.dir.clone();
    std::fs::create_dir_all(dir.join("state")).expect("state dir");
    let (pid, wid, state_file) = apps.launch("fixture", &[]);
    let mut live = Live {
        dir: dir.clone(),
        _apps: apps,
        sandbox: None,
    };
    write_scope(&dir, pid, wid);

    let mut sandbox = Command::new(script())
        .args(["--bin", env!("CARGO_BIN_EXE_trusty-computer"), "--dir"])
        .arg(&dir)
        .stdout(Stdio::piped())
        .spawn()
        .expect("sandbox script");
    let mut out = BufReader::new(sandbox.stdout.take().expect("stdout"));
    live.sandbox = Some(sandbox);
    let ready = next_line(&mut out, "ready ");
    let socket = dir.join("state/driver/cua-driver.sock");
    assert!(
        ready.contains(&format!("socket={}", socket.display())),
        "{ready}"
    );
    let client = DriverClient::with_timeout(&socket, Duration::from_secs(60));
    let meta = client.metadata().expect("metadata");
    println!("daemon metadata: {}", Value::Object(meta.clone()));
    assert_eq!(meta["driver_version"], "0.23.2");

    for (label, window) in [("away", wid + 1), ("back", wid)] {
        write_scope(&dir, pid, window);
        hup(&dir);
        println!("rescope {label}: {}", next_line(&mut out, "rescoped "));
    }

    let args = json!({"pid": pid, "window_id": wid, "include_screenshot": false});
    let snap = client
        .call("get_window_state", &args)
        .expect("get_window_state");
    let sc = &snap["structuredContent"];
    println!(
        "get_window_state: element_count={} degraded={} reason={}",
        sc["element_count"], sc["degraded"], sc["degraded_reason"]
    );
    let token = sc["elements"]
        .as_array()
        .and_then(|els| els.iter().find(|e| e["label"] == "Press Me"))
        .and_then(|e| e["element_token"].as_str())
        .map(str::to_owned)
        .expect("a 'Press Me' element in the fixture snapshot");
    let before = clicks(&state_file);
    let started = Instant::now();
    let click = json!({"pid": pid, "window_id": wid, "element_token": token});
    client.call("click", &click).expect("click");
    wait_for("fixture click", 10, || {
        (clicks(&state_file) == before + 1).then_some(())
    });
    println!(
        "click confirmed by fixture state in {} ms",
        started.elapsed().as_millis()
    );
    drop(live);
    assert!(
        client.metadata().is_err(),
        "own daemon still answers after stop"
    );
}
