//! Which app a pid really is, checked before the consent dialog shows.
//!
//! Why: a `grant_request` names a `bundle_id` and a `pid`; without a check
//! the dialog shows the claimed bundle while the grant drives whatever app
//! owns the pid, and the sensitive-app check reads the claimed string too
//! (#11460 code-critic finding 2).
//! What: [`AppResolver`] is the seam; [`LsappinfoResolver`] asks
//! LaunchServices through `/usr/bin/lsappinfo info -only bundleid -only name
//! <pid>`, and [`parse_lsappinfo`] reads its `"Key"="value"` lines. Tests
//! use a fake resolver, so no test runs `lsappinfo`.
//! The resolver also reads the process start time (`proc_pidinfo`
//! `PROC_PIDTBSDINFO`), so a grant names one process, not a pid that can be
//! reused; [`check_same`] re-checks it at confirm and at every call
//! (#11460 pid reuse).
//! Test: `grant_request_refuses_a_pid_of_another_app`,
//! `sensitive_check_reads_the_resolved_bundle`, `lsappinfo_output_parses`,
//! `app_changed_between_request_and_confirm_refuses`,
//! `app_changed_between_confirm_and_call_revokes`.

use std::process::{Command, Stdio};

/// The app LaunchServices reports for a pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedApp {
    /// `CFBundleIdentifier`.
    pub bundle_id: String,
    /// `LSDisplayName`; the bundle id when the app has none.
    pub name: String,
    /// Process start time in microseconds since the epoch; 0 when unread.
    pub started_us: u64,
}

/// Resolve a pid to its app.
pub trait AppResolver: Send + Sync {
    /// The app running as `pid`, or why it cannot be named.
    fn resolve(&self, pid: u32) -> Result<ResolvedApp, String>;
}

/// The real resolver: `lsappinfo`, read-only, no TCC prompt.
#[derive(Debug, Default)]
pub struct LsappinfoResolver;

impl AppResolver for LsappinfoResolver {
    fn resolve(&self, pid: u32) -> Result<ResolvedApp, String> {
        let out = Command::new("/usr/bin/lsappinfo")
            .args([
                "info",
                "-only",
                "bundleid",
                "-only",
                "name",
                &pid.to_string(),
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("lsappinfo: {e}"))?;
        if !out.status.success() {
            return Err(format!("lsappinfo exited {}", out.status));
        }
        parse_lsappinfo(&String::from_utf8_lossy(&out.stdout))
            .ok_or_else(|| format!("pid {pid} is not a LaunchServices app"))
            .and_then(|mut app| {
                app.started_us = start_time_us(pid)?;
                Ok(app)
            })
    }
}

/// `Ok` when `pid` is still the process a grant named: same bundle, same start time.
///
/// `Err` says what changed; a vanished process is an `Err` too.
pub fn check_same(
    resolver: &dyn AppResolver,
    pid: u32,
    bundle_id: &str,
    started_us: u64,
) -> Result<(), String> {
    let app = resolver.resolve(pid)?;
    if app.bundle_id != bundle_id {
        return Err(format!(
            "pid {pid} is now {}, not {bundle_id}",
            app.bundle_id
        ));
    }
    if app.started_us != started_us {
        return Err(format!("pid {pid} is a new process (start time changed)"));
    }
    Ok(())
}

/// The start time of `pid` in microseconds since the epoch.
#[cfg(target_os = "macos")]
pub fn start_time_us(pid: u32) -> Result<u64, String> {
    let pid = i32::try_from(pid).map_err(|_| format!("pid {pid} out of range"))?;
    // SAFETY: `info` is a plain C struct, zeroed, and its size is passed.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| "proc_bsdinfo size".to_owned())?;
    // SAFETY: the buffer is `size` bytes of writable memory owned by `info`.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(info).cast(),
            size,
        )
    };
    if got != size {
        return Err(format!("pid {pid}: no such process"));
    }
    Ok(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

/// The start time of `pid`; unsupported off macOS (fail closed).
#[cfg(not(target_os = "macos"))]
pub fn start_time_us(pid: u32) -> Result<u64, String> {
    Err(format!("pid {pid}: start time is read on macOS only"))
}

/// Read `"CFBundleIdentifier"="..."` and `"LSDisplayName"="..."` lines.
///
/// `None` when no bundle id is printed or it is `[ NULL ]` (no such app).
pub fn parse_lsappinfo(text: &str) -> Option<ResolvedApp> {
    let value = |key: &str| {
        text.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(&format!("\"{key}\"="))?;
            let v = rest.strip_prefix('"')?.strip_suffix('"')?;
            (!v.is_empty()).then(|| v.to_owned())
        })
    };
    let bundle_id = value("CFBundleIdentifier")?;
    let name = value("LSDisplayName").unwrap_or_else(|| bundle_id.clone());
    Some(ResolvedApp {
        bundle_id,
        name,
        started_us: 0,
    })
}
