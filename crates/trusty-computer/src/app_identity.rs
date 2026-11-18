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
//! Test: `grant_request_refuses_a_pid_of_another_app`,
//! `sensitive_check_reads_the_resolved_bundle`, `lsappinfo_output_parses`.

use std::process::{Command, Stdio};

/// The app LaunchServices reports for a pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedApp {
    /// `CFBundleIdentifier`.
    pub bundle_id: String,
    /// `LSDisplayName`; the bundle id when the app has none.
    pub name: String,
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
    }
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
    Some(ResolvedApp { bundle_id, name })
}
