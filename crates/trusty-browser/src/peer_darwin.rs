//! Darwin process identity for the enrollment socket, both directions.
//!
//! Why: `enrollment.sock` hands out pairing credentials, so each side checks
//! the other's code identity, not just its uid (`mac_peer.py`, `native_host.py`
//! `verify_enrollment_service`). The broker accepts only our native host
//! running under Chrome Stable; the native host releases bytes only to our
//! broker.
//! What: kernel facts by PID (`LOCAL_PEERPID`, `proc_pidpath`,
//! `PROC_PIDT_SHORTBSDINFO`, `KERN_PROCARGS2`) and a live `codesign -v -R`
//! against Chrome's designated requirement. The argv pins are pure functions
//! ([`host_argv_ok`], [`service_argv_ok`]) so their strictness is unit-tested.
//! Test: `kernel_facts_describe_this_process`, `host_argv_pin_is_exact`,
//! `service_argv_pin_is_exact`.

#[cfg(target_os = "macos")]
use std::ffi::OsStr;
use std::io;
use std::os::fd::AsRawFd;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The only extension origin the native host serves (trailing slash, as Chrome passes it).
pub const ORIGIN: &str = "chrome-extension://jabfhcolknbbdajilgpficniedhcfahc/";
/// Chrome Stable's executable.
pub const CHROME_EXE: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
/// Chrome's designated requirement, checked against the running parent PID.
pub const CHROME_REQUIREMENT: &str = "anchor apple generic and identifier \"com.google.Chrome\" and certificate leaf[subject.OU] = \"EQHXZ8M8AV\"";
/// Chrome flags that load another profile store or extension, open a debugger,
/// or put the browser under automation. Matched with one or two leading dashes.
// See review af942b9e95 #1: debugger-address, automation and allow-origins added.
pub const UNSAFE_FLAGS: &[&str] = &[
    "--user-data-dir",
    "--load-extension",
    "--disable-extensions-except",
    "--remote-debugging-port",
    "--remote-debugging-pipe",
    "--remote-debugging-address",
    "--enable-automation",
    "--remote-allow-origins",
];
/// The native-host subcommand name.
pub const HOST_VERB: &str = "native-host";

/// A failed host check; `code` is its audit reason (`HostRefused`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct HostRefused {
    /// Audit reason, one of `audit::REASONS`.
    pub code: &'static str,
    /// Human text for stderr.
    pub message: String,
}

impl HostRefused {
    fn new(code: &'static str, message: impl Into<String>) -> HostRefused {
        HostRefused {
            code,
            message: message.into(),
        }
    }
}

fn denied(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.to_owned())
}

/// `LOCAL_PEERPID` of a connected Unix socket; PIDs 0 and 1 are refused.
pub fn kernel_pid(sock: &impl AsRawFd) -> io::Result<i32> {
    if !cfg!(target_os = "macos") {
        return Err(denied(
            "Chrome enrollment requires macOS peer authentication",
        ));
    }
    let mut pid: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: buffer and length describe `pid`. Level 0 is SOL_LOCAL; option 2 LOCAL_PEERPID.
    let rc = unsafe { libc::getsockopt(sock.as_raw_fd(), 0, 2, (&raw mut pid).cast(), &mut len) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if pid <= 1 {
        return Err(denied("invalid peer PID"));
    }
    Ok(pid)
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    fn proc_pidinfo(
        pid: libc::c_int,
        flavor: libc::c_int,
        arg: u64,
        buffer: *mut libc::c_void,
        size: libc::c_int,
    ) -> libc::c_int;
}

/// The resolved executable path of `pid` (`proc_pidpath`, then `realpath`).
#[cfg(target_os = "macos")]
pub fn process_exe(pid: i32) -> io::Result<PathBuf> {
    let mut buf = vec![0u8; 4096];
    // SAFETY: the buffer is 4096 writable bytes.
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), 4096) };
    if n <= 0 {
        return Err(denied("cannot identify peer executable"));
    }
    buf.truncate(n as usize);
    std::fs::canonicalize(Path::new(OsStr::from_bytes(&buf)))
}

/// The parent PID of `pid` (`proc_bsdshortinfo.pbsi_ppid`); PID 1 is refused.
#[cfg(target_os = "macos")]
pub fn parent_pid(pid: i32) -> io::Result<i32> {
    let mut info = [0u8; 64];
    // SAFETY: the buffer is 64 writable bytes; flavor 13 is PROC_PIDT_SHORTBSDINFO.
    let n = unsafe { proc_pidinfo(pid, 13, 0, info.as_mut_ptr().cast(), 64) };
    if n < 40 {
        return Err(denied("cannot identify peer parent"));
    }
    let actual = u32::from_ne_bytes([info[0], info[1], info[2], info[3]]);
    let parent = u32::from_ne_bytes([info[4], info[5], info[6], info[7]]);
    if i64::from(actual) != i64::from(pid) || parent <= 1 {
        return Err(denied("invalid peer ancestry"));
    }
    i32::try_from(parent).map_err(|_| denied("invalid peer ancestry"))
}

/// The argv of `pid` (`KERN_PROCARGS2`), exactly `argc` strings.
#[cfg(target_os = "macos")]
pub fn process_argv(pid: i32) -> io::Result<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query; mib holds three ints.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || !(5..=16_777_216).contains(&size) {
        return Err(denied("cannot identify peer arguments"));
    }
    let mut buf = vec![0u8; size];
    // SAFETY: the buffer has `size` writable bytes.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(denied("cannot read peer arguments"));
    }
    buf.truncate(size);
    parse_procargs(&buf)
}

/// Parse a `KERN_PROCARGS2` buffer: argc, exec path, NUL padding, then argc strings.
pub fn parse_procargs(raw: &[u8]) -> io::Result<Vec<String>> {
    let head: [u8; 4] = raw
        .get(..4)
        .and_then(|h| h.try_into().ok())
        .ok_or_else(|| denied("invalid peer arguments"))?;
    let argc = i32::from_ne_bytes(head);
    let mut offset = raw
        .iter()
        .skip(4)
        .position(|b| *b == 0)
        .map(|p| p + 4)
        .ok_or_else(|| denied("invalid peer arguments"))?;
    if argc < 1 {
        return Err(denied("invalid peer arguments"));
    }
    while offset < raw.len() && raw[offset] == 0 {
        offset += 1;
    }
    let mut args = Vec::new();
    let mut rest = &raw[offset..];
    for _ in 0..argc {
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| denied("truncated peer arguments"))?;
        // Non-UTF-8 argv never matches a pin, so refuse it outright.
        let arg =
            std::str::from_utf8(&rest[..end]).map_err(|_| denied("invalid peer arguments"))?;
        args.push(arg.to_owned());
        rest = &rest[end + 1..];
    }
    Ok(args)
}

/// True when `arg0` is an absolute path that resolves to `own_exe`.
///
/// Why: Python's `argv[1]` named the script (`service.py`/`native_host.py`); a
/// single Rust binary has no script, so `argv[0]` carries that pin instead.
pub fn argv0_is(arg0: &str, own_exe: &Path) -> bool {
    // See review af942b9e95 #3: string checks first; canonicalize (which may
    // touch a peer-chosen, possibly hung mount) only when the path differs.
    let p = Path::new(arg0);
    if !p.is_absolute() {
        return false;
    }
    if p == own_exe {
        return true;
    }
    if p.file_name() != own_exe.file_name() {
        return false;
    }
    std::fs::canonicalize(p).is_ok_and(|r| r == own_exe)
}

/// The broker's pin on the native host's argv.
///
/// Why: Python required `argv[1:] == [native_host.py, "--socket", <sock>, ORIGIN]`
/// with the same interpreter. The Rust host is this binary, so the pin is
/// `[<this binary>, "native-host", "--socket", <state>/enrollment.sock, ORIGIN]`:
/// exactly five arguments, `argv[0]` absolute and resolving to this executable.
/// Test: `host_argv_pin_is_exact`.
pub fn host_argv_ok(argv: &[String], own_exe: &Path, enrollment_sock: &Path) -> bool {
    let sock = enrollment_sock.to_string_lossy();
    argv.len() == 5
        && argv0_is(&argv[0], own_exe)
        && argv[1] == HOST_VERB
        && argv[2] == "--socket"
        && argv[3] == sock
        && argv[4] == ORIGIN
}

/// The native host's pin on the broker's argv.
///
/// Why: Python required `[python, <service.py>, "--state", <abs dir>, "serve"]`
/// with the same interpreter, `<dir>` resolving to the socket's directory. The
/// Rust broker is this binary, so the pin is
/// `[<this binary>, "--state", <abs dir>, "serve"]`: exactly four arguments,
/// `argv[0]` absolute and resolving to this executable, the state resolving to
/// the socket's directory.
/// Test: `service_argv_pin_is_exact`.
pub fn service_argv_ok(argv: &[String], own_exe: &Path, socket_dir: &Path) -> bool {
    if argv.len() != 4 || !argv0_is(&argv[0], own_exe) || argv[1] != "--state" || argv[3] != "serve"
    {
        return false;
    }
    let state = Path::new(&argv[2]);
    let (Ok(a), Ok(b)) = (
        std::fs::canonicalize(state),
        std::fs::canonicalize(socket_dir),
    ) else {
        return false;
    };
    state.is_absolute() && a == b
}

/// The switch name of a Chrome argument: one or two leading dashes stripped,
/// any `=value` dropped; `None` for a non-switch (including a bare `--`).
fn switch_name(arg: &str) -> Option<&str> {
    let body = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'))?;
    let name = body.split_once('=').map_or(body, |(f, _)| f);
    (!name.is_empty()).then_some(name)
}

/// True when any Chrome argument (after `argv[0]`) is an unsafe flag, `=value` or not.
///
/// What: `-name` and `--name` match alike, as Chromium accepts both prefixes
/// on macOS and Linux. Every argument is scanned, so an unsafe name after a
/// bare `--` separator is still refused.
/// Test: `unsafe_chrome_flags_are_seen_with_one_dash`,
/// `unsafe_chrome_flags_after_a_bare_separator_are_refused`,
/// `automation_and_debugger_flags_are_unsafe`.
// See review af942b9e95 #1: the double-dash-only match let `-load-extension` through.
pub fn has_unsafe_flag(chrome_argv: &[String]) -> bool {
    chrome_argv
        .iter()
        .skip(1)
        .filter_map(|a| switch_name(a))
        .any(|name| {
            UNSAFE_FLAGS
                .iter()
                .any(|f| f.strip_prefix("--") == Some(name))
        })
}

/// `codesign -v -R=<requirement> <pid>` with a 5 s bound; `Ok(true)` when it passes.
pub fn codesign_pid(pid: i32, requirement: &str) -> io::Result<bool> {
    let mut child = Command::new("/usr/bin/codesign")
        .arg("-v")
        .arg(format!("-R={requirement}"))
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "codesign timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(target_os = "macos")]
fn own_exe() -> io::Result<PathBuf> {
    process_exe(std::process::id() as i32)
}

/// `verify_chrome_host`: the peer is our native host, child of Chrome Stable.
///
/// Why: only the enrolled native host, launched by the user's real Chrome,
/// may pair a profile. What: `host_not_native_host` (unreadable identity or a
/// different executable/argv), `host_parent_not_chrome`, `host_unsafe_flag`,
/// `host_timeout` (codesign over 5 s), `host_codesign` (signature failed, or
/// the peer's PID/parent changed during the check).
/// Test: `enroll-host-refused` in `captured_exchanges_replay_byte_identically`.
#[cfg(target_os = "macos")]
pub fn verify_chrome_host(sock: &impl AsRawFd, state: &Path) -> Result<(), HostRefused> {
    let not_host = |e: io::Error| HostRefused::new("host_not_native_host", e.to_string());
    let pid = kernel_pid(sock).map_err(not_host)?;
    let parent = parent_pid(pid).map_err(not_host)?;
    let me = own_exe().map_err(not_host)?;
    let argv = process_argv(pid).map_err(not_host)?;
    if process_exe(pid).map_err(not_host)? != me
        || !host_argv_ok(&argv, &me, &state.join("enrollment.sock"))
    {
        return Err(HostRefused::new(
            "host_not_native_host",
            "the enrolled Chrome native host is required",
        ));
    }
    if process_exe(parent).map_err(not_host)? != Path::new(CHROME_EXE) {
        return Err(HostRefused::new(
            "host_parent_not_chrome",
            "the native host's parent is not Chrome",
        ));
    }
    if has_unsafe_flag(&process_argv(parent).map_err(not_host)?) {
        return Err(HostRefused::new(
            "host_unsafe_flag",
            "expected the existing Chrome profile store",
        ));
    }
    let signed = match codesign_pid(parent, CHROME_REQUIREMENT) {
        Ok(ok) => ok,
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            return Err(HostRefused::new("host_timeout", "codesign timed out"));
        }
        Err(e) => return Err(not_host(e)),
    };
    let same = parent_pid(pid).ok() == Some(parent) && kernel_pid(sock).ok() == Some(pid);
    if !signed || !same {
        return Err(HostRefused::new(
            "host_codesign",
            "Chrome code identity could not be verified",
        ));
    }
    Ok(())
}

/// Off macOS the enrollment socket is closed to every peer.
#[cfg(not(target_os = "macos"))]
pub fn verify_chrome_host(_sock: &impl AsRawFd, _state: &Path) -> Result<(), HostRefused> {
    Err(HostRefused::new(
        "host_not_native_host",
        "Chrome enrollment requires macOS peer authentication",
    ))
}

/// `verify_enrollment_service`: the native host's check that `sock` is our broker.
///
/// Why: bootstrap credentials go only to the installed code. What: same
/// executable as this process and [`service_argv_ok`]; any failure is
/// `PermissionDenied`.
/// Test: `service_argv_pin_is_exact`.
#[cfg(target_os = "macos")]
pub fn verify_enrollment_service(sock: &impl AsRawFd, path: &Path) -> io::Result<()> {
    let pid = kernel_pid(sock)?;
    let me = own_exe()?;
    if process_exe(pid)? != me {
        return Err(denied("unexpected enrollment service executable"));
    }
    let dir = path
        .parent()
        .ok_or_else(|| denied("unexpected enrollment service command"))?;
    if !service_argv_ok(&process_argv(pid)?, &me, dir) {
        return Err(denied("unexpected enrollment service command"));
    }
    // See review af942b9e95 #2: re-read the peer PID, as `verify_chrome_host` does,
    // so a PID reused during the exe/argv reads is refused.
    if kernel_pid(sock)? != pid {
        return Err(denied("enrollment service changed during verification"));
    }
    Ok(())
}

/// Off macOS popup enrollment is refused.
#[cfg(not(target_os = "macos"))]
pub fn verify_enrollment_service(_sock: &impl AsRawFd, _path: &Path) -> io::Result<()> {
    Err(denied(
        "popup enrollment currently requires macOS peer verification",
    ))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn kernel_facts_describe_this_process() {
        let (a, _b) = UnixStream::pair().expect("socketpair");
        let me = std::process::id() as i32;
        assert_eq!(kernel_pid(&a).ok(), Some(me));
        let exe = std::fs::canonicalize(std::env::current_exe().expect("exe")).expect("canon");
        assert_eq!(process_exe(me).ok(), Some(exe));
        let args: Vec<String> = std::env::args().collect();
        assert_eq!(process_argv(me).ok(), Some(args));
        // SAFETY: getppid has no preconditions.
        let ppid = unsafe { libc::getppid() };
        if ppid > 1 {
            assert_eq!(parent_pid(me).ok(), Some(ppid));
        }
    }

    #[test]
    fn host_argv_pin_is_exact() {
        let exe = std::fs::canonicalize(std::env::current_exe().expect("exe")).expect("canon");
        let e = exe.to_string_lossy().into_owned();
        let sock = Path::new("/s/enrollment.sock");
        let good = v(&[&e, "native-host", "--socket", "/s/enrollment.sock", ORIGIN]);
        assert!(host_argv_ok(&good, &exe, sock));
        let bad = [
            v(&[
                "trusty-browser",
                "native-host",
                "--socket",
                "/s/enrollment.sock",
                ORIGIN,
            ]),
            v(&[
                "/bin/sh",
                "native-host",
                "--socket",
                "/s/enrollment.sock",
                ORIGIN,
            ]),
            v(&[&e, "native-host", "--socket", "/s/service.sock", ORIGIN]),
            v(&[
                &e,
                "native-host",
                "--socket",
                "/s/enrollment.sock",
                "chrome-extension://x/",
            ]),
            v(&[
                &e,
                "native-host",
                "--socket",
                "/s/enrollment.sock",
                ORIGIN,
                "--extra",
            ]),
            v(&[&e, "serve", "--socket", "/s/enrollment.sock", ORIGIN]),
        ];
        for argv in bad {
            assert!(!host_argv_ok(&argv, &exe, sock), "{argv:?}");
        }
    }

    #[test]
    fn service_argv_pin_is_exact() {
        let exe = std::fs::canonicalize(std::env::current_exe().expect("exe")).expect("canon");
        let e = exe.to_string_lossy().into_owned();
        let dir = tempfile::tempdir().expect("tmp");
        let d = dir.path().to_string_lossy().into_owned();
        assert!(service_argv_ok(
            &v(&[&e, "--state", &d, "serve"]),
            &exe,
            dir.path()
        ));
        let other = tempfile::tempdir().expect("tmp");
        let o = other.path().to_string_lossy().into_owned();
        let bad = [
            v(&["trusty-browser", "--state", &d, "serve"]),
            v(&[&e, "--state", &o, "serve"]),
            v(&[&e, "--state", "relative", "serve"]),
            v(&[&e, "--state", &d, "revoke"]),
            v(&[&e, "--state", &d, "serve", "x"]),
            v(&[&e, &d, "--state", "serve"]),
        ];
        for argv in bad {
            assert!(!service_argv_ok(&argv, &exe, dir.path()), "{argv:?}");
        }
    }

    #[test]
    fn unsafe_chrome_flags_are_seen_with_or_without_value() {
        assert!(has_unsafe_flag(&v(&["chrome", "--load-extension=/x"])));
        assert!(has_unsafe_flag(&v(&["chrome", "--remote-debugging-pipe"])));
        assert!(!has_unsafe_flag(&v(&["--user-data-dir", "--flag"])));
        assert!(parse_procargs(&[1, 0, 0, 0]).is_err());
    }

    // See review af942b9e95 #1: Chromium also takes single-dash switches.
    #[test]
    fn unsafe_chrome_flags_are_seen_with_one_dash() {
        for flag in [
            "-user-data-dir",
            "-load-extension",
            "-disable-extensions-except",
            "-remote-debugging-port",
            "-remote-debugging-pipe",
        ] {
            assert!(has_unsafe_flag(&v(&["chrome", flag])), "{flag}");
            let valued = format!("{flag}=/tmp/evil");
            assert!(has_unsafe_flag(&v(&["chrome", &valued])), "{valued}");
        }
    }

    #[test]
    fn unsafe_chrome_flags_after_a_bare_separator_are_refused() {
        assert!(has_unsafe_flag(&v(&["chrome", "--", "-load-extension=/x"])));
        assert!(has_unsafe_flag(&v(&["chrome", "--", "-user-data-dir=/x"])));
        assert!(!has_unsafe_flag(&v(&["chrome", "--", "https://x/"])));
    }

    #[test]
    fn automation_and_debugger_flags_are_unsafe() {
        for name in [
            "remote-debugging-address",
            "enable-automation",
            "remote-allow-origins",
        ] {
            for arg in [
                format!("--{name}"),
                format!("-{name}"),
                format!("--{name}=*"),
                format!("-{name}=*"),
            ] {
                assert!(has_unsafe_flag(&v(&["chrome", &arg])), "{arg}");
            }
        }
        assert!(!has_unsafe_flag(&v(&["chrome", "--enable-automation-x"])));
    }
}
