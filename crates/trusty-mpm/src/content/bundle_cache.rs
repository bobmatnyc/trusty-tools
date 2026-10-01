//! Installing, updating and inspecting the runtime content cache (ADR-0064
//! PHASE_3, epic #8378 PR-C).
//!
//! Why: `trusty_common::content::resolve` verifies and serves an installed
//! bundle but never installs one. This module is the writer behind
//! `tm content install --from`, `tm content update` and `tm content status`.
//! The fallback is runtime-only (owner ruling on #8974): nothing is compiled
//! in, so with no cache and no network every error names
//! [`INSTALL_HINT`].
//! What: every write holds an exclusive file lock on `<cache>/.update.lock`
//! for its whole read-fetch-write span, checks the bytes against the release's
//! sha256 sidecar, verifies them with the same `content::resolve` the runtime
//! uses (in a staging directory inside the cache), stores `<tag>.tar.gz`
//! atomically, and only then swaps `content-lock.toml`. A failure at any step
//! leaves the previous pin and its bundle in place.
//! Test: `content::bundle_cache::tests` (`bundle_cache_tests.rs`).
//!
//! # Spec References
//! - ADR-0064 decision 5, PHASE_3 acceptance criteria (i)-(iv):
//!   `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use trusty_common::content::{
    self, ContentError, ContentLock, ContentSource, DevOverride, LOCK_FILE_NAME, MAX_BUNDLE_BYTES,
    ResolveOptions,
};
use trusty_common::integrity::{IntegrityError, Sha256Digest};

pub use super::release_source::{FetchError, GithubReleases, ReleaseSource};

/// The file every writer locks exclusively, inside the cache directory.
pub const UPDATE_LOCK_FILE: &str = ".update.lock";

/// The command an operator runs when no network is reachable.
pub const INSTALL_HINT: &str = "tm content install --from <bundle.tar.gz>";

/// Suffix of the sha256 sidecar the release ships beside each bundle.
pub const SIDECAR_SUFFIX: &str = ".sha256";

/// Largest sidecar read; a real one is one 90-byte line.
pub const MAX_SIDECAR_BYTES: u64 = 4096;

/// What remains in use when an update cannot reach its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// A verified installed bundle, which keeps serving.
    Cached(String),
    /// Nothing verified is installed.
    None,
}

impl std::fmt::Display for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cached(tag) => write!(f, "the installed {tag} is verified and stays in use"),
            Self::None => write!(
                f,
                "no verified content bundle is installed; offline, run `{INSTALL_HINT}`"
            ),
        }
    }
}

/// Every way installing or updating the content cache fails.
///
/// Why: the fix differs per cause, and none of them may be answered by
/// pinning bytes that failed a check (owner ruling on #8974: fail closed).
/// What: `#[non_exhaustive]`; resolver failures pass through as `Content`.
/// Test: one error-arm test per variant in `bundle_cache_tests.rs`, among them
/// `install_refuses_an_unparseable_sidecar`,
/// `install_refuses_a_bundle_that_names_no_tag`,
/// `install_refuses_an_oversized_sidecar`,
/// `update_with_only_prereleases_published_has_no_release`; version order in
/// `latest_release_and_newer_report_compare_versions_not_strings`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CacheError {
    /// The resolver refused the bundle or the lock.
    #[error(transparent)]
    Content(#[from] ContentError),
    /// The sidecar is not a sha256 line.
    #[error("the sha256 sidecar {origin} is invalid: {source}")]
    Sidecar {
        /// The sidecar file or URL.
        origin: String,
        /// Why it failed to parse.
        #[source]
        source: IntegrityError,
    },
    /// An offline install found no sidecar beside the bundle.
    #[error(
        "no sha256 sidecar at {}: install the `<bundle>{SIDECAR_SUFFIX}` file the release \
         ships beside the bundle",
        path.display()
    )]
    SidecarMissing {
        /// Where the sidecar was looked for.
        path: PathBuf,
    },
    /// The bundle's bytes differ from the sidecar's digest.
    #[error(
        "refusing {origin}: the sidecar pins sha256 {expected}, but the bundle hashes to {actual}"
    )]
    ChecksumMismatch {
        /// The bundle file or URL.
        origin: String,
        /// The sidecar's digest.
        expected: Sha256Digest,
        /// The digest of the bytes received.
        actual: Sha256Digest,
    },
    /// The release, or one of its assets, does not exist upstream.
    #[error("content release {tag} was not found upstream ({url}); {fallback}")]
    TagNotFound {
        /// The requested tag.
        tag: String,
        /// The URL that answered 404.
        url: String,
        /// What stays in use.
        fallback: Fallback,
    },
    /// The release could not be reached.
    #[error("could not reach {url}: {reason}; {fallback}")]
    Network {
        /// The URL that failed.
        url: String,
        /// The transport failure.
        reason: String,
        /// What stays in use.
        fallback: Fallback,
    },
    /// The lock pins this tag with other bytes than the release now carries.
    #[error(
        "refusing {tag}: the lock pins sha256 {pinned}, but the release now hashes to {upstream}; \
         a published content release never changes"
    )]
    PinConflict {
        /// The tag.
        tag: String,
        /// The digest the lock pins.
        pinned: Sha256Digest,
        /// The digest offered now.
        upstream: Sha256Digest,
    },
    /// No `content-v*` release is published.
    #[error("no content-v* release is published upstream")]
    NoReleases,
    /// Neither the sidecar nor the bundle's file name names a release tag.
    #[error(
        "cannot tell which release {} is: its sidecar names no content-vX.Y.Z.tar.gz file, \
         and neither does its own file name",
        path.display()
    )]
    UnknownTag {
        /// The bundle file.
        path: PathBuf,
    },
    /// A local file is over its size cap.
    #[error("{} is {len} bytes, over the {cap}-byte cap", path.display())]
    TooLarge {
        /// The file.
        path: PathBuf,
        /// Its size.
        len: u64,
        /// The cap.
        cap: u64,
    },
    /// A local read or write failed.
    #[error("could not {action} {}: {source}", path.display())]
    Io {
        /// What was being done.
        action: &'static str,
        /// The path.
        path: PathBuf,
        /// The failure.
        #[source]
        source: std::io::Error,
    },
}

/// What an install or update did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAction {
    /// A new pin was written.
    Installed,
    /// The pinned bundle was missing or broken and was fetched again.
    Repaired,
    /// The pinned bundle verified; nothing was written.
    AlreadyCurrent,
}

/// The pin in force after an install or update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    /// The pinned tag.
    pub tag: String,
    /// The pinned sha256.
    pub sha256: Sha256Digest,
    /// What was done.
    pub action: UpdateAction,
    /// A newer release than the pin, found while checking; never applied.
    pub newer: Option<String>,
}

/// Installs a bundle from a local file with no network (ADR-0064 (ii)).
///
/// Why: an air-gapped host has no other way in, and the runtime-only ruling
/// leaves no compiled fallback.
/// What: reads `<bundle>` and `<bundle>.sha256` (required; a missing sidecar
/// is [`CacheError::SidecarMissing`]), takes the tag from the sidecar's file
/// name or else the bundle's, and commits it through [`commit`]. A broken lock
/// is replaced — this is the offline repair path.
/// Test: `install_from_a_file_pins_its_tag_and_sha256`,
/// `install_refuses_a_bundle_that_does_not_match_its_sidecar`,
/// `install_without_a_sidecar_is_refused`.
pub fn install_from_file(cache: &Path, bundle: &Path) -> Result<UpdateOutcome, CacheError> {
    let bytes = read_capped(bundle, MAX_BUNDLE_BYTES)?;
    let sidecar_path = sidecar_path(bundle);
    let sidecar = match read_capped(&sidecar_path, MAX_SIDECAR_BYTES) {
        Err(CacheError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(CacheError::SidecarMissing { path: sidecar_path });
        }
        other => other?,
    };
    let origin = sidecar_path.display().to_string();
    let expected = parse_sidecar(&sidecar, &origin)?;
    let tag = tag_from_sidecar(&sidecar)
        .or_else(|| tag_from_file_name(bundle))
        .ok_or_else(|| CacheError::UnknownTag {
            path: bundle.to_path_buf(),
        })?;
    let _guard = UpdateGuard::acquire(cache)?;
    let current = read_lock(cache).ok().flatten();
    let action = pin_action(current.as_ref(), &tag);
    let lock = commit(
        cache,
        &tag,
        &bytes,
        &expected,
        current.as_ref(),
        &bundle.display().to_string(),
    )?;
    Ok(outcome(lock, action, None))
}

/// Fetches and pins a content release (`tm content update`).
///
/// Why: #8389 — no flags installs the latest release, `--content-ref` pins one
/// exactly, and a later no-flag run never silently moves off a recorded pin.
/// What: under the update lock, picks the target — `content_ref`, else the
/// locked tag, else the newest published non-prerelease tag. A pinned bundle
/// that already verifies needs no network: the call succeeds and reports any
/// newer release as [`UpdateOutcome::newer`] (best effort). Otherwise fetches
/// the sidecar and the bundle, and commits them through [`commit`]. A lock that
/// does not parse is an error unless `content_ref` replaces it.
/// Test: `update_with_no_lock_installs_the_latest_release`,
/// `update_without_a_ref_keeps_the_recorded_pin`,
/// `update_refuses_a_bundle_whose_sha256_differs_from_the_sidecar`,
/// `update_to_a_tag_missing_upstream_is_a_named_error`,
/// `update_offline_with_no_cache_names_the_install_command`,
/// `update_offline_with_a_verified_cache_keeps_it_in_use`,
/// `concurrent_updates_serialise_on_the_file_lock`.
pub fn update<S: ReleaseSource + ?Sized>(
    cache: &Path,
    source: &S,
    content_ref: Option<&str>,
) -> Result<UpdateOutcome, CacheError> {
    if let Some(tag) = content_ref {
        content::validate_tag(tag)?;
    }
    let _guard = UpdateGuard::acquire(cache)?;
    let current = match read_lock(cache) {
        Ok(lock) => lock,
        Err(_) if content_ref.is_some() => None,
        Err(e) => return Err(e),
    };
    let verified = verified_tag(cache);
    let fallback = verified
        .clone()
        .map_or(Fallback::None, |(tag, _)| Fallback::Cached(tag));
    let target = match (content_ref, &current) {
        (Some(tag), _) => tag.to_owned(),
        (None, Some(lock)) => lock.tag().to_owned(),
        (None, None) => latest_tag(source, &fallback)?,
    };
    if let Some((tag, sha256)) = verified.filter(|(tag, _)| *tag == target) {
        let newer = match content_ref {
            None => latest_tag(source, &fallback)
                .ok()
                .filter(|latest| is_newer(latest, &tag)),
            Some(_) => None,
        };
        return Ok(UpdateOutcome {
            tag,
            sha256,
            action: UpdateAction::AlreadyCurrent,
            newer,
        });
    }
    let bundle_name = format!("{target}.tar.gz");
    let sidecar_name = format!("{bundle_name}{SIDECAR_SUFFIX}");
    let sidecar = fetch(source, &target, &sidecar_name, MAX_SIDECAR_BYTES, &fallback)?;
    let expected = parse_sidecar(&sidecar, &sidecar_name)?;
    let bytes = fetch(source, &target, &bundle_name, MAX_BUNDLE_BYTES, &fallback)?;
    let action = pin_action(current.as_ref(), &target);
    let lock = commit(
        cache,
        &target,
        &bytes,
        &expected,
        current.as_ref(),
        &bundle_name,
    )?;
    Ok(outcome(lock, action, None))
}

/// Verifies `bytes` and pins them: the one write path for install and update.
///
/// Why: the bundle must be on disk before the lock names it, so a crash or a
/// failed write leaves the previous pin resolvable; and only bytes the
/// runtime resolver itself accepts may be pinned.
/// What: checks the sidecar digest, refuses a pin conflict, verifies the
/// candidate with `content::resolve` in a staging directory inside the cache
/// (tag, sha256, caps, entries, manifest, schema major), writes
/// `<cache>/<tag>.tar.gz` atomically, then stores the lock atomically.
/// Test: `a_failed_bundle_write_leaves_the_previous_pin_in_force`,
/// `install_refuses_a_newer_schema_major_and_leaves_the_cache_untouched`.
fn commit(
    cache: &Path,
    tag: &str,
    bytes: &[u8],
    expected: &Sha256Digest,
    current: Option<&ContentLock>,
    origin: &str,
) -> Result<ContentLock, CacheError> {
    let actual = Sha256Digest::of_bytes(bytes);
    if &actual != expected {
        return Err(CacheError::ChecksumMismatch {
            origin: origin.to_owned(),
            expected: expected.clone(),
            actual,
        });
    }
    if let Some(pinned) = current.filter(|lock| lock.tag() == tag)
        && pinned.sha256() != &actual
    {
        return Err(CacheError::PinConflict {
            tag: tag.to_owned(),
            pinned: pinned.sha256().clone(),
            upstream: actual,
        });
    }
    let lock = ContentLock::new(tag, actual)?;
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(cache)
        .map_err(|source| io_err("create a staging directory in", cache, source))?;
    let staged = staging.path().join(lock.bundle_file_name());
    trusty_common::atomic_file::write_atomic(&staged, bytes)
        .map_err(|source| io_err("write", &staged, source))?;
    lock.store(&staging.path().join(LOCK_FILE_NAME))?;
    content::resolve(&ResolveOptions::new(staging.path(), DevOverride::Off))?;
    // #8378: the bundle is stored BEFORE the lock names it.
    let bundle = cache.join(lock.bundle_file_name());
    trusty_common::atomic_file::write_atomic(&bundle, bytes)
        .map_err(|source| io_err("write", &bundle, source))?;
    lock.store(&cache.join(LOCK_FILE_NAME))?;
    Ok(lock)
}

/// An exclusive lock on `<cache>/.update.lock`, released on drop.
///
/// Why: an update reads the pin, fetches, then writes; two unserialised
/// updates lose one of the writes — a no-flag update could put back the pin a
/// concurrent `--content-ref` had just replaced.
/// Test: `concurrent_updates_serialise_on_the_file_lock`.
struct UpdateGuard(#[allow(dead_code)] File);

impl UpdateGuard {
    fn acquire(cache: &Path) -> Result<Self, CacheError> {
        std::fs::create_dir_all(cache).map_err(|source| io_err("create", cache, source))?;
        let path = cache.join(UPDATE_LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| io_err("open", &path, source))?;
        file.lock()
            .map_err(|source| io_err("lock", &path, source))?;
        Ok(Self(file))
    }
}

/// The lock in the cache: `Ok(None)` when none is installed.
fn read_lock(cache: &Path) -> Result<Option<ContentLock>, CacheError> {
    match ContentLock::load(&cache.join(LOCK_FILE_NAME)) {
        Ok(lock) => Ok(Some(lock)),
        Err(ContentError::NotInstalled { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The installed pin, when its bundle verifies.
fn verified_tag(cache: &Path) -> Option<(String, Sha256Digest)> {
    match content::resolve(&ResolveOptions::new(cache, DevOverride::Off))
        .ok()?
        .source()
    {
        ContentSource::Installed { tag, sha256, .. } => Some((tag.clone(), sha256.clone())),
        _ => None,
    }
}

fn pin_action(current: Option<&ContentLock>, tag: &str) -> UpdateAction {
    match current {
        Some(lock) if lock.tag() == tag => UpdateAction::Repaired,
        _ => UpdateAction::Installed,
    }
}

fn outcome(lock: ContentLock, action: UpdateAction, newer: Option<String>) -> UpdateOutcome {
    UpdateOutcome {
        tag: lock.tag().to_owned(),
        sha256: lock.sha256().clone(),
        action,
        newer,
    }
}

/// The newest published tag that is not a prerelease.
fn latest_tag<S: ReleaseSource + ?Sized>(
    source: &S,
    fallback: &Fallback,
) -> Result<String, CacheError> {
    let tags = source.content_tags().map_err(|e| CacheError::Network {
        url: e.url,
        reason: e.reason,
        fallback: fallback.clone(),
    })?;
    tags.into_iter()
        .filter_map(|tag| Some((release_version(&tag)?, tag)))
        .filter(|(version, _)| version.pre.is_empty())
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, tag)| tag)
        .ok_or(CacheError::NoReleases)
}

fn release_version(tag: &str) -> Option<semver::Version> {
    content::validate_tag(tag).ok()?;
    semver::Version::parse(tag.strip_prefix(content::TAG_PREFIX)?).ok()
}

fn is_newer(candidate: &str, pinned: &str) -> bool {
    match (release_version(candidate), release_version(pinned)) {
        (Some(c), Some(p)) => c > p,
        _ => false,
    }
}

fn fetch<S: ReleaseSource + ?Sized>(
    source: &S,
    tag: &str,
    file: &str,
    max_bytes: u64,
    fallback: &Fallback,
) -> Result<Vec<u8>, CacheError> {
    match source.asset(tag, file, max_bytes) {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(CacheError::TagNotFound {
            tag: tag.to_owned(),
            url: source.asset_url(tag, file),
            fallback: fallback.clone(),
        }),
        Err(e) => Err(CacheError::Network {
            url: e.url,
            reason: e.reason,
            fallback: fallback.clone(),
        }),
    }
}

fn parse_sidecar(bytes: &[u8], origin: &str) -> Result<Sha256Digest, CacheError> {
    let text = String::from_utf8_lossy(bytes);
    Sha256Digest::from_sidecar(&text).map_err(|source| CacheError::Sidecar {
        origin: origin.to_owned(),
        source,
    })
}

/// The tag a `sha256sum` line names: `<hex>  [*]content-vX.Y.Z.tar.gz`.
fn tag_from_sidecar(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let name = text.split_whitespace().nth(1)?.trim_start_matches('*');
    tag_from_file_name(Path::new(name))
}

fn tag_from_file_name(path: &Path) -> Option<String> {
    let tag = path.file_name()?.to_str()?.strip_suffix(".tar.gz")?;
    content::validate_tag(tag).ok().map(|()| tag.to_owned())
}

fn sidecar_path(bundle: &Path) -> PathBuf {
    let mut name = bundle.as_os_str().to_owned();
    name.push(SIDECAR_SUFFIX);
    PathBuf::from(name)
}

fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, CacheError> {
    let file = File::open(path).map_err(|source| io_err("read", path, source))?;
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| io_err("read", path, source))?;
    let len = bytes.len() as u64;
    if len > cap {
        return Err(CacheError::TooLarge {
            path: path.to_path_buf(),
            len,
            cap,
        });
    }
    Ok(bytes)
}

fn io_err(action: &'static str, path: &Path, source: std::io::Error) -> CacheError {
    CacheError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
#[path = "bundle_cache_tests.rs"]
mod tests;
