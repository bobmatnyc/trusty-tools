//! Live check of menu invocation under the path deny list (issue #11695).
//!
//! Why: the fake daemon cannot show what cua-driver 0.23.2 reports for a real
//! menu bar, nor that `invoke_menu` presses a real item.
//! What: builds `tests/fixtures/signedapp` (ad-hoc) and opens two copies with
//! `SIGNEDCHECK_MENU=1`: a Fixture menu with "Safe Item" and "Close Window",
//! and an app menu with "Quit SignedCheck". One bounded daemon runs under a
//! temp HOME with the broker's pid+window manifest for the granted copy. It
//! checks: [`menu::menu_paths`] of the live snapshot lists the safe item and
//! no Quit, Close or Apple-menu path; the policy refuses the Quit and Close
//! paths as `denied_menu_path`; the daemon presses "Safe Item" (the fixture
//! logs it); and the second copy's menu is refused by the bounded daemon.
//! Run: `cargo test -p trusty-computer --test live_menu -- --ignored --nocapture`.
//! Test: this file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_computer::driver::{DriverClient, DriverError};
use trusty_computer::launcher::{APP_BINARY, DAEMON_ENV, open_argv};
use trusty_computer::manifest::{self, LeaseScope, Lifetimes};
use trusty_computer::menu;
use trusty_computer::policy::{Scope, check_call};

struct Live {
    dir: PathBuf,
    app_pids: Vec<i32>,
}

impl Live {
    fn socket(&self) -> PathBuf {
        self.dir.join("drv.sock")
    }

    fn env_cmd(&self, program: &str) -> Command {
        let mut cmd = Command::new("/usr/bin/env");
        cmd.arg("-i")
            .arg(format!("HOME={}", self.dir.join("home").display()))
            .arg("PATH=/usr/bin:/bin")
            .args(DAEMON_ENV.map(|(k, v)| format!("{k}={v}")))
            .arg(program)
            .current_dir(self.dir.join("home"));
        cmd
    }

    fn stop_daemon(&self) {
        let sock = self.socket();
        let _ = self
            .env_cmd(APP_BINARY)
            .args(["stop", "--socket", &sock.to_string_lossy()])
            .status();
    }

    fn start_daemon(&self, (pid, window_id): (u64, u64)) -> DriverClient {
        let scope = LeaseScope {
            pid: u32::try_from(pid).expect("pid"),
            window_id,
        };
        let path = self.dir.join("manifest.yaml");
        std::fs::write(&path, manifest::render(scope, &Lifetimes::default())).expect("manifest");
        let argv = open_argv(&self.socket(), &path);
        let status = self
            .env_cmd(&argv[0])
            .args(&argv[1..])
            .status()
            .expect("open CuaDriver");
        assert!(status.success(), "open -a CuaDriver: {status}");
        let client = DriverClient::with_timeout(&self.socket(), Duration::from_secs(60));
        wait_for("daemon metadata", 30, || client.metadata().ok());
        client
    }

    /// Build and open one fixture copy with its menus; returns (pid, window, state file).
    fn launch(&mut self, label: &str) -> (u64, u64, PathBuf) {
        let out = self.dir.join(label);
        std::fs::create_dir_all(&out).expect("fixture dir");
        let make =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signedapp/make_app.sh");
        let built = Command::new(&make)
            .arg(&out)
            .arg("-")
            .output()
            .expect("make_app.sh");
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let app = String::from_utf8_lossy(&built.stdout).trim().to_owned();
        let state = out.join("state.txt");
        let opened = Command::new("/usr/bin/open")
            .args(["-n", "-g", "--env"])
            .arg(format!("SIGNEDCHECK_STATE={}", state.display()))
            .args(["--env", "SIGNEDCHECK_MENU=1"])
            .arg(&app)
            .status()
            .expect("open fixture");
        assert!(opened.success(), "open {app}: {opened}");
        let pid = wait_for("fixture pid", 20, || state_value(&state, "pid "));
        self.app_pids.push(pid as i32);
        let wid = wait_for("fixture window", 20, || state_value(&state, "window "));
        println!("{label}: pid={pid} window={wid}");
        (pid, wid, state)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop_daemon();
        for pid in self.app_pids.drain(..) {
            // SAFETY: a pid a fixture this test launched wrote for itself.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

fn wait_for<T>(what: &str, secs: u64, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn state_value(path: &Path, key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.trim().parse().ok())
}

fn invoke(client: &DriverClient, pid: u64, wid: u64, path: Value) -> Result<Value, DriverError> {
    client.call(
        "invoke_menu",
        &json!({"pid": pid, "window_id": wid, "path": path}),
    )
}

#[test]
#[ignore = "live: builds fixture apps and runs CuaDriver.app bounded; needs an unlocked screen"]
fn live_menu_invocation_is_scoped_and_filtered() {
    let out = Command::new("/usr/bin/mktemp")
        .args(["-d", "/tmp/tcomp-menu.XXXXXX"])
        .output()
        .expect("mktemp");
    let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
        .canonicalize()
        .expect("canonical temp dir");
    std::fs::create_dir_all(dir.join("home")).expect("home dir");
    let mut live = Live {
        dir,
        app_pids: Vec::new(),
    };
    let (pid, wid, state) = live.launch("granted");
    let (other_pid, other_wid, _) = live.launch("other");
    let client = live.start_daemon((pid, wid));

    let snap = client
        .call(
            "get_window_state",
            &json!({"pid": pid, "window_id": wid, "include_screenshot": false}),
        )
        .expect("granted window state");
    let listed = menu::menu_paths(&snap);
    println!("menu: {listed}");
    let paths = listed["paths"].as_array().expect("paths");
    assert!(paths.contains(&json!(["Fixture", "Safe Item"])), "{listed}");
    let text = listed.to_string();
    for word in ["Quit", "Close", "Apple", "Shut Down", "Log Out"] {
        assert!(!text.contains(word), "{word} listed: {text}");
    }

    let scope = Scope {
        pids: BTreeSet::from([i64::try_from(pid).expect("pid")]),
        bundle_ids: BTreeSet::new(),
    };
    for path in [
        json!(["SignedCheck", "Quit SignedCheck"]),
        json!(["Fixture", "Close Window"]),
    ] {
        let args = json!({"pid": pid, "window_id": wid, "path": path});
        let refused = check_call("invoke_menu", &args, &scope)
            .err()
            .map(|e| e.code);
        assert_eq!(refused, Some("denied_menu_path"), "{path}");
    }

    invoke(&client, pid, wid, json!(["Fixture", "Safe Item"])).expect("safe item invoked");
    wait_for("safe item logged", 10, || {
        std::fs::read_to_string(&state)
            .ok()
            .filter(|t| t.contains("menu 1"))
    });

    let refused = invoke(
        &client,
        other_pid,
        other_wid,
        json!(["Fixture", "Safe Item"]),
    );
    let bounded = matches!(&refused, Err(DriverError::ToolError { result, .. })
        if result.pointer("/structuredContent/refusal/code")
            == Some(&json!("bounded_resource_outside_manifest")));
    assert!(bounded, "second app's menu: {refused:?}");
    drop(live);
}
