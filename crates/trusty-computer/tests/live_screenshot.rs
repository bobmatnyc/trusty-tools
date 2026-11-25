//! Live check of the bounded window screenshot (issue #11694).
//!
//! Why: the fake daemon cannot show what cua-driver 0.23.2 really captures,
//! nor that the bounded daemon refuses a second app's window.
//! What: builds `tests/fixtures/signedapp` (ad-hoc) and opens three copies
//! through LaunchServices: the granted app, a second app, and one showing a
//! password field (`SIGNEDCHECK_SECURE=1`). For each probe it starts one
//! bounded daemon on a private socket under a temp HOME with the broker's own
//! pid+window manifest (`manifest::render`), and asks for the broker's
//! screenshot request (`screenshot::driver_args`). It checks: the second
//! app's window, and the granted pid naming the second window, are refused
//! by the daemon; [`screenshot::bound`] admits the granted window as one PNG
//! whose size is that window's bounds at 1x or 2x, so the display's menu bar
//! is not in it; and it refuses the window with the password field.
//! It needs an unlocked screen: a locked one degrades the AX tree, and the
//! test fails on purpose with `screenshot_unverified` rather than pass.
//! Run: `cargo test -p trusty-computer --test live_screenshot -- --ignored --nocapture`.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_computer::driver::{DriverClient, DriverError};
use trusty_computer::launcher::{APP_BINARY, DAEMON_ENV, open_argv};
use trusty_computer::manifest::{self, LeaseScope, Lifetimes};
use trusty_computer::screenshot::{self, Refusal};

/// The PNG signature, base64-encoded (`\x89PNG\r\n\x1a\n`).
const PNG_B64_PREFIX: &str = "iVBORw0KGgo";

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

    /// Start a bounded daemon whose manifest is the broker's for `scope`.
    fn start_daemon(&self, scope: (u64, u64)) -> DriverClient {
        let scope = LeaseScope {
            pid: u32::try_from(scope.0).expect("pid"),
            window_id: scope.1,
        };
        let path = self.dir.join(format!("{}.yaml", scope.pid));
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

    /// Build and open one fixture copy; `secure` adds the password field.
    fn launch(&mut self, label: &str, secure: bool) -> (u64, u64) {
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
            .args(["--env", &format!("SIGNEDCHECK_SECURE={}", u8::from(secure))])
            .arg(&app)
            .status()
            .expect("open fixture");
        assert!(opened.success(), "open {app}: {opened}");
        let pid = wait_for("fixture pid", 20, || state_value(&state, "pid "));
        self.app_pids.push(pid as i32);
        let wid = wait_for("fixture window", 20, || state_value(&state, "window "));
        println!("{label}: pid={pid} window={wid}");
        (pid, wid)
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

/// The broker's screenshot request for `(pid, window)`, sent to `client`.
fn capture(client: &DriverClient, (pid, wid): (u64, u64)) -> Result<Value, DriverError> {
    let args = json!({"pid": pid, "window_id": wid});
    let request = screenshot::driver_args(args.as_object().expect("object"));
    client.call(screenshot::DRIVER_TOOL, &request)
}

/// True only for the bounded daemon's refusal, never a timeout or other error.
fn outside(outcome: &Result<Value, DriverError>) -> bool {
    matches!(outcome, Err(DriverError::ToolError { result, .. })
        if result.pointer("/structuredContent/refusal/code")
            == Some(&json!("bounded_resource_outside_manifest")))
}

/// [`screenshot::bound`] of a live capture, with the degraded reason printed.
fn bounded(reply: &Value, wid: u64) -> Result<Value, Refusal> {
    let sc = &reply["structuredContent"];
    println!(
        "capture of {wid}: degraded={} reason={} frame_valid={} size={}x{} bounds={}",
        sc["degraded"],
        sc["degraded_reason"],
        sc["screenshot_frame_valid"],
        sc["screenshot_width"],
        sc["screenshot_height"],
        sc["window_bounds"]
    );
    screenshot::bound(reply, wid)
}

#[test]
#[ignore = "live: builds fixture apps and runs CuaDriver.app bounded; needs an unlocked screen"]
fn live_screenshot_is_bounded_to_the_granted_window() {
    let out = Command::new("/usr/bin/mktemp")
        .args(["-d", "/tmp/tcomp-shot.XXXXXX"])
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
    let granted = live.launch("granted", false);
    let other = live.launch("other", false);
    let secure = live.launch("secure", true);

    let client = live.start_daemon(granted);
    let refused = capture(&client, other);
    assert!(outside(&refused), "second app: {refused:?}");
    let refused = capture(&client, (granted.0, other.1));
    assert!(outside(&refused), "granted pid, second window: {refused:?}");
    let reply = capture(&client, granted).expect("granted window captures");
    live.stop_daemon();

    let image = bounded(&reply, granted.1).expect("the granted window is admitted");
    let data = image["image"].as_str().expect("image data");
    assert!(data.starts_with(PNG_B64_PREFIX), "not a PNG");
    // One window at 1x or 2x: the display's menu bar strip is not in the image.
    let bounds = &reply["structuredContent"]["window_bounds"];
    let (w, h) = (bounds["width"].as_f64(), bounds["height"].as_f64());
    let (iw, ih) = (image["width"].as_f64(), image["height"].as_f64());
    let fits = |scale: f64| {
        let near = |a: Option<f64>, b: Option<f64>| {
            a.zip(b).is_some_and(|(a, b)| (a * scale - b).abs() <= 2.0)
        };
        near(w, iw) && near(h, ih)
    };
    assert!(
        fits(1.0) || fits(2.0),
        "image {iw:?}x{ih:?} is not window {bounds}"
    );

    let client = live.start_daemon(secure);
    let reply = capture(&client, secure).expect("secure window captures");
    live.stop_daemon();
    let code = bounded(&reply, secure.1).err().map(|(code, _)| code);
    assert_eq!(code, Some("sensitive_content"));
    drop(live);
}
