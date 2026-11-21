//! The bounded-mode capability manifest for one lease scope.
//!
//! Why: cua-driver 0.23.2 admits a bounded app only by exact pid and window
//! (`desktop.applications` / `desktop.windows`); `executable` and `bundle_id`
//! entries are refused (GO-STOP.md:140-141). So the manifest is rendered per
//! lease scope, and a scope change means a daemon restart (plan section 3).
//! What: [`LeaseScope`] is one pid plus one window id; [`render`] emits the
//! version-3 YAML with the tool ceiling of `tests/fixtures/capabilities.live.yaml`
//! and no `resources.apps` entry; [`write`] stores it 0600 without following a
//! symlink.
//! Test: `manifest_pins_exact_pid_and_window_only`, `manifest_file_is_private`.

use std::io::{self, Write};
use std::path::Path;

use crate::private;

/// The daemon-side tool ceiling; the broker's policy narrows it further.
pub const TOOLS: [&str; 11] = [
    "start_session",
    "end_session",
    "list_apps",
    "list_windows",
    "get_window_state",
    "verify_state",
    "click",
    "type_text",
    "set_value",
    "press_key",
    "check_permissions",
];

/// The one app window a lease may touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseScope {
    pub pid: u32,
    pub window_id: u64,
}

/// Manifest lifetimes, in cua duration syntax (`2h`, `15m`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifetimes {
    pub expires_after: String,
    pub idle_timeout: String,
}

impl Default for Lifetimes {
    fn default() -> Lifetimes {
        Lifetimes {
            expires_after: "2h".into(),
            idle_timeout: "15m".into(),
        }
    }
}

/// The manifest YAML for `scope`.
pub fn render(scope: LeaseScope, life: &Lifetimes) -> String {
    let tools: String = TOOLS.iter().map(|t| format!("    - {t}\n")).collect();
    format!(
        "# trusty-computer lease manifest (issue #11460); exact pid and window only.\n\
         version: 3\n\
         expires_after: {}\n\
         idle_timeout: {}\n\
         allow:\n  tools:\n{tools}\
         resources:\n  desktop:\n    applications: [{pid}]\n    windows:\n\
         \x20     - pid: {pid}\n        window_id: {wid}\n    display: false\n",
        life.expires_after,
        life.idle_timeout,
        pid = scope.pid,
        wid = scope.window_id,
    )
}

/// Write `render(scope, life)` to `path` (0600, no symlink follow, truncated).
pub fn write(path: &Path, scope: LeaseScope, life: &Lifetimes) -> io::Result<()> {
    let mut file = private::open_nofollow(path, false)?;
    if !private::private_file(&file.metadata()?) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "manifest must be an owner-only regular file",
        ));
    }
    file.set_len(0)?;
    file.write_all(render(scope, life).as_bytes())?;
    file.sync_all()
}
