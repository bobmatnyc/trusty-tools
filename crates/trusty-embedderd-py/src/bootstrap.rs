//! Robust uv/venv bootstrap for the Python/MPS embedding sidecar (slice 4).
//!
//! Why: the sidecar needs torch + sentence-transformers in a reproducible,
//! pinned Python environment that lives OUTSIDE the repo (in the trusty-search
//! data dir) and is shared/keyed by the committed `uv.lock`'s content hash. A
//! first `trusty-search start` with `TRUSTY_EMBEDDER=python` eagerly builds
//! this venv; on ANY failure the caller (trusty-search) falls back to the Rust
//! ort path so search never hard-fails.
//!
//! What: [`ensure_venv`] / [`ensure_venv_eager`] materialize the embedded
//! Python project, locate `uv`, install a pinned CPython, create a venv, and
//! `uv pip sync` a hashed requirements file exported from the committed
//! `uv.lock`, then run an import+embed smoke test and write a `.ready`
//! sentinel (recording the lockfile hash). Robustness: disk-space precheck,
//! bounded timeout + one retry on transient failure, `flock` against
//! concurrent bootstraps, and a double-checked `.ready` fast path.
//!
//! Two-tier `.ready` recheck (fast-follow): `.ready` is trusted forever by
//! default, so a post-build corruption (broken native `.so`, an ABI shift, a
//! half-deleted directory) would otherwise route real traffic to a broken
//! interpreter. [`ensure_venv`] — called by the `trusty-embedderd-py` launcher
//! binary on EVERY respawn — rechecks with the CHEAP, torch-free
//! [`verify_venv_alive`] (interpreter liveness + an installed-package marker
//! file, no `import sentence_transformers`) so a respawn never re-pays torch's
//! import cost. [`ensure_venv_eager`] — called ONCE by trusty-search's daemon
//! at `start` — rechecks with the FULL [`verify_full_import_smoke`] (a real
//! `import sentence_transformers`) since that cost is paid only once per
//! daemon lifetime. See `verify_venv_alive`'s doc comment for the accepted
//! trade-off.
//!
//! Cross-platform lock note: the committed `uv.lock` is resolved for BOTH
//! macOS-arm64 and linux-x86_64 (see `python/pyproject.toml` `tool.uv.environments`).
//! At bootstrap time `uv export` narrows the lock to the *running* platform and
//! emits a hashed requirements file, which `uv pip sync` installs — so one
//! committed lock bootstraps reproducibly on either host.
//!
//! Test: `bootstrap_tests.rs` covers hash stability, layout derivation, both
//! `.ready` fast paths (cheap and full recheck), and disk/space/uv-missing
//! error surfaces (no real torch/venv). The full venv build is an `#[ignore]`
//! e2e test.

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

/// Bound on the FULL import recheck (see [`verify_full_import_smoke`]) — the
/// eager, once-per-daemon-start path only.
const FULL_RECHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on the cheap, torch-free liveness recheck (see [`verify_venv_alive`])
/// — the per-respawn hot-path check. Shorter than [`FULL_RECHECK_TIMEOUT`]
/// since it only waits on interpreter startup, never an import of torch.
const LIVENESS_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Ensure a ready venv exists and return its layout — the PER-RESPAWN path.
///
/// Called by the `trusty-embedderd-py` launcher binary on EVERY spawn
/// (initial lazy spawn, crash-restart, post-idle-shutdown re-spawn) — a
/// short-lived process that `exec`s into python immediately after this
/// returns. Fast path (no lock): if `.ready` matches the current lock, the
/// venv python exists, AND the venv passes the CHEAP, torch-free
/// [`verify_venv_alive`] liveness check, return immediately. Otherwise flock
/// the base dir and (re)build. Idempotent and safe under concurrent callers.
///
/// See [`ensure_venv_eager`] for the once-per-daemon-start variant that pays
/// for a full `import sentence_transformers` recheck instead — deliberately
/// NOT done here; see [`verify_venv_alive`]'s doc comment for why.
pub fn ensure_venv() -> Result<VenvLayout> {
    ensure_venv_checked(RecheckDepth::Liveness)
}

/// Ensure a ready venv exists and return its layout — the EAGER,
/// once-per-daemon-start path.
///
/// Called exactly once by trusty-search's long-lived daemon process at
/// `start` (`commands/start/embedder.rs`'s `TRUSTY_EMBEDDER=python` arm).
/// Identical to [`ensure_venv`] except the `.ready` fast-path recheck is the
/// FULL [`verify_full_import_smoke`] (imports `sentence_transformers`, i.e.
/// torch) rather than the cheap liveness check — worth the one-time cost here
/// because it runs once per daemon lifetime, not once per respawn, and
/// catches a torch-internal corruption (bad `.so`, ABI shift) that the cheap
/// check cannot.
pub fn ensure_venv_eager() -> Result<VenvLayout> {
    ensure_venv_checked(RecheckDepth::FullImport)
}

/// How thoroughly to recheck an already-`.ready` venv before trusting it —
/// see [`ensure_venv`] vs [`ensure_venv_eager`].
#[derive(Clone, Copy)]
enum RecheckDepth {
    /// Cheap, torch-free interpreter-liveness + marker-file check
    /// ([`verify_venv_alive`]). Used on every respawn.
    Liveness,
    /// Full `import sentence_transformers` (imports torch)
    /// ([`verify_full_import_smoke`]). Used once, at eager daemon-start.
    FullImport,
}

impl RecheckDepth {
    fn verify(self, layout: &VenvLayout) -> bool {
        match self {
            RecheckDepth::Liveness => verify_venv_alive(layout),
            RecheckDepth::FullImport => verify_full_import_smoke(layout),
        }
    }
}

fn ensure_venv_checked(depth: RecheckDepth) -> Result<VenvLayout> {
    let layout = resolve_layout()?;

    // Fast path — already built for this lock.
    //
    // Why the extra recheck beyond `is_ready`: `.ready` is a sentinel written
    // once, at the end of a successful build, and trusted forever after. If
    // the venv is corrupted *after* that (a broken native `.so`, an ABI shift
    // from an OS/Xcode upgrade, a half-deleted directory) the stale sentinel
    // would keep routing real embed traffic to a broken interpreter instead
    // of falling back to the Rust ort path. `depth` controls how thoroughly
    // that recheck looks (see `RecheckDepth`).
    if is_ready(&layout) {
        if depth.verify(&layout) {
            tracing::debug!(venv = %layout.venv_dir.display(), "py-embedder venv already ready");
            return Ok(layout);
        }
        tracing::warn!(
            venv = %layout.venv_dir.display(),
            "py-embedder: `.ready` sentinel present but the venv failed its \
             recheck (possibly corrupted) — rebuilding"
        );
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
        if is_ready(&layout) && depth.verify(&layout) {
            tracing::info!("py-embedder: venv became ready while awaiting lock — reusing");
            return Ok(());
        }
        build_venv(&layout)
    })();

    let _ = FileExt::unlock(&lock_file);
    result?;
    Ok(layout)
}

/// The venv's site-packages directory (standard `<venv>/lib/pythonX.Y/site-packages`
/// layout on macOS/Linux, `<venv>\Lib\site-packages` on Windows).
fn site_packages_dir(layout: &VenvLayout) -> PathBuf {
    if cfg!(windows) {
        layout.venv_dir.join("Lib").join("site-packages")
    } else {
        layout
            .venv_dir
            .join("lib")
            .join(format!("python{PINNED_PYTHON}"))
            .join("site-packages")
    }
}

/// Cheap, torch-free liveness check for the per-respawn hot path.
///
/// Why (fast-follow: the prior version of this recheck ran
/// `import sentence_transformers`, which transitively imports torch — on
/// EVERY respawn via `ensure_venv`, called by the short-lived launcher binary
/// each time the supervisor spawns it. That duplicated a large slice of the
/// sidecar's cold-start cost (torch's own import is the dominant chunk of
/// `t_import` in `model.py`'s startup log) on every single respawn and
/// undercut the whole point of a longer idle-shutdown window
/// (`TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS`, default 1800s): a "cheap" cold
/// restart that pays torch's import cost twice (once here, once for real in
/// `build_encoder`). This check instead only proves (1) the installed-package
/// marker is present on disk — catches a half-deleted venv — and (2) the
/// interpreter itself starts and runs cleanly — catches a broken interpreter
/// binary or a missing/incompatible shared library severe enough to crash
/// python at startup — WITHOUT ever importing torch.
///
/// Accepted trade-off: this cannot detect a torch-internal-only breakage that
/// leaves the bare interpreter working (e.g. a corrupted torch `.so` from an
/// ABI shift, with `python -c "import sys"` still exiting 0). That deeper
/// check still runs once per daemon lifetime via [`verify_full_import_smoke`]
/// (see [`ensure_venv_eager`]); a respawn that hits this residual gap still
/// fails safely one request later — `build_encoder`'s non-zero-exit-on-
/// failure contract (`__main__.py`) surfaces it as a failed startup probe,
/// which the supervisor's crash-restart/backoff already handles.
///
/// What: (1) `<venv>/lib/pythonX.Y/site-packages/sentence_transformers` must
/// exist as a directory (a cheap `Path::is_dir`, no process spawn); (2)
/// `<venv>/bin/python -c "import sys; sys.exit(0)"` must exit 0 within
/// [`LIVENESS_CHECK_TIMEOUT`] — proves the interpreter starts and runs
/// without touching torch.
/// Test: `verify_venv_alive_*` in `bootstrap_tests.rs`.
fn verify_venv_alive(layout: &VenvLayout) -> bool {
    if !site_packages_dir(layout)
        .join("sentence_transformers")
        .is_dir()
    {
        tracing::warn!("py-embedder: liveness-recheck: sentence_transformers marker missing from site-packages");
        return false;
    }
    run_bounded_python_check(
        &layout.venv_python,
        &["-c", "import sys; sys.exit(0)"],
        LIVENESS_CHECK_TIMEOUT,
        "liveness-recheck",
    )
}

/// Full import recheck of an already-`.ready` venv — the eager,
/// once-per-daemon-start path only (see [`ensure_venv_eager`]).
///
/// What: runs `<venv>/bin/python -c "import sentence_transformers"` — an
/// import only, NOT a full embed (no model download/load, no torch device
/// init) — bounded by [`FULL_RECHECK_TIMEOUT`]. Returns `true` only on a
/// clean, successful exit; any spawn failure, non-zero exit, or timeout
/// returns `false` so the caller treats the venv as not-ready and rebuilds
/// (and if the rebuild itself fails, that error propagates so
/// `commands/start/embedder.rs`'s fall-back-to-ort path fires — the venv is
/// never trusted on a failed recheck).
///
/// Deliberately does NOT set `current_dir`/`PYTHONPATH` to `project_dir` (unlike
/// [`smoke_test`], which imports our own bundled `trusty_embed_sidecar`
/// package): this only imports the venv's own installed `sentence_transformers`
/// from site-packages, so it stays correct even if `project_dir` were ever
/// missing (e.g. manually deleted) while the venv itself is intact.
fn verify_full_import_smoke(layout: &VenvLayout) -> bool {
    run_bounded_python_check(
        &layout.venv_python,
        &["-c", "import sentence_transformers"],
        FULL_RECHECK_TIMEOUT,
        "full-import-recheck",
    )
}

/// Shared bounded spawn-and-poll helper for [`verify_venv_alive`] and
/// [`verify_full_import_smoke`]: run `python <args>` to completion, killing it
/// (and returning `false`) if it outlives `timeout`. `label` only tags the
/// warn-log lines so the two call sites stay distinguishable in output.
fn run_bounded_python_check(python: &Path, args: &[&str], timeout: Duration, label: &str) -> bool {
    let mut cmd = Command::new(python);
    cmd.args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("py-embedder: {label} failed to spawn venv python: {e}");
            return false;
        }
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(
                        "py-embedder: {label} timed out after {}s",
                        timeout.as_secs()
                    );
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!("py-embedder: {label} poll failed: {e}");
                return false;
            }
        }
    }
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
