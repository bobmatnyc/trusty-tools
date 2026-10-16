//! Locate `uv`, or fetch a pinned, SHA-256-verified uv when none is installed
//! (#9468, D1a; offline half B1, network half #9966).
//!
//! Why: owner ruling D1a — use an installed uv, else a pinned, verified one.
//! What: [`locate_or_fetch_uv`] orders `TRUSTY_UV_BIN` → installed uv → the
//! verified cache at `<data>/py-embedder/uv/<UV_VERSION>/uv` → an HTTPS
//! download checked against [`PINNED_DIGESTS`] before anything is extracted.
//! `TRUSTY_UV_FETCH=0` turns the download off.
//! Test: `uv_fetch_tests.rs`; `locate_uv_*` in `bootstrap_tests.rs`.

mod http;

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use fs4::FileExt;
use trusty_common::integrity::{IntegrityError, Sha256Digest};

/// The pinned uv release (#9468, delivery plan of 2026-10-08).
pub const UV_VERSION: &str = "0.12.23";

/// Env var that turns the download off when set to `0`/`false`/`off`/`no`.
pub const UV_FETCH_ENV: &str = "TRUSTY_UV_FETCH";

/// Env var naming an explicit `uv` binary; always wins over everything else.
pub const UV_BIN_ENV: &str = "TRUSTY_UV_BIN";

/// Largest uv archive read from disk or the network (#9966); the real ones are
/// ~17-20 MB. A bigger one is refused before it is buffered.
pub const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

/// Name prefix of an install's temp file, `.uv.tmp.<pid>`.
const TEMP_PREFIX: &str = ".uv.tmp.";

/// SHA-256 of `uv-<triple>.tar.gz` for [`UV_VERSION`] — the only trust root.
/// Ported from PR #9473, checked there against Astral's `.sha256` sidecars.
#[rustfmt::skip]
const PINNED_DIGESTS: &[(&str, &str)] = &[
    ("aarch64-apple-darwin", "50487ae565ccd96e499056b4674d438f4c53170202617b4c759defe0c6a1b544"),
    ("x86_64-unknown-linux-gnu", "9167d72b3319674b6303c4cbe071854bba13ebdf3d76b1a7cbdc175471fb66d6"),
];

/// Why `uv` could not be resolved, fetched or cached; never a fall-through.
/// Test: `digest_mismatch_places_nothing_and_names_both_digests`,
/// `disabled_fetch_names_both_variables_and_never_fetches`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UvError {
    #[error("TRUSTY_UV_BIN={value:?} does not point to an existing file")]
    BadOverride { value: String },
    #[error(
        "`uv` not found on PATH or in the well-known install dirs (/opt/homebrew/bin, \
         /usr/local/bin, ~/.local/bin, ~/.cargo/bin), TRUSTY_UV_BIN is unset, no verified \
         uv is cached at {}, and TRUSTY_UV_FETCH=0 turns off fetching the pinned uv \
         {UV_VERSION}. Install uv (https://docs.astral.sh/uv/), set TRUSTY_UV_BIN=/path/to/uv, \
         or unset TRUSTY_UV_FETCH. Until then trusty-search uses the Rust ort embedder.",
        cache.display()
    )]
    Disabled { cache: PathBuf },
    #[error(
        "`uv` not found and no pinned uv {UV_VERSION} exists for host {triple}. Install uv \
         (https://docs.astral.sh/uv/) or set TRUSTY_UV_BIN=/path/to/uv."
    )]
    UnsupportedHost { triple: String },
    #[error(
        "`uv` not found and fetching the pinned uv {UV_VERSION} from {url} failed: {reason}. \
         Install uv (https://docs.astral.sh/uv/) or set TRUSTY_UV_BIN=/path/to/uv; \
         TRUSTY_UV_FETCH=0 turns the fetch off. Until then trusty-search uses the Rust ort \
         embedder."
    )]
    Network { url: String, reason: String },
    #[error(
        "cached uv {} is not usable and was not run: {reason}. Remove it, or leave \
         TRUSTY_UV_FETCH unset so a verified download replaces it.",
        path.display()
    )]
    CacheRejected { path: PathBuf, reason: String },
    #[error("uv archive {origin} failed its checksum; nothing extracted: {source}")]
    Checksum {
        origin: String,
        source: IntegrityError,
    },
    #[error("uv archive {origin} is unusable: {reason}")]
    Archive { origin: String, reason: String },
    #[error("{action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

/// Downloads one URL into memory. Injected so tests count calls and serve bytes.
pub(crate) type Fetcher<'a> = &'a (dyn Fn(&str) -> Result<Vec<u8>, UvError> + Sync);

/// Resolve `uv` for the venv build from the real environment (#9966).
///
/// Why: `build_venv` must bootstrap a host with no uv (D1a), and
/// `TRUSTY_UV_FETCH=0` is the one opt-out (Architect, 2026-10-16 Q5).
/// What: [`resolve_uv`] over `TRUSTY_UV_BIN`, the shared `resolve_binary`
/// lookup (#4125: launchd's PATH omits Homebrew) and the HTTPS fetcher,
/// which is `None` when [`fetch_enabled`] says the variable turned it off.
/// Test: `a_build_with_no_uv_and_the_fetch_off_names_both_variables` (bootstrap).
pub fn locate_or_fetch_uv(
    py_root: &dyn Fn() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<PathBuf> {
    let explicit = std::env::var(UV_BIN_ENV).ok();
    let installed = || trusty_common::bin_resolve::resolve_binary("uv");
    let enabled = fetch_enabled(std::env::var(UV_FETCH_ENV).ok().as_deref());
    let download: Fetcher<'_> = &http::http_fetch;
    resolve_uv(
        explicit.as_deref(),
        &installed,
        py_root,
        enabled.then_some(download),
    )
}

/// The resolution order behind [`locate_or_fetch_uv`], every source injected.
///
/// Why: an installed uv always wins; a bad override is an error, never a
/// fall-through to something less trusted.
/// What: `explicit` → `installed()` → [`cached_uv`] under `py_root()` → a
/// fetch for this host. A refused cache (#9966, 4c) is an error when `fetch`
/// is `None` and is replaced by a verified download otherwise; it never runs.
/// Test: `resolve_uv_prefers_explicit_override_and_never_fetches`,
/// `resolve_uv_prefers_path_then_the_verified_cache`,
/// `disabled_fetch_names_both_variables_and_never_fetches`,
/// `a_refused_cache_is_refetched_with_the_fetch_on_and_refused_with_it_off`.
pub(crate) fn resolve_uv(
    explicit: Option<&str>,
    installed: &dyn Fn() -> Option<PathBuf>,
    py_root: &dyn Fn() -> anyhow::Result<PathBuf>,
    fetch: Option<Fetcher<'_>>,
) -> anyhow::Result<PathBuf> {
    if let Some(value) = explicit {
        let path = PathBuf::from(value);
        if !path.is_file() {
            let value = value.to_owned();
            return Err(UvError::BadOverride { value }.into());
        }
        return Ok(path);
    }
    if let Some(found) = installed() {
        return Ok(found);
    }
    let root = py_root()?;
    match (cached_uv(&root), fetch) {
        (Ok(Some(hit)), _) => return Ok(hit),
        (Ok(None), _) => {}
        (Err(refused), None) => return Err(refused.into()),
        // #9966 (4c): the embedded archive digest, not the cache, is the trust
        // root, so a verified re-download restores exactly the trusted state.
        (Err(refused), Some(_)) => tracing::warn!("py-embedder: {refused}; fetching a replacement"),
    }
    let Some(fetch) = fetch else {
        return Err(UvError::Disabled {
            cache: cache_path(&root),
        }
        .into());
    };
    Ok(fetch_for_triple(&root, &host_triple(), fetch)?)
}

/// Is the download on for this `TRUSTY_UV_FETCH` value? Unset means yes.
/// Test: `fetch_enabled_parses_the_off_switch`.
pub fn fetch_enabled(value: Option<&str>) -> bool {
    let Some(value) = value else { return true };
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// `<py_root>/uv/<UV_VERSION>/uv`, where `py_root` is `<data>/py-embedder`.
pub fn cache_path(py_root: &Path) -> PathBuf {
    py_root.join("uv").join(UV_VERSION).join("uv")
}

/// The embedded archive digest for `triple`; `None` for an unsupported host.
pub fn pinned_digest(triple: &str) -> Option<Sha256Digest> {
    let (_, hex) = PINNED_DIGESTS.iter().find(|(t, _)| *t == triple)?;
    Sha256Digest::parse_hex(hex).ok()
}

/// This host's target triple in Astral's asset naming.
pub fn host_triple() -> String {
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin".to_owned()
    } else if cfg!(all(
        target_arch = "x86_64",
        target_os = "linux",
        target_env = "gnu"
    )) {
        "x86_64-unknown-linux-gnu".to_owned()
    } else {
        format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
    }
}

/// The release asset URL for `triple`.
pub fn asset_url(triple: &str) -> String {
    format!("https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/uv-{triple}.tar.gz")
}

/// Fetch `triple`'s asset against its pinned digest; an unknown host fetches
/// nothing. Test: `an_unsupported_host_never_fetches`.
pub(crate) fn fetch_for_triple(
    py_root: &Path,
    triple: &str,
    fetch: Fetcher<'_>,
) -> Result<PathBuf, UvError> {
    let expected = pinned_digest(triple).ok_or_else(|| UvError::UnsupportedHost {
        triple: triple.to_owned(),
    })?;
    fetch_verified(py_root, &asset_url(triple), &expected, fetch)
}

/// Download `url`, verify it against `expected`, and place its `uv` (#9966).
///
/// Why: the cache must only ever hold a complete, verified binary, and racing
/// bootstraps must download it once.
/// What: under `.fetch.lock`, re-checks [`cached_uv`] first (#9966, 4b) and
/// returns a hit another fetcher placed while this one waited; a refused
/// cache counts as a miss and is replaced. Otherwise fetches, verifies, and
/// places as [`install_verified`] does. Any failure places nothing.
/// Test: `a_fetch_that_waited_on_the_lock_reuses_the_cache`,
/// `a_refused_cache_is_replaced_under_the_lock`,
/// `a_network_error_or_bad_digest_places_nothing`.
pub(crate) fn fetch_verified(
    py_root: &Path,
    url: &str,
    expected: &Sha256Digest,
    fetch: Fetcher<'_>,
) -> Result<PathBuf, UvError> {
    with_fetch_lock(py_root, |dir, dest| {
        if let Ok(Some(hit)) = cached_uv(py_root) {
            return Ok(hit);
        }
        tracing::warn!("py-embedder: `uv` not found — fetching pinned uv {UV_VERSION} from {url}");
        let bytes = fetch(url)?;
        let uv = verified_uv(&bytes, expected, url)?;
        place(dir, dest, &uv)
    })
}

/// The verified cached uv under `py_root`, if an install completed (#9468).
///
/// Why: a hit must prove the file is the one an install verified (#9473
/// review finding "cache re-verify"), not merely that it exists.
/// What: `Ok(None)` when the binary or its `uv.sha256` is absent. A symlink,
/// a non-file, a torn `uv.sha256`, or bytes that no longer match it is
/// `CacheRejected`.
/// Test: `a_tampered_cached_uv_is_refused_not_run`.
pub fn cached_uv(py_root: &Path) -> Result<Option<PathBuf>, UvError> {
    let path = cache_path(py_root);
    let reject = |reason: String| UvError::CacheRejected {
        path: path.clone(),
        reason,
    };
    let read = (
        fs::symlink_metadata(&path),
        fs::read_to_string(path.with_file_name("uv.sha256")),
    );
    let (meta, recorded) = match read {
        (Ok(meta), Ok(recorded)) => (meta, recorded),
        (Err(e), _) | (_, Err(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        (Err(e), _) | (_, Err(e)) => return Err(reject(format!("cannot read it: {e}"))),
    };
    if !meta.file_type().is_file() {
        return Err(reject("not a regular file".to_owned()));
    }
    Sha256Digest::from_sidecar(&recorded)
        .and_then(|want| Sha256Digest::of_file(&path)?.verify(&want))
        .map_err(|e| reject(e.to_string()))?;
    Ok(Some(path))
}

/// Check the on-disk `archive` against `expected` (from [`pinned_digest`]),
/// then place its `uv` at [`cache_path`].
///
/// Why: the cache must only ever hold a complete, verified binary.
/// What: an archive over [`MAX_ARCHIVE_BYTES`] is refused before it is read
/// (#9966, 4a). A digest mismatch returns `Checksum` (naming both digests)
/// and creates nothing. Then [`place`] runs under `.fetch.lock`.
/// Test: `digest_match_installs_an_executable_uv`,
/// `digest_mismatch_places_nothing_and_names_both_digests`,
/// `an_oversized_archive_is_refused_before_it_is_read`,
/// `unsafe_archive_entries_are_refused`,
/// `concurrent_fetches_download_once_and_place_a_whole_file`.
pub fn install_verified(
    py_root: &Path,
    archive: &Path,
    expected: &Sha256Digest,
) -> Result<PathBuf, UvError> {
    let origin = archive.display().to_string();
    let bytes = read_bounded(archive, &origin)?;
    let uv = verified_uv(&bytes, expected, &origin)?;
    with_fetch_lock(py_root, |dir, dest| place(dir, dest, &uv))
}

/// Read `archive` only if it is at most [`MAX_ARCHIVE_BYTES`] (#9966, 4a).
fn read_bounded(archive: &Path, origin: &str) -> Result<Vec<u8>, UvError> {
    let refuse = |n: u64| UvError::Archive {
        origin: origin.to_owned(),
        reason: format!("{n} bytes is over the {MAX_ARCHIVE_BYTES} byte bound; not read"),
    };
    let file = fs::File::open(archive).map_err(io_err("open uv archive", archive))?;
    let len = file
        .metadata()
        .map_err(io_err("stat uv archive", archive))?
        .len();
    if len > MAX_ARCHIVE_BYTES {
        return Err(refuse(len));
    }
    // The bound holds even if the file grows after the stat.
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(MAX_ARCHIVE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_err("read uv archive", archive))?;
    let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if read > MAX_ARCHIVE_BYTES {
        return Err(refuse(read));
    }
    Ok(bytes)
}

/// `bytes` checked against `expected`, then their `uv` entry extracted.
fn verified_uv(bytes: &[u8], expected: &Sha256Digest, origin: &str) -> Result<Vec<u8>, UvError> {
    if let Err(source) = Sha256Digest::of_bytes(bytes).verify(expected) {
        let origin = origin.to_owned();
        return Err(UvError::Checksum { origin, source });
    }
    extract_uv(bytes, origin)
}

/// Run `f(cache dir, cache path)` holding `<cache dir>/.fetch.lock`.
///
/// What: creates the cache dir, then blocks on an exclusive flock; a failure
/// to open or lock it is an `Io` error and `f` never runs. Dropping the file
/// releases the lock.
fn with_fetch_lock<T>(
    py_root: &Path,
    f: impl FnOnce(&Path, &Path) -> Result<T, UvError>,
) -> Result<T, UvError> {
    let dest = cache_path(py_root);
    let dir = dest.parent().unwrap_or(py_root);
    fs::create_dir_all(dir).map_err(io_err("create uv cache dir", dir))?;
    let lock_path = dir.join(".fetch.lock");
    let lock = fs::File::create(&lock_path).map_err(io_err("open uv lock", &lock_path))?;
    FileExt::lock_exclusive(&lock).map_err(io_err("flock uv lock", &lock_path))?;
    f(dir, &dest)
}

/// Place verified `uv` bytes at `dest`; the caller holds `.fetch.lock`.
///
/// What: sweeps every leftover temp file, writes a 0755 temp file (removed on
/// failure), renames it into place, and records its digest in `uv.sha256`.
/// Test: `stale_temp_files_are_swept_before_a_fetch`,
/// `a_stale_temp_that_cannot_be_removed_stops_the_install`.
fn place(dir: &Path, dest: &Path, uv: &[u8]) -> Result<PathBuf, UvError> {
    sweep_stale_temps(dir)?;
    // Unique under the flock, which excludes every other installer.
    let tmp = dir.join(format!("{TEMP_PREFIX}{}", std::process::id()));
    let placed = write_executable(&tmp, uv)
        .and_then(|()| fs::rename(&tmp, dest).map_err(io_err("rename uv to", dest)));
    if placed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    placed?;
    // A torn digest file fails `cached_uv`'s parse: refused, never run.
    let digest = dest.with_file_name("uv.sha256");
    let line = format!("{}  uv\n", Sha256Digest::of_bytes(uv).as_hex());
    fs::write(&digest, line).map_err(io_err("write uv digest", &digest))?;
    tracing::info!(uv = %dest.display(), "py-embedder: cached verified uv {UV_VERSION}");
    Ok(dest.to_path_buf())
}

/// The bytes of the archive's `uv` entry; nothing is written to disk here.
///
/// Why: defence in depth behind the digest — an entry path that leaves the
/// archive root, or a `uv` that is a link, fails the whole archive.
/// What: any entry path with a `..`, root or prefix component is refused; the
/// first entry whose basename is `uv` must be a regular file.
/// Test: `unsafe_archive_entries_are_refused`.
fn extract_uv(tarball: &[u8], origin: &str) -> Result<Vec<u8>, UvError> {
    let err = |reason: String| UvError::Archive {
        origin: origin.to_owned(),
        reason,
    };
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    for entry in tar.entries().map_err(|e| err(format!("reading: {e}")))? {
        let mut entry = entry.map_err(|e| err(format!("reading entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| err(format!("path: {e}")))?
            .into_owned();
        let shown = path.display();
        let safe = |c: Component| matches!(c, Component::Normal(_) | Component::CurDir);
        if !path.components().all(safe) {
            return Err(err(format!("entry {shown} escapes the archive root")));
        }
        if path.file_name().is_none_or(|name| name != "uv") {
            continue;
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() {
            let reason = format!("entry {shown} is a {kind:?}, not a regular file");
            return Err(err(reason));
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| err(format!("reading uv: {e}")))?;
        return Ok(data);
    }
    Err(err("no `uv` file in the archive".to_owned()))
}

/// Remove every `.uv.tmp.*` entry in `dir`, whatever pid wrote it (#9468).
///
/// Why: an install killed mid-write never removes its temp file, which
/// would stay for good and, under a reused pid, fail `create_new`.
/// What: runs under the flock, so no live installer owns a match. A failed
/// list or removal is an `Io` error and the install stops before writing.
/// Test: `stale_temp_files_are_swept_before_a_fetch`,
/// `a_stale_temp_that_cannot_be_removed_stops_the_install`.
fn sweep_stale_temps(dir: &Path) -> Result<(), UvError> {
    let entries = fs::read_dir(dir).map_err(io_err("list uv cache dir", dir))?;
    for entry in entries {
        let entry = entry.map_err(io_err("list uv cache dir", dir))?;
        if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            let path = entry.path();
            fs::remove_file(&path).map_err(io_err("remove stale uv temp file", &path))?;
        }
    }
    Ok(())
}

/// Create `path` with mode 0755 (Unix) and sync it; [`sweep_stale_temps`]
/// has already removed any leftover at `path`.
fn write_executable(path: &Path, data: &[u8]) -> Result<(), UvError> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o755);
    let mut file = opts.open(path).map_err(io_err("create", path))?;
    let written = file.write_all(data).and_then(|()| file.sync_all());
    written.map_err(io_err("write", path))
}

fn io_err(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> UvError {
    let path = path.to_path_buf();
    move |source| UvError::Io {
        action,
        path,
        source,
    }
}

#[cfg(test)]
#[path = "uv_fetch_tests.rs"]
mod tests;
