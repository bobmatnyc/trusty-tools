//! Typed failures of the content resolver.

use std::path::PathBuf;

use crate::integrity::Sha256Digest;

/// Every way resolving or reading instructional content can fail.
///
/// Why: ADR-0064 decision 5 makes content runtime-only, so a failure here
/// means `tm` has no agents or skills to serve. Each variant names a distinct
/// cause the caller turns into a distinct instruction; none of them is ever
/// answered by serving unverified bytes instead.
/// What: `#[non_exhaustive]` so a later variant is additive.
/// Test: one error-arm test per variant in `content::tests`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ContentError {
    /// No `content-lock.toml` exists, so no bundle is installed.
    #[error("no content bundle is installed: {} does not exist", lock_path.display())]
    NotInstalled {
        /// Where the lock was looked for.
        lock_path: PathBuf,
    },
    /// The lock file exists but could not be read.
    #[error("could not read the content lock {}: {source}", path.display())]
    LockUnreadable {
        /// The lock file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The lock file is not a valid `content-lock.toml`.
    #[error("the content lock {} is invalid: {reason}", path.display())]
    LockInvalid {
        /// The lock file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// A tag is not of the form `content-vX.Y.Z[-pre]`.
    #[error("{tag:?} is not a content release tag (expected content-vX.Y.Z[-pre])")]
    InvalidTag {
        /// The rejected tag.
        tag: String,
    },
    /// The lock could not be written.
    #[error("could not write the content lock {}: {source}", path.display())]
    LockWrite {
        /// The lock file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The lock pins a bundle that is not in the cache.
    #[error("the pinned content bundle {} is missing", path.display())]
    BundleMissing {
        /// Where the bundle was expected.
        path: PathBuf,
    },
    /// The pinned bundle exists but could not be read.
    #[error("could not read the content bundle {}: {source}", path.display())]
    BundleUnreadable {
        /// The bundle file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The bundle's bytes do not hash to the sha256 the lock pins.
    #[error(
        "refusing to load the content bundle {}: the lock pins sha256 {expected}, \
         but the bundle hashes to {actual}",
        path.display()
    )]
    ChecksumMismatch {
        /// The bundle file.
        path: PathBuf,
        /// The digest the lock pins.
        expected: Sha256Digest,
        /// The digest the bundle actually hashes to.
        actual: Sha256Digest,
    },
    /// The bundle passed its checksum but is not a readable content archive.
    #[error("the content bundle {} is malformed: {reason}", path.display())]
    BundleCorrupt {
        /// The bundle file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// The bundle, its unpacked contents or its entry count exceed a cap.
    #[error("refusing to load the content bundle {}: {reason}", path.display())]
    BundleTooLarge {
        /// The bundle file.
        path: PathBuf,
        /// Which cap was exceeded, and by what.
        reason: String,
    },
    /// The bundle's own manifest names a different tag than the lock.
    #[error("the lock pins {lock_tag}, but the bundle's manifest says {bundle_tag}")]
    TagMismatch {
        /// The tag the lock pins.
        lock_tag: String,
        /// The tag in the bundle's `bundle-manifest.toml`.
        bundle_tag: String,
    },
    /// The bundle's manifest declares a layout newer than this reader
    /// (ADR-0064 PHASE_3 (iv)).
    #[error(
        "refusing to load the content bundle {}: it declares schema_major {bundle}, \
         and this binary reads schema_major {supported} or older; upgrade tm, or pin \
         an older content release",
        path.display()
    )]
    UnsupportedSchema {
        /// The bundle file.
        path: PathBuf,
        /// The `schema_major` the bundle declares.
        bundle: u32,
        /// The newest `schema_major` this binary reads.
        supported: u32,
    },
    /// The dev checkout named explicitly lacks a content class directory.
    #[error("{} is not a content checkout: {} is missing", root.display(), missing.display())]
    NotACheckout {
        /// The directory that was named.
        root: PathBuf,
        /// The first class directory that does not exist.
        missing: PathBuf,
    },
    /// A tree with every class directory is not trusted as a checkout: no
    /// `.git`, no `[workspace]` `Cargo.toml`, or another user owns it.
    #[error("refusing to read content from {}: {reason}", root.display())]
    UntrustedCheckout {
        /// The candidate checkout root.
        root: PathBuf,
        /// Which check failed.
        reason: String,
    },
    /// A requested content path is absolute, empty, or climbs with `..`.
    #[error("{path:?} is not a relative content path")]
    InvalidPath {
        /// The rejected path.
        path: String,
    },
    /// The content path does not exist in the resolved source.
    #[error("content path {path:?} does not exist")]
    NotFound {
        /// The requested path.
        path: String,
    },
    /// A dev-checkout file could not be read.
    #[error("could not read {}: {source}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}
