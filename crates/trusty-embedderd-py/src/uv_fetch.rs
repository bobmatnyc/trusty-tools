//! Locate `uv`, or fetch a pinned, SHA-256-verified uv release when none is
//! installed (#9468).
//!
//! Why: the venv bootstrap needs `uv`, and a host without one used to fail
//! the bootstrap outright, pinning trusty-search on the ort embedder until the
//! operator installed uv by hand. Fetching a pinned release removes that user
//! step without trusting the network: the tarball's digest is embedded in this
//! binary, and the runtime never reads the upstream `.sha256` sidecar.
//! What: [`resolve_uv`] tries, in order, `TRUSTY_UV_BIN`, the `PATH` and
//! well-known install dirs, a cached `<data>/py-embedder/uv/<version>/uv`, and
//! finally a download of [`UV_VERSION`] for the host triple. The download is
//! verified against [`PINNED_DIGESTS`] before the binary is extracted, then
//! written to a temp file and renamed into place with mode 0755, under a
//! `flock` of its own so concurrent bootstraps (even for different lock
//! hashes) never corrupt the cache. `TRUSTY_UV_FETCH=0` disables the download.
//! Test: `uv_fetch_tests.rs`.

use std::fs;
use std::io;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fs4::FileExt;
use trusty_common::integrity::{IntegrityError, Sha256Digest};

/// The pinned uv release fetched when no `uv` is installed (#9468).
pub const UV_VERSION: &str = "0.12.23";

/// SHA-256 of `uv-<triple>.tar.gz` for [`UV_VERSION`], per supported triple.
///
/// Why: these are the only two environments `python/uv.lock` resolves. Each
/// digest was checked against both Astral's `.sha256` sidecar and a local hash
/// of the downloaded tarball before it was embedded (#9468).
const PINNED_DIGESTS: &[(&str, &str)] = &[
    (
        "aarch64-apple-darwin",
        "50487ae565ccd96e499056b4674d438f4c53170202617b4c759defe0c6a1b544",
    ),
    (
        "x86_64-unknown-linux-gnu",
        "9167d72b3319674b6303c4cbe071854bba13ebdf3d76b1a7cbdc175471fb66d6",
    ),
];

/// Env var that disables the download step when set to `0`/`false`/`off`/`no`.
pub const UV_FETCH_ENV: &str = "TRUSTY_UV_FETCH";

/// Env var naming an explicit `uv` binary; always wins over everything else.
pub const UV_BIN_ENV: &str = "TRUSTY_UV_BIN";

/// Upper bound on the downloaded tarball (the real ones are ~17-20 MB).
const MAX_TARBALL_BYTES: u64 = 64 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Why `uv` could not be located or fetched.
///
/// Why: the caller logs this and falls back to ort; each variant names its
/// cause so the operator knows whether to install uv, fix a variable, or
/// look at the network.
/// Test: `a_digest_mismatch_places_nothing_and_names_the_checksum`,
/// `disabled_fetch_names_both_variables_and_never_fetches`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UvError {
    /// `TRUSTY_UV_BIN` is set but names no file. Never falls through to a fetch.
    #[error("TRUSTY_UV_BIN={value:?} does not point to an existing file")]
    BadOverride {
        /// The rejected value.
        value: String,
    },
    /// No `uv` anywhere and the download is disabled.
    #[error(
        "`uv` not found (TRUSTY_UV_BIN unset, nothing on PATH or in the well-known \
         install dirs, no cached copy) and TRUSTY_UV_FETCH=0 disables the pinned \
         download. Install uv (https://docs.astral.sh/uv/), set TRUSTY_UV_BIN=/path/to/uv, \
         or unset TRUSTY_UV_FETCH. Until then trusty-search falls back to the Rust ort embedder."
    )]
    Disabled,
    /// No pinned digest exists for this host, so nothing is downloaded.
    #[error(
        "`uv` not found and no pinned uv {UV_VERSION} digest exists for host triple \
         {triple}; install uv or set TRUSTY_UV_BIN=/path/to/uv"
    )]
    UnsupportedHost {
        /// The host triple that has no entry.
        triple: String,
    },
    /// The download failed (connect, HTTP status, body read, or size bound).
    #[error("`uv` not found and downloading {url} failed: {reason}")]
    Network {
        /// The asset URL.
        url: String,
        /// What went wrong.
        reason: String,
    },
    /// The downloaded bytes do not hash to the embedded digest.
    #[error("downloaded uv tarball {url} failed its checksum: {source}")]
    Checksum {
        /// The asset URL.
        url: String,
        /// The expected/actual digests.
        #[source]
        source: IntegrityError,
    },
    /// The verified tarball could not be read or holds no `uv` file.
    #[error("downloaded uv tarball {url} is unusable: {reason}")]
    Archive {
        /// The asset URL.
        url: String,
        /// What went wrong.
        reason: String,
    },
    /// A filesystem step of placing the binary failed.
    #[error("{action} {}: {source}", path.display())]
    Io {
        /// The step that failed.
        action: &'static str,
        /// The path it acted on.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },
}

/// Downloads one URL into memory. Injected so tests count calls and serve bytes.
pub(crate) type Fetcher<'a> = &'a (dyn Fn(&str) -> Result<Vec<u8>, UvError> + Sync);

/// The inputs to [`resolve_uv`], read from the environment in production.
pub(crate) struct UvSources<'a> {
    /// `TRUSTY_UV_BIN`, when set.
    pub explicit: Option<String>,
    /// The `PATH` + well-known install dir lookup.
    pub on_path: &'a dyn Fn() -> Option<PathBuf>,
    /// `false` when `TRUSTY_UV_FETCH` disables the download.
    pub fetch_enabled: bool,
    /// The downloader.
    pub fetch: Fetcher<'a>,
}

/// Resolve `uv` for the venv build, downloading the pinned release as a last
/// resort (#9468).
///
/// Why: called only from `build_venv`, under the bootstrap flock, so the
/// download never runs on the daemon's listener or startup path.
/// What: [`resolve_uv`] over the real environment, `resolve_binary`, and
/// [`http_fetch`].
/// Test: `resolve_uv_*` in `uv_fetch_tests.rs` cover the order through the seam.
pub(crate) fn locate_or_fetch_uv(py_root: &Path) -> Result<PathBuf, UvError> {
    let on_path = || trusty_common::bin_resolve::resolve_binary("uv");
    let sources = UvSources {
        explicit: std::env::var(UV_BIN_ENV).ok(),
        on_path: &on_path,
        fetch_enabled: fetch_enabled(std::env::var(UV_FETCH_ENV).ok().as_deref()),
        fetch: &http_fetch,
    };
    resolve_uv(py_root, &sources)
}

/// The resolution order behind [`locate_or_fetch_uv`], with every source injected.
///
/// What: `explicit` → `on_path` → [`cached_uv`] → download. An existing `uv`
/// always wins over a download; a bad `explicit` errors instead of falling
/// through. `py_root` is `<data>/py-embedder`.
/// Test: `resolve_uv_prefers_explicit_override_and_never_fetches`,
/// `resolve_uv_prefers_path_and_never_fetches`,
/// `resolve_uv_bad_override_errors_without_fetching`,
/// `a_cache_hit_never_fetches`, `disabled_fetch_names_both_variables_and_never_fetches`.
pub(crate) fn resolve_uv(py_root: &Path, src: &UvSources<'_>) -> Result<PathBuf, UvError> {
    if let Some(value) = &src.explicit {
        return explicit_uv(value);
    }
    if let Some(found) = (src.on_path)() {
        return Ok(found);
    }
    if let Some(cached) = cached_uv(py_root) {
        return Ok(cached);
    }
    if !src.fetch_enabled {
        return Err(UvError::Disabled);
    }
    fetch_for_triple(py_root, &host_triple(), src.fetch)
}

/// Validate a `TRUSTY_UV_BIN` value: it must name an existing file.
pub(crate) fn explicit_uv(value: &str) -> Result<PathBuf, UvError> {
    let path = PathBuf::from(value);
    if path.is_file() {
        Ok(path)
    } else {
        Err(UvError::BadOverride {
            value: value.to_owned(),
        })
    }
}

/// Is the download enabled for this `TRUSTY_UV_FETCH` value? Unset means yes.
///
/// Test: `fetch_enabled_parses_the_off_switch`.
pub(crate) fn fetch_enabled(value: Option<&str>) -> bool {
    let Some(value) = value else { return true };
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// `<py_root>/uv/<UV_VERSION>/uv` — where a fetched uv lives.
pub(crate) fn cache_path(py_root: &Path) -> PathBuf {
    py_root.join("uv").join(UV_VERSION).join("uv")
}

/// The cached uv, if it exists as a file with an execute bit set.
///
/// Test: `a_cache_hit_never_fetches`, `a_non_executable_cache_file_is_not_a_hit`.
pub(crate) fn cached_uv(py_root: &Path) -> Option<PathBuf> {
    let path = cache_path(py_root);
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(path)
}

/// The Rust target triple of this host, in Astral's asset naming.
pub(crate) fn host_triple() -> String {
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

/// The embedded digest for `triple`, if it is a supported host.
pub(crate) fn pinned_digest(triple: &str) -> Option<Sha256Digest> {
    PINNED_DIGESTS
        .iter()
        .find(|(t, _)| *t == triple)
        .and_then(|(_, hex)| Sha256Digest::parse_hex(hex).ok())
}

/// The release asset URL for `triple`.
pub(crate) fn asset_url(triple: &str) -> String {
    format!("https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/uv-{triple}.tar.gz")
}

/// Look up `triple`'s pinned digest and fetch against it.
///
/// Test: `an_unsupported_host_never_fetches`.
pub(crate) fn fetch_for_triple(
    py_root: &Path,
    triple: &str,
    fetch: Fetcher<'_>,
) -> Result<PathBuf, UvError> {
    let expected = pinned_digest(triple).ok_or_else(|| UvError::UnsupportedHost {
        triple: triple.to_owned(),
    })?;
    fetch_verified(py_root, triple, &expected, fetch)
}

/// Download, verify, extract and place uv at [`cache_path`] (#9468).
///
/// Why: a half-written or unverified binary at the cache path would be
/// executed by every later bootstrap, so the cache path must only ever hold a
/// complete, verified file.
/// What: takes `<cache dir>/.fetch.lock` (flock), returns a cache hit another
/// process placed while this one waited, else fetches the asset, checks it
/// against `expected` before reading the archive, streams the entry whose
/// basename is `uv` into a temp file, sets mode 0755 and renames it into
/// place. Every error path removes the temp file.
/// Test: `a_digest_mismatch_places_nothing_and_names_the_checksum`,
/// `extraction_finds_uv_by_basename_under_any_prefix`,
/// `a_tarball_without_uv_places_nothing`, `a_network_error_places_nothing`,
/// `concurrent_fetches_download_once_and_place_a_whole_file`.
pub(crate) fn fetch_verified(
    py_root: &Path,
    triple: &str,
    expected: &Sha256Digest,
    fetch: Fetcher<'_>,
) -> Result<PathBuf, UvError> {
    let dest = cache_path(py_root);
    let dir = dest.parent().unwrap_or(py_root).to_path_buf();
    fs::create_dir_all(&dir).map_err(io_err("create uv cache dir", &dir))?;
    let lock_path = dir.join(".fetch.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(io_err("open uv fetch lock", &lock_path))?;
    FileExt::lock_exclusive(&lock).map_err(io_err("flock uv fetch lock", &lock_path))?;
    let result = fetch_locked(py_root, triple, expected, fetch, &dir, &dest);
    let _ = FileExt::unlock(&lock);
    result
}

fn fetch_locked(
    py_root: &Path,
    triple: &str,
    expected: &Sha256Digest,
    fetch: Fetcher<'_>,
    dir: &Path,
    dest: &Path,
) -> Result<PathBuf, UvError> {
    // A concurrent bootstrap may have placed it while this one waited.
    if let Some(cached) = cached_uv(py_root) {
        return Ok(cached);
    }
    let url = asset_url(triple);
    tracing::warn!("py-embedder: `uv` not found — fetching pinned uv {UV_VERSION} from {url}");
    let bytes = fetch(&url)?;
    Sha256Digest::of_bytes(&bytes)
        .verify(expected)
        .map_err(|source| UvError::Checksum {
            url: url.clone(),
            source,
        })?;

    let tmp = TempFile::new(dir);
    extract_uv(&bytes, &tmp.path, &url)?;
    set_executable(&tmp.path)?;
    fs::rename(&tmp.path, dest).map_err(io_err("rename uv into place at", dest))?;
    tmp.disarm();
    tracing::info!(uv = %dest.display(), "py-embedder: placed verified uv {UV_VERSION}");
    Ok(dest.to_path_buf())
}

/// Stream the first regular-file entry whose basename is `uv` into `out`.
fn extract_uv(tarball: &[u8], out: &Path, url: &str) -> Result<(), UvError> {
    let archive_err = |reason: String| UvError::Archive {
        url: url.to_owned(),
        reason,
    };
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    let entries = archive
        .entries()
        .map_err(|e| archive_err(format!("reading entries: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| archive_err(format!("reading entry: {e}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let is_uv = entry
            .path()
            .map_err(|e| archive_err(format!("reading entry path: {e}")))?
            .file_name()
            .is_some_and(|n| n == "uv");
        if !is_uv {
            continue;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out)
            .map_err(io_err("create temp uv", out))?;
        io::copy(&mut entry, &mut file).map_err(|e| archive_err(format!("extracting uv: {e}")))?;
        file.sync_all().map_err(io_err("sync temp uv", out))?;
        return Ok(());
    }
    Err(archive_err("no `uv` file in the archive".to_owned()))
}

fn set_executable(path: &Path) -> Result<(), UvError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(io_err("chmod 0755", path))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn io_err(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> UvError {
    let path = path.to_path_buf();
    move |source| UvError::Io {
        action,
        path,
        source,
    }
}

/// A uniquely named temp path in the cache dir, removed on drop unless disarmed.
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl TempFile {
    fn new(dir: &Path) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        Self {
            path: dir.join(format!(".uv.tmp.{}.{n}", std::process::id())),
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Download `url` over HTTPS into memory, honouring the operator's proxy.
///
/// Why: `reqwest::blocking` refuses to run on a thread that is inside a tokio
/// runtime, and `build_venv` is reached from `spawn_blocking` and
/// `block_in_place`. A dedicated thread has no runtime context.
/// What: GET with connect/request bounds, a success-status check, and a
/// [`MAX_TARBALL_BYTES`] body bound. Every failure is [`UvError::Network`].
/// Test: `real_pinned_asset_matches_the_embedded_digest` (`#[ignore]`, network).
pub(crate) fn http_fetch(url: &str) -> Result<Vec<u8>, UvError> {
    let owned = url.to_owned();
    std::thread::Builder::new()
        .name("uv-fetch".to_owned())
        .spawn(move || http_fetch_on_this_thread(&owned))
        .map_err(|e| network(url, format!("spawn download thread: {e}")))?
        .join()
        .map_err(|_| network(url, "download thread panicked".to_owned()))?
}

fn http_fetch_on_this_thread(url: &str) -> Result<Vec<u8>, UvError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("trusty-embedderd-py/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| network(url, error_chain(&e)))?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| network(url, error_chain(&e)))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(network(url, format!("HTTP {status}")));
    }
    let mut body = Vec::new();
    resp.take(MAX_TARBALL_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|e| network(url, format!("reading body: {e}")))?;
    if body.len() as u64 > MAX_TARBALL_BYTES {
        return Err(network(
            url,
            format!("body exceeds {MAX_TARBALL_BYTES} bytes"),
        ));
    }
    Ok(body)
}

fn network(url: &str, reason: String) -> UvError {
    UvError::Network {
        url: url.to_owned(),
        reason,
    }
}

/// `e` and its source chain, joined by `: ` (reqwest's own Display is terse).
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(src) = cur {
        out.push_str(": ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}

#[cfg(test)]
#[path = "uv_fetch_tests.rs"]
mod tests;
