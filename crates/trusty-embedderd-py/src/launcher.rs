//! Launcher discovery + exec into the Python sidecar (slice 3).
//!
//! Why: the trusty-search `EmbedderSupervisor` spawns a *binary* with `--stdio`
//! and piped stdin/stdout (see `trusty-common::embedder_client::supervisor`).
//! This module lets trusty-search find the `trusty-embedderd-py` launcher via
//! the same sibling/PATH/env logic used for `trusty-embedderd`, and — inside
//! the launcher process — replace itself with the venv's
//! `python -m trusty_embed_sidecar --stdio`, inheriting stdio and forwarding
//! `TRUSTY_EMBED_BATCH_SIZE` (env is inherited across exec).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::bootstrap::VenvLayout;

/// Locate the `trusty-embedderd-py` launcher binary.
///
/// Search order mirrors `locate_embedderd_binary`:
///   1. `TRUSTY_EMBEDDERD_PY_BIN` env var (explicit override).
///   2. Sibling of `current_exe()` (workspace/release build).
///   3. `trusty-embedderd-py` on `PATH`.
pub fn locate_launcher_binary() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("TRUSTY_EMBEDDERD_PY_BIN") {
        let p = PathBuf::from(&explicit);
        if p.is_file() {
            return Ok(p);
        }
        bail!("TRUSTY_EMBEDDERD_PY_BIN={explicit:?} does not point to an existing file");
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(bin_name());
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }

    which::which(bin_name()).with_context(|| {
        format!(
            "could not locate the {} launcher. Set TRUSTY_EMBEDDERD_PY_BIN=/path/to/{} \
             or ensure it is on PATH.",
            bin_name(),
            bin_name()
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
