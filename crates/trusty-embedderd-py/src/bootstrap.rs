//! Robust uv/venv bootstrap for the Python/MPS embedding sidecar (slice 4).
//!
//! Why: the sidecar needs torch + sentence-transformers in a reproducible,
//! pinned Python environment that lives OUTSIDE the repo (in the trusty-search
//! data dir) and is shared/keyed by the committed `uv.lock`'s content hash. A
//! first `trusty-search start` with `TRUSTY_EMBEDDER=python` eagerly builds
//! this venv; on ANY failure the caller (trusty-search) falls back to the Rust
//! ort path so search never hard-fails.
//!
//! What: [`ensure_venv`] materializes the embedded Python project, locates
//! `uv`, installs a pinned CPython, creates a venv, and `uv pip sync`s a
//! hashed requirements file exported from the committed `uv.lock`, then runs an
//! import+embed smoke test and writes a `.ready` sentinel (recording the
//! lockfile hash). Robustness: disk-space precheck, bounded timeout + one retry
//! on transient failure, `flock` against concurrent bootstraps, and a
//! double-checked `.ready` fast path.
//!
//! Cross-platform lock note: the committed `uv.lock` is resolved for BOTH
//! macOS-arm64 and linux-x86_64 (see `python/pyproject.toml` `tool.uv.environments`).
//! At bootstrap time `uv export` narrows the lock to the *running* platform and
//! emits a hashed requirements file, which `uv pip sync` installs — so one
//! committed lock bootstraps reproducibly on either host.
//!
//! Test: `bootstrap_tests.rs` covers hash stability, layout derivation, the
//! `.ready` fast path, and disk/space/uv-missing error surfaces (no real
//! torch/venv). The full venv build is an `#[ignore]` e2e test.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use fs4::FileExt;
use include_dir::{include_dir, Dir};
use sha2::{Digest, Sha256};

/// The Python project (pyproject + hashed uv.lock + `trusty_embed_sidecar`
/// package + tests) embedded into the binary and materialized at bootstrap.
static PY_PROJECT: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/python");

/// Pinned CPython version `uv python install` provisions for the venv.
const PINNED_PYTHON: &str = "3.11";

/// Approximate free space required to build the venv (torch is ~2-3 GB).
const REQUIRED_FREE_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// Default bootstrap timeout (each uv step); override via
/// `TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS`.
const DEFAULT_BOOTSTRAP_TIMEOUT_SECS: u64 = 600;

/// Resolved paths for a bootstrapped (or to-be-bootstrapped) venv.
#[derive(Debug, Clone)]
pub struct VenvLayout {
    /// Base dir: `<data>/py-embedder/<lockfile-hash>/`.
    pub base: PathBuf,
    /// The materialized Python project dir (also the `PYTHONPATH` root so
    /// `python -m trusty_embed_sidecar` resolves the bundled package).
    pub project_dir: PathBuf,
    /// The venv directory.
    pub venv_dir: PathBuf,
    /// The venv's python interpreter.
    pub venv_python: PathBuf,
}

impl VenvLayout {
    fn ready_sentinel(&self) -> PathBuf {
        self.base.join(".ready")
    }
    fn lock_file(&self) -> PathBuf {
        self.base.join(".bootstrap.lock")
    }
}

/// Content hash of the embedded `uv.lock` (first 16 hex chars of its SHA-256).
///
/// Why: keys the venv directory so a lock change provisions a fresh venv and
/// the `.ready` sentinel can prove a venv matches the current lock.
pub fn lockfile_hash() -> String {
    let lock = PY_PROJECT
        .get_file("uv.lock")
        .expect("uv.lock must be embedded")
        .contents();
    let mut hasher = Sha256::new();
    hasher.update(lock);
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

/// Compute the venv layout for the current lockfile hash under the trusty-search
/// data dir (honours `TRUSTY_DATA_DIR_OVERRIDE`). Creates no files.
pub fn resolve_layout() -> Result<VenvLayout> {
    let data = trusty_common::resolve_data_dir("trusty-search")
        .context("resolve trusty-search data dir for py-embedder venv")?;
    let base = data.join("py-embedder").join(lockfile_hash());
    let project_dir = base.join("project");
    let venv_dir = base.join("venv");
    let venv_python = if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    };
    Ok(VenvLayout {
        base,
        project_dir,
        venv_dir,
        venv_python,
    })
}

/// Is this layout already fully bootstrapped for the current lock?
fn is_ready(layout: &VenvLayout) -> bool {
    let want = lockfile_hash();
    layout.venv_python.is_file()
        && fs::read_to_string(layout.ready_sentinel())
            .map(|s| s.trim() == want)
            .unwrap_or(false)
}

fn bootstrap_timeout() -> Duration {
    let secs = std::env::var("TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_BOOTSTRAP_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Ensure a ready venv exists and return its layout.
///
/// Fast path (no lock): if `.ready` matches the current lock and the venv
/// python exists, return immediately. Otherwise flock the base dir and build.
/// Idempotent and safe under concurrent callers.
pub fn ensure_venv() -> Result<VenvLayout> {
    let layout = resolve_layout()?;

    // Fast path — already built for this lock.
    if is_ready(&layout) {
        tracing::debug!(venv = %layout.venv_dir.display(), "py-embedder venv already ready");
        return Ok(layout);
    }

    fs::create_dir_all(&layout.base)
        .with_context(|| format!("create venv base dir {}", layout.base.display()))?;

    // Acquire the cross-process bootstrap lock (flock). A concurrent builder
    // in another process (or trusty-search instance) holds this while it works;
    // we block until it releases, then re-check `.ready`.
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(layout.lock_file())
        .with_context(|| format!("open bootstrap lock {}", layout.lock_file().display()))?;
    tracing::info!("py-embedder: acquiring bootstrap lock (flock)…");
    FileExt::lock_exclusive(&lock_file).context("flock the py-embedder bootstrap lock")?;

    // Double-checked: another process may have finished while we waited.
    let result = (|| {
        if is_ready(&layout) {
            tracing::info!("py-embedder: venv became ready while awaiting lock — reusing");
            return Ok(());
        }
        build_venv(&layout)
    })();

    let _ = FileExt::unlock(&lock_file);
    result?;
    Ok(layout)
}

/// The full build (called under the flock, after the ready re-check failed).
fn build_venv(layout: &VenvLayout) -> Result<()> {
    precheck_disk_space(&layout.base)?;
    let uv = locate_uv()?;
    materialize_project(&layout.project_dir)?;

    // Isolate uv's provisioned CPython + cache under the data dir so the venv
    // is fully self-contained and honours TRUSTY_DATA_DIR_OVERRIDE.
    let uv_python_dir = layout.base.join("uv-python");
    let requirements = layout.project_dir.join("requirements.txt");

    tracing::warn!(
        "py-embedder: bootstrapping Python/MPS sidecar venv at {} — this is a \
         one-time ~2-3 GB download (torch + sentence-transformers); set \
         TRUSTY_EMBEDDER back to unset/stdio to use the Rust ort path instead",
        layout.venv_dir.display()
    );

    // 1. Pinned CPython.
    run_uv_step(
        &uv,
        &["python", "install", PINNED_PYTHON],
        &layout.project_dir,
        &uv_python_dir,
    )?;
    // 2. venv.
    run_uv_step(
        &uv,
        &[
            "venv",
            layout.venv_dir.to_str().context("venv path utf8")?,
            "--python",
            PINNED_PYTHON,
        ],
        &layout.project_dir,
        &uv_python_dir,
    )?;
    // 3. Export the committed, hashed uv.lock → a platform-narrowed hashed
    //    requirements file.
    run_uv_step(
        &uv,
        &[
            "export",
            "--frozen",
            "--no-emit-project",
            "--format",
            "requirements-txt",
            "-o",
            requirements.to_str().context("requirements path utf8")?,
        ],
        &layout.project_dir,
        &uv_python_dir,
    )?;
    // 4. uv pip sync into the venv from the hashed requirements.
    run_uv_step(
        &uv,
        &[
            "pip",
            "sync",
            "--python",
            layout.venv_python.to_str().context("venv python utf8")?,
            requirements.to_str().context("requirements path utf8")?,
        ],
        &layout.project_dir,
        &uv_python_dir,
    )?;

    smoke_test(layout)?;

    // Write the sentinel LAST, recording the lock hash. Its presence is the
    // only signal `is_ready` trusts.
    let mut f = fs::File::create(layout.ready_sentinel()).with_context(|| {
        format!(
            "write .ready sentinel {}",
            layout.ready_sentinel().display()
        )
    })?;
    f.write_all(lockfile_hash().as_bytes())?;
    tracing::info!(venv = %layout.venv_dir.display(), "py-embedder: venv bootstrap complete");
    Ok(())
}

/// Precheck: refuse to start a ~3 GB build with insufficient free space.
fn precheck_disk_space(base: &Path) -> Result<()> {
    // fs4::available_space needs an existing path; base is created by caller.
    match fs4::available_space(base) {
        Ok(free) if free < REQUIRED_FREE_BYTES => bail!(
            "insufficient disk space for the Python/MPS sidecar venv: {} free at {}, \
             need ~{} GB (torch + sentence-transformers)",
            human_bytes(free),
            base.display(),
            REQUIRED_FREE_BYTES / (1024 * 1024 * 1024)
        ),
        Ok(free) => {
            tracing::debug!(free = %human_bytes(free), "py-embedder: disk-space precheck ok");
            Ok(())
        }
        Err(e) => {
            // Non-fatal: proceed (a real out-of-space will still fail loudly at pip sync).
            tracing::warn!(
                "py-embedder: disk-space precheck could not stat {}: {e}",
                base.display()
            );
            Ok(())
        }
    }
}

fn human_bytes(n: u64) -> String {
    format!("{:.1} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// Locate `uv`: `TRUSTY_UV_BIN` → PATH. (Vendored/download-with-SHA256 is a
/// slice-5/6 follow-up; until then a missing uv is an actionable bootstrap
/// error that triggers the ort fallback.)
pub fn locate_uv() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("TRUSTY_UV_BIN") {
        let p = PathBuf::from(&explicit);
        if p.is_file() {
            return Ok(p);
        }
        bail!("TRUSTY_UV_BIN={explicit:?} does not point to an existing file");
    }
    which::which("uv").map_err(|_| {
        anyhow!(
            "`uv` not found on PATH and TRUSTY_UV_BIN is unset. Install uv \
             (https://docs.astral.sh/uv/) or set TRUSTY_UV_BIN=/path/to/uv. \
             Until then trusty-search falls back to the Rust ort embedder."
        )
    })
}

/// Materialize the embedded Python project to `dest` (overwrites in place so a
/// partial prior attempt self-heals). Skips the `tests/` dir — not needed at
/// runtime, and it carries a large reference-vectors fixture.
fn materialize_project(dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("create project dir {}", dest.display()))?;
    write_dir(&PY_PROJECT, dest)?;
    Ok(())
}

fn write_dir(dir: &Dir<'_>, dest: &Path) -> Result<()> {
    for file in dir.files() {
        let name = file
            .path()
            .file_name()
            .context("embedded file has no name")?;
        let out = dest.join(name);
        fs::write(&out, file.contents()).with_context(|| format!("write {}", out.display()))?;
    }
    for sub in dir.dirs() {
        let name = sub.path().file_name().context("embedded dir has no name")?;
        // Skip the tests fixture dir at runtime materialization.
        if name == "tests" {
            continue;
        }
        let out = dest.join(name);
        fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;
        write_dir(sub, &out)?;
    }
    Ok(())
}

/// Run one uv subcommand with a bounded timeout and ONE retry on a transient
/// (non-zero exit / spawn) failure — most bootstrap failures are transient
/// network hiccups on the first attempt.
fn run_uv_step(uv: &Path, args: &[&str], cwd: &Path, uv_python_dir: &Path) -> Result<()> {
    let timeout = bootstrap_timeout();
    let mut last_err = None;
    for attempt in 1..=2 {
        match run_with_timeout(uv, args, cwd, uv_python_dir, timeout) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(
                    "py-embedder: uv {} failed (attempt {attempt}/2): {e:#}",
                    args.first().copied().unwrap_or("")
                );
                last_err = Some(e);
                if attempt == 1 {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("uv step failed")))
}

/// Spawn a uv command, poll for completion under a deadline, and kill on timeout.
fn run_with_timeout(
    uv: &Path,
    args: &[&str],
    cwd: &Path,
    uv_python_dir: &Path,
    timeout: Duration,
) -> Result<()> {
    tracing::info!("py-embedder: uv {}", args.join(" "));
    let mut cmd = Command::new(uv);
    cmd.args(args)
        .current_dir(cwd)
        .env("UV_PYTHON_INSTALL_DIR", uv_python_dir)
        // Never let uv reach for a system/managed python outside our pin.
        .env("UV_PYTHON_PREFERENCE", "only-managed")
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn uv {}", args.join(" ")))?;

    let start = Instant::now();
    loop {
        match child.try_wait().context("poll uv child")? {
            Some(status) => {
                if status.success() {
                    return Ok(());
                }
                bail!("uv {} exited with {status}", args.join(" "));
            }
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!(
                        "uv {} timed out after {}s (TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS)",
                        args.join(" "),
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Import + one-embed smoke test in the freshly built venv. Loads torch, the
/// pinned model, and embeds a probe — proving the venv is genuinely usable
/// before `.ready` is written.
fn smoke_test(layout: &VenvLayout) -> Result<()> {
    tracing::info!("py-embedder: running import+embed smoke test…");
    let code = "from trusty_embed_sidecar.model import build_encoder\n\
                v = build_encoder(log=lambda m: print(m))(['trusty smoke test'])\n\
                assert len(v) == 1 and len(v[0]) == 384 and any(v[0]), 'bad embedding'\n\
                print('SMOKE_OK')\n";
    let timeout = bootstrap_timeout();
    let mut cmd = Command::new(&layout.venv_python);
    cmd.args(["-c", code])
        .current_dir(&layout.project_dir)
        .env("PYTHONPATH", &layout.project_dir)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let mut child = cmd.spawn().context("spawn venv python smoke test")?;
    let start = Instant::now();
    loop {
        match child.try_wait().context("poll smoke test")? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => bail!("py-embedder smoke test failed: {status}"),
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!(
                        "py-embedder smoke test timed out after {}s",
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
