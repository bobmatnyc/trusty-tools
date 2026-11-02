//! The python3 the dashboard's LaunchAgent runs (#10606).
//!
//! Why: the dashboard reads TOML with `tomllib`, which needs Python 3.11. The
//! first `python3` found can be macOS's 3.9 at `/usr/bin/python3`, and then the
//! Projects view loses its stack and config rows.
//! What: [`choose`] takes the first candidate at or above [`PYTHON_FLOOR`] and
//! falls back to the first candidate when none qualifies. The dashboard then
//! reports the old Python itself, on stderr, in `/config.json` and on the page.
//! Test: `the_dashboard_prefers_a_python_at_or_above_the_floor`,
//! `the_dashboard_falls_back_to_the_first_python_below_the_floor`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The lowest Python the dashboard runs at full function (`tomllib`).
pub(crate) const PYTHON_FLOOR: (u32, u32) = (3, 11);

/// How long one `python3 -c` version probe may run.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The chosen interpreter and the version its probe reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Python {
    pub(crate) path: PathBuf,
    /// `(major, minor)`, or `None` when the probe failed.
    pub(crate) version: Option<(u32, u32)>,
}

impl Python {
    /// True when the version is unknown or below [`PYTHON_FLOOR`].
    pub(crate) fn below_floor(&self) -> bool {
        self.version.is_none_or(|v| v < PYTHON_FLOOR)
    }
}

/// Every runnable `python3`, in lookup order: the absolute `PATH` entries,
/// then `fallback_dirs`. A path seen twice is listed once.
///
/// Why: the same order `resolve_binary("python3")` used, so a host whose first
/// `python3` is already >= 3.11 gets the same interpreter as before.
/// What: relative `PATH` entries are skipped, as in `resolve_binary`.
/// Test: `the_dashboard_prefers_a_python_at_or_above_the_floor`.
pub(crate) fn candidates(path_var: Option<&OsStr>, fallback_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let from_path = path_var
        .map(|v| std::env::split_paths(v).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in from_path
        .iter()
        .filter(|d| d.is_absolute())
        .chain(fallback_dirs)
    {
        let p = dir.join("python3");
        if runnable(&p) && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// The first candidate at or above [`PYTHON_FLOOR`], else the first candidate.
///
/// Why: prefer a Python that runs the dashboard fully, and still start it on a
/// host that has only an older one (the dashboard then warns).
/// What: probes in order and stops at the first one at or above the floor.
/// `None` only when `candidates` is empty.
/// Test: `the_dashboard_prefers_a_python_at_or_above_the_floor`,
/// `the_dashboard_falls_back_to_the_first_python_below_the_floor`.
pub(crate) fn choose(
    candidates: &[PathBuf],
    version_of: &dyn Fn(&Path) -> Option<(u32, u32)>,
) -> Option<Python> {
    let mut first: Option<Python> = None;
    for path in candidates {
        let found = Python {
            path: path.clone(),
            version: version_of(path),
        };
        if !found.below_floor() {
            return Some(found);
        }
        first.get_or_insert(found);
    }
    first
}

/// `(major, minor)` that `python -I -c` prints, or `None` when it fails, exits
/// non-zero, prints something else, or runs past [`PROBE_TIMEOUT`].
pub(crate) fn version(python: &Path) -> Option<(u32, u32)> {
    let mut child = Command::new(python)
        .args([
            "-I",
            "-c",
            "import sys; print('%d.%d' % sys.version_info[:2])",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (major, minor) = text.trim().split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The interpreter for the plist: [`choose`] over the live `PATH` and the
/// daemon dirs, else `/usr/bin/python3` (the earlier fallback).
#[cfg(target_os = "macos")]
pub(crate) fn dashboard_python() -> Python {
    let cands = candidates(
        std::env::var_os("PATH").as_deref(),
        &trusty_common::bin_resolve::daemon_path_dirs(),
    );
    choose(&cands, &version).unwrap_or_else(|| Python {
        path: PathBuf::from("/usr/bin/python3"),
        version: None,
    })
}

/// An existing file with an execute bit (any file off Unix).
fn runnable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}
