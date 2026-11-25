//! The live harness shared by the `live_*` tests that run a bounded
//! CuaDriver.app against `tests/fixtures/signedapp` copies (#11696).
//!
//! What: [`Live`] owns a temp HOME, the fixture copies it launched and the one
//! bounded daemon; dropping it stops the daemon and the copies.
//! Test: `live_coordinate_input_is_bounded_to_the_granted_window`,
//! `live_window_management_is_scoped_to_the_granted_pid`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use trusty_computer::driver::DriverClient;
use trusty_computer::launcher::{APP_BINARY, DAEMON_ENV, open_argv};
use trusty_computer::manifest::{self, LeaseScope, Lifetimes};

pub struct Live {
    pub dir: PathBuf,
    pub app_pids: Vec<i32>,
}

impl Live {
    pub fn socket(&self) -> PathBuf {
        self.dir.join("drv.sock")
    }

    pub fn env_cmd(&self, program: &str) -> Command {
        let mut cmd = Command::new("/usr/bin/env");
        cmd.arg("-i")
            .arg(format!("HOME={}", self.dir.join("home").display()))
            .arg("PATH=/usr/bin:/bin")
            .args(DAEMON_ENV.map(|(k, v)| format!("{k}={v}")))
            .arg(program)
            .current_dir(self.dir.join("home"));
        cmd
    }

    pub fn stop_daemon(&self) {
        let sock = self.socket();
        let _ = self
            .env_cmd(APP_BINARY)
            .args(["stop", "--socket", &sock.to_string_lossy()])
            .status();
    }

    pub fn start_daemon(&self, (pid, window_id): (u64, u64)) -> DriverClient {
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

    /// Build and open one fixture copy with `env` set; returns (pid, window, state file).
    pub fn launch(&mut self, label: &str, env: &[&str]) -> (u64, u64, PathBuf) {
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
            .args(env.iter().flat_map(|e| ["--env", e]))
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

pub fn wait_for<T>(what: &str, secs: u64, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn state_value(path: &Path, key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.trim().parse().ok())
}

pub fn logged(state: &Path, line: &str) {
    wait_for(line, 10, || {
        std::fs::read_to_string(state)
            .ok()
            .filter(|t| t.lines().any(|l| l == line))
    });
}

impl Live {
    /// A fresh live dir under `/tmp` (short, for the daemon socket path) with a `home`.
    pub fn new(prefix: &str) -> Live {
        let out = Command::new("/usr/bin/mktemp")
            .args(["-d", &format!("/tmp/{prefix}.XXXXXX")])
            .output()
            .expect("mktemp");
        let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
            .canonicalize()
            .expect("canonical temp dir");
        std::fs::create_dir_all(dir.join("home")).expect("home dir");
        Live {
            dir,
            app_pids: Vec::new(),
        }
    }
}
