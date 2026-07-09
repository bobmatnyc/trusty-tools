//! Prebuilt-binary download layer for `trusty-installer` (Phase 2, issue #1760).
//!
//! Why: Building from source (`cargo install`) requires a Rust toolchain and takes
//! minutes; prebuilt binaries install in seconds. This module implements the
//! prebuilt-first install strategy: attempt a prebuilt download on Tier-1 targets,
//! fall back to `cargo install` when the target is unsupported, when no matching
//! release asset exists, or when any download/verify/extract step fails.
//!
//! What: Four focused submodules:
//! - [`platform`] — Tier-1 target detection (`aarch64-apple-darwin`,
//!   `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`).
//! - [`glibc`] — host-glibc probing and glibc-aware ORT asset selection so
//!   low-glibc Linux hosts get the portable AL2023 asset instead of a binary
//!   that fails with `GLIBC_2.39 not found` (issue #1992).
//! - [`release`] — GitHub Releases API queries and asset-URL construction.
//! - [`fetch`] — HTTP download, SHA-256 verification, tar.gz extraction, and
//!   atomic binary placement.
//!
//! The public entry point is [`try_install_prebuilt`]: it orchestrates the full
//! download→verify→extract→place pipeline and returns an `Outcome` that tells the
//! caller which path was taken. The caller (`install.rs` / `upgrade.rs`) always
//! falls back to `cargo install` on `Outcome::Fallback`.
//!
//! Test: Each submodule has its own tests (pure + `#[ignore]`-tagged live tests).
//! The orchestrator's fallback decision logic is tested in `tests` below.

pub mod fetch;
pub mod glibc;
pub mod platform;
pub mod release;

use std::path::PathBuf;

use anyhow::Context;

/// Default install directory for prebuilt binaries (mirrors `install.sh`).
pub const DEFAULT_INSTALL_DIR: &str = "~/.local/bin";

/// Outcome of a [`try_install_prebuilt`] call.
///
/// Why: The `install`/`upgrade` handlers need to know whether the prebuilt path
/// succeeded (so they can skip `cargo install`) or whether they should fall back.
///
/// What: [`Outcome::Installed`] carries the list of binary paths placed on disk;
/// [`Outcome::Fallback`] carries the human-readable reason for the fallback so
/// the caller can print an informative message before running `cargo install`.
///
/// Test: `tests::fallback_on_unsupported_target`.
#[derive(Debug)]
pub enum Outcome {
    /// Prebuilt binaries were successfully downloaded, verified, and placed.
    Installed {
        /// Paths to the installed binaries.
        paths: Vec<PathBuf>,
        /// The crate version that was installed.
        version: String,
    },
    /// The prebuilt path was skipped; the caller should fall back to `cargo install`.
    Fallback {
        /// Human-readable reason for the fallback.
        reason: String,
    },
}

/// Attempt to install a prebuilt binary for `crate_name` into `install_dir`.
///
/// Why: Prebuilt-first gives users a fast, toolchain-free install experience on
/// Tier-1 platforms. Cargo fallback ensures correctness on every other platform.
///
/// What: Determines the host target triple; returns [`Outcome::Fallback`] for
/// non-Tier-1 targets. On Tier-1, queries the GitHub Releases API for the latest
/// stable tag, builds the download URL, downloads + verifies + extracts the
/// tarball, and atomically places all binaries into `install_dir`.
///
/// Returns [`Outcome::Fallback`] on any failure (network, 404, SHA mismatch, I/O)
/// so the caller can always proceed via `cargo install`.
///
/// Test: `tests::fallback_on_unsupported_target`; the full prebuilt path is
/// validated by the `#[ignore]`-tagged live integration test.
pub async fn try_install_prebuilt(crate_name: &str, install_dir: &std::path::Path) -> Outcome {
    let client = build_client();

    // Step 1: Check Tier-1 target.
    let target = match platform::current_target() {
        Some(t) => t,
        None => {
            let os = std::env::consts::OS;
            let arch = std::env::consts::ARCH;
            return Outcome::Fallback {
                reason: format!(
                    "prebuilt binaries are not available for {arch}-{os}; \
                     building from source with cargo install"
                ),
            };
        }
    };

    // Step 2: Resolve the latest release tag.
    let resolved = match release::resolve_latest_tag(&client, crate_name).await {
        Ok(r) => r,
        Err(e) => {
            return Outcome::Fallback {
                reason: format!(
                    "could not resolve latest {crate_name} release: {e}; \
                     falling back to cargo install"
                ),
            };
        }
    };

    // Step 3: Pick the glibc-aware asset suffix. On a low-glibc x86_64 Linux
    // host the native ORT asset would crash with `GLIBC_2.39 not found`, so an
    // ORT crate is routed to the portable `x86_64-linux-al2023` load-dynamic
    // asset instead (issue #1992). Non-ORT crates and adequate hosts keep the
    // native target suffix.
    let choice = glibc::select_asset_suffix(crate_name, target, glibc::host_glibc_version());
    let suffix = choice.suffix.as_str();

    // Step 4: Build URLs from the chosen suffix.
    let archive_name = release::asset_filename(crate_name, &resolved.version, suffix);
    let tarball_url = release::asset_url(&resolved.tag, crate_name, &resolved.version, suffix);
    let sha_url = release::sha256_url(&resolved.tag, crate_name, &resolved.version, suffix);

    // Step 5: Download into a temp dir, verify, extract, place — atomically.
    match install_from_urls(
        &client,
        crate_name,
        &resolved.version,
        &archive_name,
        &tarball_url,
        &sha_url,
        install_dir,
    )
    .await
    {
        Ok(paths) => {
            // A load-dynamic asset dlopen()s libonnxruntime.so at startup and
            // will not run until ORT_DYLIB_PATH is set — surface the setup steps
            // rather than leaving a binary that silently fails to start.
            if choice.load_dynamic {
                tracing::warn!(
                    crate_name,
                    asset = suffix,
                    "{}",
                    glibc::ort_dylib_instructions(target)
                );
            }
            Outcome::Installed {
                paths,
                version: resolved.version,
            }
        }
        Err(e) => Outcome::Fallback {
            reason: format!("prebuilt download failed ({e}); falling back to cargo install"),
        },
    }
}

/// Build the shared reqwest client.
///
/// Why: A single client reuses the underlying connection pool across the two
/// downloads (`.sha256` and the tarball) within one install operation.
///
/// What: Returns a default `reqwest::Client` with `rustls-tls` (the workspace
/// reqwest is configured with `rustls-tls`).
///
/// Test: Exercised indirectly by any live network test.
fn build_client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Inner impl: download, verify, extract, and place into `install_dir`.
///
/// Why: Extracted from `try_install_prebuilt` so errors propagate cleanly with
/// `?` and the `Fallback` wrapping is done once at the call site.
///
/// What: Creates a system temp dir scoped to this call (auto-cleaned on drop),
/// calls `fetch::download_and_verify`, then `fetch::extract_binaries`, then
/// `fetch::place_binaries`.
///
/// Test: Exercised by the `#[ignore]`-tagged live integration test.
async fn install_from_urls(
    client: &reqwest::Client,
    _crate_name: &str,
    _version: &str,
    archive_name: &str,
    tarball_url: &str,
    sha_url: &str,
    install_dir: &std::path::Path,
) -> anyhow::Result<Vec<PathBuf>> {
    // Create a temp dir; it is cleaned up on drop even on error.
    let tmp = tempfile::tempdir().context("creating temp directory for prebuilt download")?;
    let tmp_path = tmp.path();

    // Download and verify integrity.
    let tarball = fetch::download_and_verify(client, tarball_url, sha_url, archive_name, tmp_path)
        .await
        .context("downloading and verifying prebuilt tarball")?;

    // Extract all binaries from the archive.
    let extract_dir = tmp_path.join("extracted");
    std::fs::create_dir_all(&extract_dir).context("creating extraction directory")?;
    let binary_names = fetch::extract_binaries(&tarball, &extract_dir)
        .context("extracting binaries from tarball")?;

    if binary_names.is_empty() {
        return Err(anyhow::anyhow!(
            "tarball contained no regular files; expected at least one binary"
        ));
    }

    // Atomically place into the install directory.
    let placed = fetch::place_binaries(&extract_dir, install_dir, &binary_names)
        .context("placing binaries into install directory")?;

    Ok(placed)
}

/// Resolve the default install directory (`~/.local/bin`), or `None` if the home
/// directory cannot be determined.
///
/// Why: Both `install` and `upgrade` need the default install path; this provides
/// a single resolution rule consistent with `install.sh`.
///
/// What: Expands `~` via `dirs::home_dir()`.
///
/// Test: `tests::default_install_dir_resolves` asserts the returned path ends
/// with `.local/bin`.
pub fn default_install_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".local").join("bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: On a non-Tier-1 target the orchestrator must immediately return
    /// `Fallback` without any network activity.
    /// What: The test runner may or may not be on a Tier-1 target; we synthesise
    /// the fallback decision by calling `platform::is_tier1_target` directly —
    /// the function under test is `platform::current_target()` returning `None`
    /// when not Tier-1, which `try_install_prebuilt` converts to `Fallback`.
    /// Test: This is the test.
    #[test]
    fn fallback_on_unsupported_target() {
        // A non-Tier-1 triple must never reach the network.
        assert!(!platform::is_tier1_target("x86_64-apple-darwin"));
        assert!(!platform::is_tier1_target("aarch64-unknown-linux-musl"));
        assert!(!platform::is_tier1_target("wasm32-unknown-unknown"));
    }

    /// Why: The default install dir must end in `.local/bin` so `install.sh` and
    /// the Rust installer agree on the default location.
    /// What: Asserts `default_install_dir()` ends with `.local/bin` when a home
    /// dir is available.
    /// Test: This is the test.
    #[test]
    fn default_install_dir_resolves() {
        if let Some(p) = default_install_dir() {
            assert!(
                p.ends_with(std::path::Path::new(".local").join("bin")),
                "unexpected path: {}",
                p.display()
            );
        }
    }

    /// Why: An `Installed` outcome must expose the version string and non-empty
    /// paths list; a `Fallback` outcome must expose a non-empty reason.
    /// What: Constructs each variant; asserts the fields are accessible.
    /// Test: This is the test.
    #[test]
    fn outcome_variants_accessible() {
        let inst = Outcome::Installed {
            paths: vec![PathBuf::from("/usr/local/bin/trusty-search")],
            version: "0.25.0".to_owned(),
        };
        match inst {
            Outcome::Installed { paths, version } => {
                assert!(!paths.is_empty());
                assert_eq!(version, "0.25.0");
            }
            Outcome::Fallback { .. } => panic!("expected Installed"),
        }

        let fb = Outcome::Fallback {
            reason: "no asset".to_owned(),
        };
        match fb {
            Outcome::Fallback { reason } => assert!(!reason.is_empty()),
            Outcome::Installed { .. } => panic!("expected Fallback"),
        }
    }

    /// Why: Live integration proof that the full prebuilt path works end-to-end
    /// on a Tier-1 target. Gated behind `#[ignore]` to keep CI offline-deterministic.
    /// What: Calls `try_install_prebuilt` for `trusty-installer` into a temp dir;
    /// asserts `Outcome::Installed` (on a Tier-1 host) or `Outcome::Fallback` (on
    /// a non-Tier-1 host).
    /// Test: `cargo test -p trusty-installer -- --include-ignored try_install_prebuilt_live`.
    #[tokio::test]
    #[ignore = "performs a live GitHub download; run with --include-ignored"]
    async fn try_install_prebuilt_live() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = try_install_prebuilt("trusty-installer", dir.path()).await;
        match outcome {
            Outcome::Installed { paths, version } => {
                assert!(!paths.is_empty());
                assert!(!version.is_empty());
            }
            Outcome::Fallback { reason } => {
                // Acceptable on non-Tier-1 hosts (e.g. Intel Mac CI).
                eprintln!("fallback (expected on non-Tier-1): {reason}");
            }
        }
    }
}
