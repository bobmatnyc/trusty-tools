//! `tm content` — install, update and inspect the runtime instructional
//! content (ADR-0064, #8378 PR-C).
//!
//! Why: agents, skills and instructions become runtime-only content pinned in
//! `~/.trusty-mpm/content/content-lock.toml`; these verbs are how it gets there.
//! What: [`ContentAction`] — `install --from`, `update [--content-ref]`, `status`.
//! Test: `cli_parses_content_install_update_and_status`.

use std::path::PathBuf;

use clap::Subcommand;

/// Actions for the `content` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum ContentAction {
    /// Install a content bundle from a local file, with no network.
    ///
    /// The release's `<bundle>.sha256` sidecar must sit beside the bundle; the
    /// bundle is refused unless it matches it and passes the same checks the
    /// runtime applies. Only then is it stored and pinned.
    Install {
        /// The `content-vX.Y.Z.tar.gz` bundle.
        #[arg(long, value_name = "BUNDLE")]
        from: PathBuf,
    },
    /// Fetch and pin a content release from GitHub.
    ///
    /// With no flag: the latest release when nothing is installed, otherwise
    /// the pinned release again (a newer one is reported, never applied). With
    /// `--content-ref`: exactly that release. Fails closed on any sha256
    /// mismatch; the previous pin stays in force.
    Update {
        /// The release tag to pin, e.g. `content-v0.1.0`.
        #[arg(long, value_name = "TAG")]
        content_ref: Option<String>,
    },
    /// Show the content source `tm` reads and the installed pin's health.
    ///
    /// Exits non-zero when no source can serve content.
    Status,
}
