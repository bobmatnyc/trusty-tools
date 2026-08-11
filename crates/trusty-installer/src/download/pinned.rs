//! Pinned-version, fail-closed prebuilt install entry point (#5491).
//!
//! Why: [`super::try_install_prebuilt`] resolves `latest` and returns
//! `Outcome::Fallback` on any failure, so its caller silently builds *whatever
//! version happens to be current* from source. A consumer that pins exact
//! versions — `trusty-audit` fetching `tga`, `trusty-analyze` and
//! `trusty-review` for a reproducible audit — cannot use that: getting a
//! locally-built something-else while believing a pin held is the same
//! degraded-success defect class as a stale tool exiting 0.
//!
//! What: [`install_pinned_set`] takes exact versions, verifies the downloaded
//! artifact against both the published checksum and (optionally) a
//! caller-supplied digest, verifies the binary itself reports the pinned
//! version, and installs ALL tools or NONE. Every failure is a typed
//! [`PinnedError`] naming what was pinned, what arrived, and that nothing was
//! installed. There is no fallback path — `cargo install` is never reachable
//! from this module.
//!
//! This is ADDITIVE: `try_install_prebuilt`'s latest-plus-fallback semantics are
//! untouched, because its existing callers depend on them.
//!
//! Test: `tests` drives the whole pipeline against a loopback fixture server
//! (the crate's established stub-server pattern — no live network, no new
//! dev-dependency); each fail-closed arm has its own test.

use std::path::{Path, PathBuf};

use crate::download::{fetch, glibc, platform, release};

/// A tool to install at an exact pinned version.
///
/// Why: The consumer pins several tools at once, so the pin (crate, version,
/// which binary proves it, optional digest) has to travel as one value rather
/// than as four positional arguments repeated per tool.
///
/// What: `crate_name` selects the release tag and asset; `binary` names the
/// executable whose `--version` output proves the pin held (an archive may
/// contain several); `sha256` optionally pins the artifact digest itself.
///
/// Test: `tests::pinned_tool_defaults_binary_to_crate_name`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PinnedTool {
    /// Cargo crate name, e.g. `trusty-review`. Selects the `<crate>-v<version>` tag.
    pub crate_name: String,
    /// Exact semver to install, e.g. `0.9.1`. Never a range, never `latest`.
    pub version: String,
    /// Executable that must report `version`. Defaults to `crate_name`.
    pub binary: String,
    /// Optional caller-pinned SHA-256 of the release tarball, lowercase hex.
    pub sha256: Option<String>,
}

impl PinnedTool {
    /// A pin for `crate_name` at exactly `version`, proved by the binary of the
    /// same name.
    pub fn new(crate_name: impl Into<String>, version: impl Into<String>) -> Self {
        let crate_name = crate_name.into();
        Self {
            binary: crate_name.clone(),
            crate_name,
            version: version.into(),
            sha256: None,
        }
    }

    /// Override the executable whose `--version` proves the pin (e.g. `tga`'s
    /// crate and binary names agree, but a crate shipping several binaries may
    /// need a specific one).
    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Additionally pin the artifact's SHA-256, so a re-uploaded asset at the
    /// same tag fails closed instead of installing.
    #[must_use]
    pub fn with_sha256(mut self, sha256: impl Into<String>) -> Self {
        self.sha256 = Some(sha256.into().to_lowercase());
        self
    }
}

/// What a successful pinned install placed on disk.
///
/// Why: The caller records exactly which binary satisfied which pin, for the
/// audit manifest.
///
/// What: `version` is the pin, already proved equal to what `binary_path`
/// reports; `paths` lists every file placed from the archive.
///
/// Test: `tests::pinned_install_places_and_reports_the_pinned_version`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PinnedInstall {
    /// The crate that was installed.
    pub crate_name: String,
    /// The exact version installed — equal to the pin, verified.
    pub version: String,
    /// The executable that reported `version`.
    pub binary_path: PathBuf,
    /// Every file placed from the archive.
    pub paths: Vec<PathBuf>,
}

/// Why a pinned install failed. Every variant means NOTHING was installed.
///
/// Why: A GUI has to render these, and an operator has to act on them, so the
/// pin and the thing that arrived must both survive as structured fields rather
/// than being flattened into one string. The `Display` text states explicitly
/// that nothing was installed, because the defect this type exists to prevent is
/// a caller treating a failure as a degraded success.
///
/// What: One variant per failure the pipeline can distinguish.
/// `#[non_exhaustive]` so later variants stay a non-breaking addition.
///
/// Test: One test per variant in `tests`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PinnedError {
    /// No prebuilt is published for this host. Never falls back to a source build.
    #[error(
        "no prebuilt binary is published for {arch}-{os}, so {crate_name} {version} \
         could not be installed; nothing was installed (the pinned install path \
         never falls back to `cargo install`)"
    )]
    UnsupportedTarget {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned.
        version: String,
        /// Host OS.
        os: String,
        /// Host architecture.
        arch: String,
    },

    /// The pinned version was never published as a stable release.
    #[error(
        "{crate_name} {version} is not a published stable release, so it could not \
         be installed; nothing was installed. Published stable versions: {}",
        if available.is_empty() { "none".to_owned() } else { available.join(", ") }
    )]
    VersionNotPublished {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned but does not exist.
        version: String,
        /// Every stable version that IS published, ascending.
        available: Vec<String>,
    },

    /// The release list could not be reached or parsed.
    #[error(
        "could not determine whether {crate_name} {version} is published; \
         nothing was installed: {source}"
    )]
    ReleaseLookupFailed {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned.
        version: String,
        /// Underlying transport or parse failure.
        #[source]
        source: anyhow::Error,
    },

    /// The tag exists but its asset could not be downloaded.
    #[error(
        "the release artifact for {crate_name} {version} could not be downloaded \
         from {url}; nothing was installed: {source}"
    )]
    ArtifactUnavailable {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned.
        version: String,
        /// The asset URL that failed.
        url: String,
        /// Underlying transport failure.
        #[source]
        source: anyhow::Error,
    },

    /// The artifact does not match the checksum GitHub published beside it.
    #[error(
        "checksum mismatch for {crate_name} {version}: the published checksum is \
         {expected}, but the downloaded artifact hashes to {actual}; \
         nothing was installed"
    )]
    ChecksumMismatch {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned.
        version: String,
        /// Digest from the published `.sha256` sidecar.
        expected: String,
        /// Digest actually computed over the downloaded bytes.
        actual: String,
    },

    /// The artifact does not match the digest the CALLER pinned.
    #[error(
        "the artifact for {crate_name} {version} does not match the pinned \
         checksum: pinned {pinned}, downloaded artifact hashes to {actual}; \
         nothing was installed"
    )]
    PinnedChecksumMismatch {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned.
        version: String,
        /// The digest the caller pinned.
        pinned: String,
        /// Digest actually computed over the downloaded bytes.
        actual: String,
    },

    /// The downloaded binary does not report the pinned version.
    #[error(
        "version mismatch for {crate_name}: pinned {pinned}, but the downloaded \
         binary reports {reported}; nothing was installed"
    )]
    VersionMismatch {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned.
        pinned: String,
        /// What the downloaded binary actually reported.
        reported: String,
    },

    /// The archive did not contain the binary that must prove the pin.
    #[error(
        "the artifact for {crate_name} {version} does not contain the expected \
         binary `{binary}` (found: {}); nothing was installed",
        if found.is_empty() { "nothing".to_owned() } else { found.join(", ") }
    )]
    BinaryMissing {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned.
        version: String,
        /// The binary expected to prove the pin.
        binary: String,
        /// What the archive actually contained.
        found: Vec<String>,
    },

    /// A filesystem step failed.
    #[error("installing {crate_name} {version} failed; nothing was installed: {source}")]
    Io {
        /// The crate that was pinned.
        crate_name: String,
        /// The version that was pinned.
        version: String,
        /// Underlying failure.
        #[source]
        source: anyhow::Error,
    },
}

/// Where the pinned path fetches from. Crate-internal so tests can substitute a
/// loopback fixture without a live network call.
pub(crate) struct Endpoints<'a> {
    /// GitHub Releases API URL.
    pub releases_url: &'a str,
    /// Base URL that release assets hang off.
    pub download_base: &'a str,
}

impl Default for Endpoints<'_> {
    fn default() -> Self {
        Self {
            releases_url: release::RELEASES_API,
            download_base: release::RELEASE_DL_BASE,
        }
    }
}

/// Install ONE tool at its exact pinned version, or fail closed.
///
/// Why: The single-tool shape of [`install_pinned_set`], for a caller fetching
/// one pinned tool.
///
/// # Postconditions
/// On `Ok`, `install_dir` holds the pinned version and the installed binary has
/// been observed reporting it. On `Err`, nothing was placed in `install_dir`.
///
/// What: Delegates to [`install_pinned_set`] with a one-element slice.
///
/// Test: `tests::pinned_install_places_and_reports_the_pinned_version`.
pub async fn install_pinned(
    client: &reqwest::Client,
    tool: &PinnedTool,
    install_dir: &Path,
) -> Result<PinnedInstall, PinnedError> {
    let mut installed = install_pinned_set(client, std::slice::from_ref(tool), install_dir).await?;
    // Postcondition of install_pinned_set: exactly one install per input tool.
    installed.pop().ok_or_else(|| PinnedError::Io {
        crate_name: tool.crate_name.clone(),
        version: tool.version.clone(),
        source: anyhow::anyhow!("internal: no install reported for a single pinned tool"),
    })
}

/// Install every tool at its exact pinned version, or install NONE.
///
/// Why: `trusty-audit` needs a tool set that is all present at known versions or
/// absent — a half-installed set would let an audit run with one tool at the
/// pinned version and another at whatever was already on disk, which is exactly
/// the mixed-version result the pin exists to prevent.
///
/// # Preconditions
/// Each [`PinnedTool::version`] is an exact semver. A range or `latest` is
/// rejected rather than resolved.
///
/// # Postconditions
/// On `Ok`, every tool was downloaded, checksum-verified, and observed reporting
/// its pinned version BEFORE any file entered `install_dir`; the returned vector
/// has one entry per input tool, in order. On `Err`, `install_dir` is untouched
/// by this call — no tool from the set was placed, including tools that verified
/// successfully.
///
/// What: Stages every tool into a temp dir (resolve exact tag → download →
/// verify checksums → extract → probe `--version`), and only once ALL tools pass
/// does it place any binary. `cargo install` is never invoked on any path.
///
/// Test: `tests::a_set_installs_every_tool_when_all_verify`,
/// `tests::a_set_installs_nothing_when_any_tool_fails` (the all-or-nothing
/// guarantee), plus one test per [`PinnedError`] arm.
pub async fn install_pinned_set(
    client: &reqwest::Client,
    tools: &[PinnedTool],
    install_dir: &Path,
) -> Result<Vec<PinnedInstall>, PinnedError> {
    install_pinned_set_at(client, &Endpoints::default(), tools, install_dir).await
}

/// [`install_pinned_set`], against caller-supplied endpoints.
///
/// Why: The seam that lets every fail-closed arm be proved offline against a
/// loopback fixture — the same injection `release::resolve_latest_tag_from_url`
/// already established.
///
/// What: The real implementation; [`install_pinned_set`] is this with the
/// production endpoints.
///
/// Test: All of `tests`.
pub(crate) async fn install_pinned_set_at(
    client: &reqwest::Client,
    endpoints: &Endpoints<'_>,
    tools: &[PinnedTool],
    install_dir: &Path,
) -> Result<Vec<PinnedInstall>, PinnedError> {
    // One staging root for the whole set; dropped (and cleaned) on every path.
    let staging = tempfile::tempdir().map_err(|e| PinnedError::Io {
        crate_name: tools
            .first()
            .map(|t| t.crate_name.clone())
            .unwrap_or_default(),
        version: tools.first().map(|t| t.version.clone()).unwrap_or_default(),
        source: anyhow::Error::new(e).context("creating staging directory"),
    })?;

    // Phase 1 — stage and verify EVERY tool. `?` here is what makes the set
    // all-or-nothing: the first failure returns before phase 2 places anything.
    let mut staged = Vec::with_capacity(tools.len());
    for (idx, tool) in tools.iter().enumerate() {
        let dir = staging.path().join(format!("{idx}-{}", tool.crate_name));
        staged.push(stage_one(client, endpoints, tool, &dir).await?);
    }

    // Phase 2 — every tool verified; place them.
    let mut installed = Vec::with_capacity(staged.len());
    for s in staged {
        installed.push(place_staged(&s, install_dir)?);
    }
    Ok(installed)
}

/// A tool downloaded, verified, and proved to report its pinned version, sitting
/// in a staging dir and not yet installed.
struct Staged {
    tool: PinnedTool,
    extract_dir: PathBuf,
    binary_names: Vec<String>,
}

/// Resolve, download, verify, extract, and version-probe one pinned tool into
/// `dir` — without touching the install directory.
///
/// Why: Every fail-closed check has to happen before ANY file is placed;
/// separating staging from placement is what makes "nothing was installed" true
/// rather than aspirational.
///
/// # Postconditions
/// On `Ok`, `dir` holds an extracted archive whose `tool.binary` was observed
/// reporting `tool.version`. On `Err`, nothing outside `dir` was modified.
///
/// What: Runs the five checks in order — Tier-1 target, exact tag, artifact
/// download, checksum(s), binary-reported version.
///
/// Test: Each failure arm has a test in `tests`.
async fn stage_one(
    client: &reqwest::Client,
    endpoints: &Endpoints<'_>,
    tool: &PinnedTool,
    dir: &Path,
) -> Result<Staged, PinnedError> {
    let (name, version) = (tool.crate_name.as_str(), tool.version.as_str());

    // Check 1 — a prebuilt exists for this host. Unlike the latest path, an
    // unsupported target is terminal, not a cargo fallback.
    let target = platform::current_target().ok_or_else(|| PinnedError::UnsupportedTarget {
        crate_name: name.to_owned(),
        version: version.to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
    })?;

    // Check 2 — the EXACT pinned version is published. #5491: never `latest`.
    let resolved =
        release::resolve_pinned_tag_from_url(client, endpoints.releases_url, name, version)
            .await
            .map_err(|e| match e {
                release::ResolveError::NotPublished { available } => {
                    PinnedError::VersionNotPublished {
                        crate_name: name.to_owned(),
                        version: version.to_owned(),
                        available,
                    }
                }
                release::ResolveError::Fetch(source) => PinnedError::ReleaseLookupFailed {
                    crate_name: name.to_owned(),
                    version: version.to_owned(),
                    source,
                },
            })?;

    let suffix = glibc::select_asset_suffix(name, target, glibc::host_glibc_version()).suffix;
    let archive_name = release::asset_filename(name, &resolved.version, &suffix);
    let tarball_url = release::asset_url_at_base(
        endpoints.download_base,
        &resolved.tag,
        name,
        &resolved.version,
        &suffix,
    );
    let sha_url = format!("{tarball_url}.sha256");

    std::fs::create_dir_all(dir).map_err(|e| PinnedError::Io {
        crate_name: name.to_owned(),
        version: version.to_owned(),
        source: anyhow::Error::new(e).context("creating staging directory"),
    })?;

    // Check 3 — the artifact and its checksum sidecar are downloadable.
    let sha_path = dir.join(format!("{archive_name}.sha256"));
    let tar_path = dir.join(&archive_name);
    for (url, dest) in [(&sha_url, &sha_path), (&tarball_url, &tar_path)] {
        fetch::download_to_file(client, url, dest)
            .await
            .map_err(|e| PinnedError::ArtifactUnavailable {
                crate_name: name.to_owned(),
                version: version.to_owned(),
                url: url.clone(),
                source: e,
            })?;
    }

    // Check 4 — the bytes match the published checksum, and the caller's pin.
    let sidecar = std::fs::read_to_string(&sha_path).map_err(|e| PinnedError::Io {
        crate_name: name.to_owned(),
        version: version.to_owned(),
        source: anyhow::Error::new(e).context("reading the published checksum"),
    })?;
    let expected = fetch::parse_sha256_line(&sidecar).map_err(|e| PinnedError::Io {
        crate_name: name.to_owned(),
        version: version.to_owned(),
        source: e.context("parsing the published checksum"),
    })?;
    let actual = fetch::sha256_file(&tar_path).map_err(|e| PinnedError::Io {
        crate_name: name.to_owned(),
        version: version.to_owned(),
        source: e.context("hashing the downloaded artifact"),
    })?;
    if actual != expected {
        return Err(PinnedError::ChecksumMismatch {
            crate_name: name.to_owned(),
            version: version.to_owned(),
            expected,
            actual,
        });
    }
    // A caller-supplied digest pins the BYTES, so a re-uploaded asset at the
    // same tag (whose sidecar would agree with itself) still fails closed.
    if let Some(pinned) = tool.sha256.as_deref() {
        if actual != pinned {
            return Err(PinnedError::PinnedChecksumMismatch {
                crate_name: name.to_owned(),
                version: version.to_owned(),
                pinned: pinned.to_owned(),
                actual,
            });
        }
    }

    let extract_dir = dir.join("extracted");
    std::fs::create_dir_all(&extract_dir).map_err(|e| PinnedError::Io {
        crate_name: name.to_owned(),
        version: version.to_owned(),
        source: anyhow::Error::new(e).context("creating the extraction directory"),
    })?;
    let binary_names =
        fetch::extract_binaries(&tar_path, &extract_dir).map_err(|e| PinnedError::Io {
            crate_name: name.to_owned(),
            version: version.to_owned(),
            source: e.context("extracting the artifact"),
        })?;

    // Check 5 — the binary itself reports the pinned version. This is what
    // catches a mis-tagged or mis-built asset that passed every URL-level check.
    // Probed in the STAGING dir, so a mismatch installs nothing.
    let staged_binary = extract_dir.join(&tool.binary);
    if !staged_binary.exists() {
        return Err(PinnedError::BinaryMissing {
            crate_name: name.to_owned(),
            version: version.to_owned(),
            binary: tool.binary.clone(),
            found: binary_names,
        });
    }
    let reported_line = trusty_common::update::verify_installed_binary_at_path(&staged_binary)
        .await
        .map_err(|e| PinnedError::VersionMismatch {
            crate_name: name.to_owned(),
            pinned: version.to_owned(),
            reported: format!("nothing usable (`--version` failed: {e})"),
        })?;
    let reported = crate::commands::update_engine::extract_version_from_line(&reported_line);
    if reported.as_deref() != Some(version) {
        return Err(PinnedError::VersionMismatch {
            crate_name: name.to_owned(),
            pinned: version.to_owned(),
            reported: reported.unwrap_or_else(|| format!("{reported_line:?}")),
        });
    }

    Ok(Staged {
        tool: tool.clone(),
        extract_dir,
        binary_names,
    })
}

/// Place an already-verified staged tool into the install directory.
///
/// Why: Placement is deliberately the last thing that happens and does no
/// verification of its own — every gate ran in [`stage_one`].
///
/// What: Delegates to `fetch::place_binaries` (atomic temp-file + rename).
///
/// Test: `tests::pinned_install_places_and_reports_the_pinned_version`.
fn place_staged(staged: &Staged, install_dir: &Path) -> Result<PinnedInstall, PinnedError> {
    let paths = fetch::place_binaries(&staged.extract_dir, install_dir, &staged.binary_names)
        .map_err(|e| PinnedError::Io {
            crate_name: staged.tool.crate_name.clone(),
            version: staged.tool.version.clone(),
            source: e.context("placing binaries into the install directory"),
        })?;
    Ok(PinnedInstall {
        crate_name: staged.tool.crate_name.clone(),
        version: staged.tool.version.clone(),
        binary_path: install_dir.join(&staged.tool.binary),
        paths,
    })
}

// The fixture-server suite lives in its own file: it is ~550 lines, and an
// `#[cfg(all(test, …))]` module body counts toward the 500-SLOC production cap
// (only the bare `#[cfg(test)] mod x { … }` shape is excluded, per #5153).
// `pinned/tests.rs` is classified test-file by basename, so it gets the 3000 cap.
// `unix` because the fixtures are executable /bin/sh scripts; every target this
// crate publishes prebuilts for is unix.
#[cfg(all(test, unix))]
mod tests;
