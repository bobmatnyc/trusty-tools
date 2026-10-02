//! `tm content` — install, update and inspect the runtime instructional
//! content (ADR-0064, #8378 PR-C).
//!
//! Why: agents, skills and instructions become runtime-only content pinned in
//! `~/.trusty-mpm/content/content-lock.toml`; these verbs are how it gets there.
//! What: [`ContentAction`] — `install --from`, `update [--content-ref]`, `status`.
//! Test: `cli_parses_content_install_update_and_status`,
//! `update_help_limits_the_pin_check_to_the_current_pin`.

use std::path::PathBuf;

use clap::Subcommand;

/// Actions for the `content` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum ContentAction {
    /// Install a content bundle from a local file, with no network.
    ///
    /// The release's `<bundle>.sha256` sidecar is required and must sit beside
    /// the bundle; the bundle is refused unless it matches it and passes the
    /// same checks the runtime applies. Only then is it stored and pinned.
    ///
    /// Limit: the sidecar proves the transfer only. It comes from the same
    /// place as the bundle, so it catches a corrupt or truncated copy, not a
    /// bundle someone replaced together with its sidecar. Trust is on first
    /// use: the sha256 pinned here is what every later read is checked
    /// against.
    Install {
        /// The `content-vX.Y.Z.tar.gz` bundle.
        #[arg(long, value_name = "BUNDLE")]
        from: PathBuf,
    },
    /// Fetch and pin a content release from GitHub.
    ///
    /// With no flag: the newest published release — not a draft, not a
    /// pre-release, as GitHub's releases API lists them — and re-pins to it.
    /// With `--content-ref`: exactly that release. The pin changes only when
    /// this command runs. The release's `.sha256` sidecar is required. Fails
    /// closed on a sha256 mismatch, a missing sidecar or an unreachable
    /// release; the previous pin stays in force.
    ///
    /// Limit: the sidecar proves the transfer only. It is downloaded from the
    /// same release as the bundle, so it catches a corrupt or truncated
    /// download, not a release someone replaced together with its sidecar.
    /// Trust is on first use: the pinned sha256 is what every later read
    /// checks, and a re-fetch of the currently pinned tag that returns other
    /// bytes is refused. Only the current pin is checked.
    Update {
        /// The release tag to pin, e.g. `content-v0.1.0`.
        #[arg(long, value_name = "TAG")]
        content_ref: Option<String>,
    },
    /// Show the content source `tm` reads and the installed pin's health.
    ///
    /// Exits non-zero when no source can serve content. Until ADR-0064
    /// PHASE_1, nothing installed prints an `info:` line and exits 0, because
    /// the binary's built-in content still serves.
    Status,
}
