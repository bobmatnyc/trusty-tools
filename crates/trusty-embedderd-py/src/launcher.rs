//! Launcher discovery + exec into the Python sidecar (slice 3).
//!
//! Why: the trusty-search `EmbedderSupervisor` spawns a *binary* with `--stdio`
//! and piped stdin/stdout (see `trusty-common::embedder_client::supervisor`).
//! This module lets trusty-search find the `trusty-embedderd-py` launcher via
//! the same sibling/PATH/env logic used for `trusty-embedderd`, and — inside
//! the launcher process — replace itself with the venv's
//! `python -m trusty_embed_sidecar --stdio`, inheriting stdio and forwarding
//! `TRUSTY_EMBED_BATCH_SIZE` (env is inherited across exec).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::bootstrap::VenvLayout;

/// Locate the `trusty-embedderd-py` launcher binary.
///
/// Search order mirrors `locate_embedderd_binary`:
///   1. `TRUSTY_EMBEDDERD_PY_BIN` env var (explicit override).
///   2. Sibling of `current_exe()` — the copy `cargo install trusty-search`
///      installs next to the daemon (#9468).
///   3. `trusty-embedderd-py` on `PATH`.
///
/// Test: `sibling_launcher_wins_over_path`, `path_launcher_found_without_sibling`,
/// `missing_launcher_error_names_sibling_dir_and_install`.
pub fn locate_launcher_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().ok();
    locate_launcher_in(
        std::env::var_os("TRUSTY_EMBEDDERD_PY_BIN"),
        exe.as_deref().and_then(Path::parent),
        std::env::var_os("PATH"),
    )
}

/// [`locate_launcher_binary`] with its three inputs passed in, so the order is
/// testable without mutating process-global env.
fn locate_launcher_in(
    explicit: Option<OsString>,
    exe_dir: Option<&Path>,
    path_var: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(explicit) = explicit {
        let p = PathBuf::from(&explicit);
        if p.is_file() {
            return Ok(p);
        }
        bail!("TRUSTY_EMBEDDERD_PY_BIN={explicit:?} does not point to an existing file");
    }

    if let Some(dir) = exe_dir {
        let sibling = dir.join(bin_name());
        if sibling.is_file() {
            return Ok(sibling);
        }
    }

    let cwd = std::env::current_dir().context("read the current directory")?;
    which::which_in(bin_name(), path_var, cwd).with_context(|| {
        // #9468: trusty-search ships the launcher; name where it was expected.
        let beside = exe_dir.map_or_else(|| "<unknown>".to_owned(), |d| d.display().to_string());
        format!(
            "could not locate the {bin} launcher: not beside the running binary in \
             {beside} and not on PATH. `cargo install trusty-search` installs it \
             there; or set TRUSTY_EMBEDDERD_PY_BIN=/path/to/{bin}.",
            bin = bin_name(),
        )
    })
}

fn bin_name() -> &'static str {
    if cfg!(windows) {
        "trusty-embedderd-py.exe"
    } else {
        "trusty-embedderd-py"
    }
}

/// Replace this process with the venv's `python -m trusty_embed_sidecar`,
/// forwarding `args` (notably `--stdio`) and setting `PYTHONPATH` so the
/// bundled package resolves. On Unix this `exec`s (no extra process in the
/// tree, stdio inherited); elsewhere it spawns + waits and propagates the code.
pub fn exec_sidecar(layout: &VenvLayout, args: &[String]) -> Result<()> {
    let mut cmd = Command::new(&layout.venv_python);
    cmd.arg("-m")
        .arg("trusty_embed_sidecar")
        .args(args)
        .env("PYTHONPATH", &layout.project_dir);

    exec_or_wait(cmd, &layout.venv_python)
}

#[cfg(unix)]
fn exec_or_wait(mut cmd: Command, python: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    // `exec` only returns on failure.
    let err = cmd.exec();
    Err(err).with_context(|| format!("exec venv python {}", python.display()))
}

#[cfg(not(unix))]
fn exec_or_wait(mut cmd: Command, python: &Path) -> Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("spawn venv python {}", python.display()))?;
    if !status.success() {
        bail!("venv python exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An executable stand-in for the launcher in `dir` (`which` needs +x).
    fn fake_launcher(dir: &Path) -> PathBuf {
        let p = dir.join(bin_name());
        std::fs::write(&p, b"#!/bin/sh\n").expect("write fake launcher");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        p
    }

    /// #9468: the copy installed beside trusty-search beats a stale one on PATH.
    #[test]
    fn sibling_launcher_wins_over_path() {
        let (exe_dir, path_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let sibling = fake_launcher(exe_dir.path());
        fake_launcher(path_dir.path());
        let found =
            locate_launcher_in(None, Some(exe_dir.path()), Some(path_dir.path().into())).unwrap();
        assert_eq!(found, sibling);
    }

    #[test]
    fn path_launcher_found_without_sibling() {
        let (exe_dir, path_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let on_path = fake_launcher(path_dir.path());
        let found =
            locate_launcher_in(None, Some(exe_dir.path()), Some(path_dir.path().into())).unwrap();
        assert_eq!(found, on_path);
    }

    #[test]
    fn missing_launcher_error_names_sibling_dir_and_install() {
        let (exe_dir, path_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let err = locate_launcher_in(None, Some(exe_dir.path()), Some(path_dir.path().into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains(&exe_dir.path().display().to_string()), "{err}");
        assert!(err.contains("cargo install trusty-search"), "{err}");
    }
}
