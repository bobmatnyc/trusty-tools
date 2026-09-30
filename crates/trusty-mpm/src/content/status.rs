//! Which instructional-content source `tm` reads, and the installed pin's
//! health (`tm content status`, the doctor `content` row; #8378 PR-C).
//!
//! Why: ADR-0064 makes the content version independent of the binary version;
//! an operator needs both, side by side, without either reading as an error
//! when they differ (#8389).
//! What: [`content_status`] runs the dev-checkout detection and a verifying
//! `content::resolve` of the cache; [`ContentStatus`] renders both.
//! Test: `status_reports_a_verified_bundle`, `status_with_nothing_installed_names_the_fix`,
//! `status_reports_a_tampered_bundle_as_unhealthy`.

use std::path::{Path, PathBuf};

use trusty_common::content::{
    self, ContentError, ContentSource, DevOverride, ResolveOptions, find_dev_checkout,
};
use trusty_common::integrity::Sha256Digest;

use super::bundle_cache::INSTALL_HINT;

/// A verified installed pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPin {
    /// The pinned tag.
    pub tag: String,
    /// The pinned, verified sha256.
    pub sha256: Sha256Digest,
}

/// The content sources visible from one directory.
#[derive(Debug)]
pub struct ContentStatus {
    /// The cache directory inspected.
    pub cache_dir: PathBuf,
    /// A trusted trusty-tools checkout above the start directory, if any.
    pub dev: Option<PathBuf>,
    /// The installed pin, verified, or why it is not usable.
    pub installed: Result<InstalledPin, ContentError>,
}

/// Inspects `cache_dir`, and the checkout above `start` when given.
///
/// Test: `status_reports_a_verified_bundle`.
pub fn content_status(cache_dir: &Path, start: Option<&Path>) -> ContentStatus {
    let installed =
        content::resolve(&ResolveOptions::new(cache_dir, DevOverride::Off)).and_then(|resolved| {
            match resolved.source() {
                ContentSource::Installed { tag, sha256, .. } => Ok(InstalledPin {
                    tag: tag.clone(),
                    sha256: sha256.clone(),
                }),
                // `DevOverride::Off` resolves only an installed bundle.
                _ => Err(ContentError::NotInstalled {
                    lock_path: cache_dir.join(content::LOCK_FILE_NAME),
                }),
            }
        });
    ContentStatus {
        cache_dir: cache_dir.to_path_buf(),
        dev: start.and_then(find_dev_checkout),
        installed,
    }
}

impl ContentStatus {
    /// `dev`, `bundle` or `none` — the source `content::resolve` would pick.
    pub fn source_label(&self) -> &'static str {
        match (&self.dev, &self.installed) {
            (Some(_), _) => "dev",
            (None, Ok(_)) => "bundle",
            (None, Err(_)) => "none",
        }
    }

    /// Whether some source can serve content.
    pub fn serves(&self) -> bool {
        self.dev.is_some() || self.installed.is_ok()
    }

    /// Whether no lock is installed at all, as against a broken one.
    pub fn not_installed(&self) -> bool {
        matches!(self.installed, Err(ContentError::NotInstalled { .. }))
    }

    /// One line per fact, for `tm content status` and the doctor row.
    ///
    /// Test: `status_with_nothing_installed_names_the_fix`.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!("source: {}", self.source_label())];
        if let Some(root) = &self.dev {
            lines.push(format!("dev checkout: {}", root.display()));
        }
        match &self.installed {
            Ok(pin) => lines.push(format!(
                "installed: {} sha256 {} (verified)",
                pin.tag, pin.sha256
            )),
            Err(ContentError::NotInstalled { .. }) => lines.push(format!(
                "installed: none — run `tm content update`, or offline `{INSTALL_HINT}`"
            )),
            Err(e) => lines.push(format!(
                "installed: UNHEALTHY — {e}; run `tm content update` to fetch the pinned \
                 release again, or offline `{INSTALL_HINT}`"
            )),
        }
        lines.push(format!("cache: {}", self.cache_dir.display()));
        lines.push(format!("binary: trusty-mpm {}", env!("CARGO_PKG_VERSION")));
        lines
    }
}
