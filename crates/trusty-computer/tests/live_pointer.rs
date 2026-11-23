//! Live check of coordinate input bounded to the granted window (issue #11696 Phase A).
//!
//! Why: the fake daemon cannot show which coordinate space cua-driver 0.23.2
//! really uses, nor that a click and a drag land in the window they name.
//! What: builds `tests/fixtures/signedapp` (ad-hoc) and opens two copies with
//! `SIGNEDCHECK_POINTER=1` (a drag pad; SIGUSR1 shrinks the window, SIGUSR2
//! moves it). One bounded daemon runs under a temp HOME with the broker's
//! pid+window manifest for the granted copy. Each step runs the broker's own
//! sequence: [`pointer::frame_args`] read, [`pointer::frame`],
//! [`pointer::within`], then [`pointer::driver_args`] to the daemon. It checks:
//! a click at the "Press Me" button lands (the fixture logs it); a drag
//! endpoint past the frame is refused (drag itself is off, #11696); a point past the frame is refused before the daemon; after the
//! window moves, the same window coordinate still presses the button; after
//! the window shrinks, a point inside the old frame is refused
//! (delegation-authority refusal at event time); and the second copy's window
//! is refused by the bounded daemon.
//! Run: `cargo test -p trusty-computer --test live_pointer -- --ignored --nocapture`.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_computer::driver::{DriverClient, DriverError};
use trusty_computer::launcher::{APP_BINARY, DAEMON_ENV, open_argv};
use trusty_computer::manifest::{self, LeaseScope, Lifetimes};
use trusty_computer::pointer::{self, CLICK_AT_TOOL, DRAG_TOOL, Frame};

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

    /// Build and open one fixture copy with its drag pad; returns (pid, window, state file).
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
            .args(["--env", "SIGNEDCHECK_POINTER=1"])
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

fn logged(state: &Path, line: &str) {
    wait_for(line, 10, || {
        std::fs::read_to_string(state)
            .ok()
            .filter(|t| t.lines().any(|l| l == line))
    });
}

/// The frame the broker would read now, with its capture scale.
fn read_frame(client: &DriverClient, (pid, wid): (u64, u64)) -> (Frame, f64) {
    let scope = u32::try_from(pid).expect("pid");
    let reply = client
        .call(pointer::FRAME_TOOL, &pointer::frame_args(scope, wid))
        .expect("frame read");
    let frame = pointer::frame(&reply, wid).expect("a proven frame");
    let scale = reply["structuredContent"]["screenshot_scale"]
        .as_f64()
        .unwrap_or(1.0);
    println!("frame of {wid}: {frame:?} scale={scale}");
    (frame, scale)
}

/// The broker's gate then the daemon call; `Err` names the broker refusal.
fn send(
    client: &DriverClient,
    window: (u64, u64),
    tool: &str,
    args: Value,
) -> Result<Result<Value, DriverError>, &'static str> {
    let args = args.as_object().expect("object").clone();
    let (frame, _) = read_frame(client, window);
    pointer::within(frame, &pointer::points(tool, &args)).map_err(|(code, _)| code)?;
    let (name, request) = pointer::driver_args(tool, &args);
    Ok(client.call(name, &request))
}

#[test]
#[ignore = "live: builds fixture apps and runs CuaDriver.app bounded; needs an unlocked screen"]
fn live_coordinate_input_is_bounded_to_the_granted_window() {
    let out = Command::new("/usr/bin/mktemp")
        .args(["-d", "/tmp/tcomp-ptr.XXXXXX"])
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
    let win = (pid, wid);

    // Content is 420x220 points; the button's center is 210 from the left and
    // 66 above the bottom edge; the pad spans x 10..150, 10..110 above the bottom.
    let (frame, s) = read_frame(&client, win);
    let bottom = |up: f64| frame.height - up * s;
    let button = json!({"pid": pid, "window_id": wid, "x": 210.0 * s, "y": bottom(66.0)});
    send(&client, win, CLICK_AT_TOOL, button.clone())
        .expect("inside the frame")
        .expect("click delivered");
    logged(&state, "click 1");

    // Df8: a point past the frame never reaches the daemon.
    let past = json!({"pid": pid, "window_id": wid, "x": frame.width + 5.0, "y": 10.0});
    assert_eq!(
        send(&client, win, CLICK_AT_TOOL, past).err(),
        Some("coordinates_out_of_bounds")
    );
    let past = json!({"pid": pid, "window_id": wid, "from_x": 30.0 * s, "from_y": bottom(30.0),
        "to_x": 30.0 * s, "to_y": frame.height + 1.0});
    assert_eq!(
        send(&client, win, DRAG_TOOL, past).err(),
        Some("coordinates_out_of_bounds")
    );

    // A moved window keeps its window coordinates: the same point presses again.
    // SAFETY: a pid this test launched.
    unsafe { libc::kill(pid as i32, libc::SIGUSR2) };
    logged(&state, "moved");
    send(&client, win, CLICK_AT_TOOL, button)
        .expect("inside the moved frame")
        .expect("click delivered");
    logged(&state, "click 2");

    // Valid in the agent's last view, invalid at event time: the window shrank.
    let corner = json!({"pid": pid, "window_id": wid, "x": 400.0 * s, "y": 10.0 * s});
    let args = corner.as_object().expect("object").clone();
    assert!(pointer::within(frame, &pointer::points(CLICK_AT_TOOL, &args)).is_ok());
    // SAFETY: a pid this test launched.
    unsafe { libc::kill(pid as i32, libc::SIGUSR1) };
    logged(&state, "shrunk");
    wait_for("shrunk frame", 10, || {
        (read_frame(&client, win).0.width < frame.width).then_some(())
    });
    assert_eq!(
        send(&client, win, CLICK_AT_TOOL, corner).err(),
        Some("coordinates_out_of_bounds")
    );

    // The second copy's window is outside the daemon's manifest.
    let (name, request) = pointer::driver_args(
        CLICK_AT_TOOL,
        json!({"pid": other_pid, "window_id": other_wid, "x": 10, "y": 10})
            .as_object()
            .expect("object"),
    );
    let refused = client.call(name, &request);
    let bounded = matches!(&refused, Err(DriverError::ToolError { result, .. })
        if result.pointer("/structuredContent/refusal/code")
            == Some(&json!("bounded_resource_outside_manifest")));
    assert!(bounded, "second app's window: {refused:?}");
    let text = std::fs::read_to_string(&state).expect("state");
    println!("fixture log: {text:?}");
    assert!(!text.contains("click 3"), "{text}");
    drop(live);
}
