//! Runtime resolver for instructional content (ADR-0064, epic #8378 PR-B).
//!
//! Why: ADR-0064 decision 5 makes agents, skills, instructions and output
//! styles runtime-only content — nothing is compiled into a binary. Something
//! has to decide where that content comes from and prove it is the release
//! that was pinned. This module is that decision, shared by every harness.
//! What: [`resolve`](crate::content::resolve) picks one source, in this precedence:
//!
//! 1. the dev override — a trusty-tools checkout, named explicitly or found
//!    at the enclosing repository root, and trusted only when it carries
//!    `.git`, a `[workspace]` `Cargo.toml` and the current user's ownership
//!    ([`DevOverride`](crate::content::DevOverride));
//! 2. the installed bundle — `content-lock.toml` in the cache directory pins a
//!    tag and a sha256; the bundle is hashed and refused on any mismatch, and
//!    refused when its manifest declares a `schema_major` newer than
//!    [`SUPPORTED_SCHEMA_MAJOR`](crate::content::SUPPORTED_SCHEMA_MAJOR).
//!
//! There is no third source. With no checkout and no lock, `resolve` returns
//! [`ContentError::NotInstalled`](crate::content::ContentError::NotInstalled); a failed check never falls back to
//! a later source. Fetching and installing a bundle (`tm content update` /
//! `install --from`) is the caller's job and is not in this module.
//! Test: `content::tests` (`src/content/tests.rs`).
//!
//! # Spec References
//! - ADR-0064 decision 5, PHASE_3 acceptance criteria (i), (iii) and (iv):
//!   `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

mod bundle;
mod dev;
mod error;
mod lock;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use bundle::{
    MAX_BUNDLE_BYTES, MAX_BUNDLE_ENTRIES, MAX_UNPACKED_BYTES, SUPPORTED_SCHEMA_MAJOR,
};
pub use dev::{DEV_CLASS_SOURCES, find_dev_checkout};
pub use error::ContentError;
pub use lock::{ContentLock, TAG_PREFIX, validate_tag};

use crate::integrity::Sha256Digest;

/// The lock file's name inside the cache directory.
pub const LOCK_FILE_NAME: &str = "content-lock.toml";

/// The default cache directory, `~/.trusty-mpm/content` (ADR-0064 decision 5).
///
/// Returns `None` when the home directory cannot be determined.
pub fn default_cache_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".trusty-mpm").join("content"))
}

/// Whether, and how, a trusty-tools checkout overrides the installed bundle.
///
/// Why: a developer editing a skill in the checkout must see the edit without
/// cutting a release (ADR-0064 decision 5 (iii)), while an installed `tm` run
/// anywhere else must never read a stray tree.
/// What: `Off` skips the override; `DetectFrom` walks up from a directory
/// (normally the cwd) with [`find_dev_checkout`] and falls through to the
/// installed bundle when no trusted checkout is found; `At` names a checkout
/// that must pass the same checks, and fails with
/// [`ContentError::NotACheckout`] or [`ContentError::UntrustedCheckout`]
/// rather than falling through when it does not.
/// Test: `dev_override_wins_over_a_valid_installed_bundle`,
/// `explicit_dev_root_that_is_not_a_checkout_fails_closed`,
/// `explicit_dev_root_without_a_git_marker_is_untrusted`,
/// `detect_outside_a_checkout_uses_the_installed_bundle`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DevOverride {
    /// Never read a checkout.
    Off,
    /// Use the nearest checkout at or above this directory, if there is one.
    DetectFrom(PathBuf),
    /// Use this checkout; it is an error if it is not one.
    At(PathBuf),
}

/// Inputs to [`resolve`]; `#[non_exhaustive]`, so build it with
/// [`ResolveOptions::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResolveOptions {
    /// Directory holding `content-lock.toml` and the pinned `<tag>.tar.gz`.
    pub cache_dir: PathBuf,
    /// The dev override policy.
    pub dev: DevOverride,
}

impl ResolveOptions {
    /// Options reading the lock and bundle from `cache_dir` under `dev`.
    pub fn new(cache_dir: impl Into<PathBuf>, dev: DevOverride) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            dev,
        }
    }
}

/// Where resolved content comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContentSource {
    /// A trusty-tools checkout, read from its working tree.
    DevCheckout {
        /// The checkout root.
        root: PathBuf,
    },
    /// The installed bundle, verified against the lock.
    Installed {
        /// The pinned release tag.
        tag: String,
        /// The verified sha256 of the bundle.
        sha256: Sha256Digest,
        /// The bundle file that was verified.
        bundle: PathBuf,
    },
}

/// Content from one resolved source, addressed by bundle path
/// (`<class>/<path>`, e.g. `skills/tm/SKILL.md`).
///
/// Why: callers read the same path whichever source won, so PHASE_1's move of
/// the files changes no call site.
/// What: an installed bundle is held in memory as the verified bytes; a
/// checkout is read from disk on each call.
/// Test: `installed_bundle_serves_its_files`, `dev_checkout_serves_working_tree_files`.
#[derive(Debug)]
pub struct ResolvedContent {
    source: ContentSource,
    backing: Backing,
}

/// The bytes behind a [`ResolvedContent`].
#[derive(Debug)]
enum Backing {
    /// Verified bundle entries, keyed by bundle path.
    Memory(BTreeMap<String, Vec<u8>>),
    /// A checkout root, read on demand.
    Checkout(PathBuf),
}

/// Resolves instructional content according to `options`.
///
/// Why: the one entry point every consumer calls, so the precedence and the
/// integrity check cannot differ between harnesses.
/// What: the dev override first (see [`DevOverride`]); otherwise
/// [`ContentLock::load`] on `<cache_dir>/content-lock.toml` and a verified
/// load of `<cache_dir>/<tag>.tar.gz`. Every failure is a [`ContentError`];
/// no failure falls through to another source.
/// Test: `content::tests` — one test per error arm and per precedence rule.
pub fn resolve(options: &ResolveOptions) -> Result<ResolvedContent, ContentError> {
    let dev_root = match &options.dev {
        DevOverride::Off => None,
        DevOverride::DetectFrom(start) => find_dev_checkout(start),
        DevOverride::At(root) => {
            dev::require_checkout(root)?;
            Some(root.clone())
        }
    };
    if let Some(root) = dev_root {
        return Ok(ResolvedContent {
            source: ContentSource::DevCheckout { root: root.clone() },
            backing: Backing::Checkout(root),
        });
    }
    let lock = ContentLock::load(&options.cache_dir.join(LOCK_FILE_NAME))?;
    let files = bundle::load_verified(&options.cache_dir, &lock)?;
    Ok(ResolvedContent {
        source: ContentSource::Installed {
            tag: lock.tag().to_owned(),
            sha256: lock.sha256().clone(),
            bundle: options.cache_dir.join(lock.bundle_file_name()),
        },
        backing: Backing::Memory(files),
    })
}

impl ResolvedContent {
    /// Where this content came from.
    pub fn source(&self) -> &ContentSource {
        &self.source
    }

    /// Reads one file by bundle path. In both modes only a regular file is
    /// served; anything else is [`ContentError::NotFound`].
    ///
    /// Test: `read_rejects_a_climbing_path`, `read_of_an_absent_path_is_not_found`,
    /// `dev_read_serves_only_regular_files`.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, ContentError> {
        let key =
            bundle::relative_key(Path::new(path)).ok_or_else(|| ContentError::InvalidPath {
                path: path.to_owned(),
            })?;
        let not_found = || ContentError::NotFound {
            path: path.to_owned(),
        };
        match &self.backing {
            Backing::Memory(files) => files.get(&key).cloned().ok_or_else(not_found),
            Backing::Checkout(root) => dev::read_regular(root, &key)?.ok_or_else(not_found),
        }
    }

    /// Reads one file as UTF-8 text; invalid UTF-8 is [`ContentError::Io`].
    pub fn read_to_string(&self, path: &str) -> Result<String, ContentError> {
        String::from_utf8(self.read(path)?).map_err(|e| ContentError::Io {
            path: PathBuf::from(path),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })
    }

    /// Lists every file of one content class (`instructions`), or of a
    /// subfolder destination under one (`instructions/output-styles`), as
    /// sorted bundle paths.
    ///
    /// An unknown class lists nothing. Since #8378 the bundle has three
    /// classes; a former class name such as `output-styles` lists nothing
    /// from a current bundle or checkout.
    /// Test: `installed_bundle_serves_its_files`, `dev_checkout_serves_working_tree_files`,
    /// `dev_checkout_serves_a_nested_destination_from_its_own_source`.
    pub fn list(&self, class: &str) -> Result<Vec<String>, ContentError> {
        let prefix = format!("{class}/");
        match &self.backing {
            Backing::Memory(files) => Ok(files
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect()),
            Backing::Checkout(root) => dev::list(root, class),
        }
    }
}

#[cfg(test)]
mod tests;
