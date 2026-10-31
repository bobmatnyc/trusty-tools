//! The real [`super::Host`]: `lsof`, `ps`, `kill`, `launchctl` and one HTTP
//! probe (#10571 P1b). Every call is bounded; none reads a credential.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{Host, Listener};

/// The host's own processes, launchd and loopback HTTP.
pub(crate) struct RealHost;

/// stdout of `cmd args`, or None when it fails to run or exits non-zero.
fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether `pid` still exists.
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

impl Host for RealHost {
    fn listener(&self, port: u16) -> Result<Option<Listener>, String> {
        let out = Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
            .output()
            .map_err(|e| format!("lsof could not run: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        // lsof exits 1 with no output when nothing matches; anything else is a failure.
        if !out.status.success() {
            if stdout.trim().is_empty() && out.stderr.is_empty() && out.status.code() == Some(1) {
                return Ok(None);
            }
            return Err("lsof failed".to_owned());
        }
        let mut pids: Vec<u32> = stdout
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect();
        pids.sort_unstable();
        pids.dedup();
        let pid = match pids.as_slice() {
            [] => return Ok(None),
            [pid] => *pid,
            _ => return Err(format!("{} programs listen on the port", pids.len())),
        };
        let argv = output("ps", &["-o", "args=", "-p", &pid.to_string()])
            .ok_or_else(|| format!("ps could not read pid {pid}"))?
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let cwd =
            output("lsof", &["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix('n'))
                    .map(PathBuf::from)
            });
        Ok(Some(Listener { pid, argv, cwd }))
    }

    fn agent_loaded(&self, label: &str) -> bool {
        #[cfg(target_os = "macos")]
        {
            trusty_common::launchd::label_is_loaded(label)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = label;
            false
        }
    }

    fn render(&self, dir: &Path, port: u16) -> anyhow::Result<String> {
        super::render_plist(dir, port)
    }

    fn bootstrap(&self, plist: &Path) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let domain = format!("gui/{}", trusty_common::launchd::current_uid());
            let out = Command::new("launchctl")
                .arg("bootstrap")
                .arg(&domain)
                .arg(plist)
                .output()?;
            anyhow::ensure!(
                out.status.success(),
                "launchctl bootstrap failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = plist;
            anyhow::bail!("starting the dashboard at login needs macOS (launchd)")
        }
    }

    fn bootout(&self, label: &str) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let target = format!("gui/{}/{label}", trusty_common::launchd::current_uid());
            let _ = Command::new("launchctl")
                .args(["bootout", &target])
                .output()?;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = label;
            Ok(())
        }
    }

    fn terminate(&self, pid: u32) -> bool {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        !alive(pid)
    }

    fn respawn(&self, dir: &Path, port: u16) -> anyhow::Result<()> {
        use std::os::unix::process::CommandExt;
        let python = trusty_common::bin_resolve::resolve_binary("python3")
            .unwrap_or_else(|| PathBuf::from("/usr/bin/python3"));
        let logs = super::log_dir(dir);
        std::fs::create_dir_all(&logs)?;
        let log = std::fs::File::create(logs.join("rollback.log"))?;
        Command::new(python)
            .arg(dir.join(super::SCRIPT))
            .arg("--port")
            .arg(port.to_string())
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()?;
        Ok(())
    }

    fn serving(&self, port: u16) -> bool {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if probe(port) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        false
    }
}

/// One `GET /config.json` with the dashboard's Host guard name; true on a 200.
fn probe(port: u16) -> bool {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let Ok(mut s) = TcpStream::connect_timeout(&addr.into(), Duration::from_secs(1)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let req = format!("GET /config.json HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 16];
    s.read(&mut head)
        .is_ok_and(|n| String::from_utf8_lossy(&head[..n]).contains(" 200"))
}
