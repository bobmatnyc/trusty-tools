//! Live check: does bounded mode admit a signed bundle by `bundle_id`? (issue #11697)
//!
//! Why: the spike saw a `bundle_id` entry fail for GrantCheck.app, a shell stub
//! that execs osascript and so carries no attested bundle id (GO-STOP.md:141-142).
//! A real, signed bundle was untested, and the grants flow depends on the answer
//! (plan section 3).
//! What: builds and signs `tests/fixtures/signedapp` (ad-hoc, or the identity in
//! `TRUSTY_SIGNED_FIXTURE_IDENTITY`) plus an ad-hoc impostor with the same
//! bundle id, opens both through LaunchServices, then starts one bounded
//! cua-driver daemon per probe on a private socket under a temp HOME
//! (`env -i`, as `scripts/sandbox_computer_daemon.sh` does). The broker's
//! pid+window manifest (`manifest::render`) admits the app and refuses the
//! impostor. A `bundle_id` entry admits the app and refuses another id, but
//! also admits the impostor: it does not attest the signing identity, so the
//! broker ships pid-only scoping. A driver that starts refusing the impostor
//! fails this test on purpose. Evidence: `fixtures/evidence/bundle-id-0.23.2.txt`.
//! Teardown stops each daemon by its private socket and each app by pid.
//! The daemon is started directly: the sandbox broker renders pid manifests only.
//! Run: `cargo test -p trusty-computer --test live_bundle_scope -- --ignored --nocapture`.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::json;
use trusty_computer::driver::{DriverClient, DriverError};
use trusty_computer::launcher::{APP_BINARY, DAEMON_ENV, open_argv};
use trusty_computer::manifest::{self, LeaseScope, Lifetimes};

const BUNDLE_ID: &str = "com.trusty.spike.signedcheck";

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
        let client = DriverClient::with_timeout(&sock, Duration::from_secs(2));
        wait_for("daemon stop", 15, || {
            client.metadata().is_err().then_some(())
        });
    }

    /// Start a bounded daemon on the private socket with `yaml` as its manifest.
    fn start_daemon(&self, name: &str, yaml: &str) -> DriverClient {
        let path = self.dir.join(name);
        std::fs::write(&path, yaml).expect("manifest");
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

/// The bundle-only manifest: same tool ceiling, no desktop pid or window.
fn bundle_manifest(bundle_id: &str, life: &Lifetimes) -> String {
    let tools: String = manifest::TOOLS
        .iter()
        .map(|t| format!("    - {t}\n"))
        .collect();
    format!(
        "version: 3\nexpires_after: {}\nidle_timeout: {}\nallow:\n  tools:\n{tools}\
         resources:\n  apps:\n    - bundle_id: {bundle_id}\n      launch: false\n\
         \x20     windows: all\n      terminate: driver_launched\n  desktop:\n\
         \x20   applications: []\n    windows: []\n    display: false\n",
        life.expires_after, life.idle_timeout,
    )
}

fn window_state(client: &DriverClient, pid: u64, wid: u64) -> Result<u64, DriverError> {
    let args = json!({"pid": pid, "window_id": wid, "include_screenshot": false});
    let snap = client.call("get_window_state", &args)?;
    Ok(snap["structuredContent"]["element_count"]
        .as_u64()
        .unwrap_or(0))
}

/// Build `variant` of the fixture into `out`, open it through LaunchServices,
/// and return `(pid, window_id)`; the pid is recorded on `live` for teardown.
fn launch(live: &mut Live, ident: &str, variant: &str) -> (u64, u64) {
    let out = live.dir.join(variant);
    std::fs::create_dir_all(&out).expect("fixture dir");
    let make = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signedapp/make_app.sh");
    let built = Command::new(&make)
        .arg(&out)
        .arg(ident)
        .arg(variant)
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
        .arg(&app)
        .status()
        .expect("open fixture");
    assert!(opened.success(), "open {app}: {opened}");
    let pid = wait_for("fixture pid", 20, || state_value(&state, "pid "));
    live.app_pids.push(pid as i32);
    let wid = wait_for("fixture window", 20, || state_value(&state, "window "));
    println!("{variant} ({ident}) pid={pid} window={wid}");
    (pid, wid)
}

/// Start a daemon with `yaml`, ask for one window, stop it.
fn probe(live: &Live, label: &str, yaml: &str, (pid, wid): (u64, u64)) -> Result<u64, DriverError> {
    let client = live.start_daemon(&format!("{label}.yaml"), yaml);
    let outcome = window_state(&client, pid, wid);
    println!("{label}: {outcome:?}");
    live.stop_daemon();
    outcome
}

/// True only for the daemon's bounded refusal, never a timeout or other error.
fn outside(outcome: Result<u64, DriverError>) -> bool {
    matches!(outcome, Err(DriverError::ToolError { result, .. })
        if result.pointer("/structuredContent/refusal/code")
            == Some(&json!("bounded_resource_outside_manifest")))
}

#[test]
#[ignore = "live: builds signed fixture apps and runs CuaDriver.app bounded; run explicitly"]
fn live_bounded_mode_admits_signed_bundle_id_but_not_its_identity() {
    let out = Command::new("/usr/bin/mktemp")
        .args(["-d", "/tmp/tcomp-bid.XXXXXX"])
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
    let ident = std::env::var("TRUSTY_SIGNED_FIXTURE_IDENTITY").unwrap_or_else(|_| "-".into());
    let life = Lifetimes::default();
    let real = launch(&mut live, &ident, "signed");
    // An ad-hoc build with the same bundle id: what any local process can make.
    let impostor = launch(&mut live, "-", "impostor");

    let scope = LeaseScope {
        pid: real.0 as u32,
        window_id: real.1,
    };
    let pid_yaml = manifest::render(scope, &life);
    probe(&live, "control-pid", &pid_yaml, real).expect("control: pid manifest admits");
    assert!(
        outside(probe(&live, "pid-vs-impostor", &pid_yaml, impostor)),
        "the pid manifest must refuse any other process"
    );
    let ours = bundle_manifest(BUNDLE_ID, &life);
    let other = bundle_manifest("com.trusty.spike.notthisone", &life);
    probe(&live, "bundle", &ours, real).expect("bundle_id manifest admits the signed bundle");
    assert!(
        outside(probe(&live, "bundle-other-id", &other, real)),
        "a bundle_id entry for another id must refuse"
    );
    // The finding that keeps pid-only scoping (#11697): the entry matches the
    // claimed bundle id, not a signing identity, so the impostor is admitted.
    probe(&live, "bundle-vs-impostor", &ours, impostor)
        .expect("bundle_id admits any same-id bundle; if this now refuses, revisit #11697");
    drop(live);
}
