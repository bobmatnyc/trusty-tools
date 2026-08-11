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
/// Test: `tests::pinned_set_installs_all_tools`,
/// `tests::pinned_set_places_nothing_when_one_tool_fails` (the all-or-nothing
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Routes a fixture server answers: path → (status code, body bytes).
    type Routes = HashMap<String, (u16, Vec<u8>)>;

    /// Serve fixed responses per PATH on loopback, for as long as the test runs.
    ///
    /// Why: Every fail-closed arm must be provable WITHOUT a live network call.
    /// The crate's existing `commands::test_support` stubs answer a fixed
    /// SEQUENCE with JSON bodies; this pipeline issues three requests and one of
    /// them is a binary tarball, so it needs path routing and byte bodies. Same
    /// raw-`TcpListener` vehicle, no new dev-dependency.
    ///
    /// What: Binds an ephemeral loopback port, spawns an accept loop, and
    /// answers each request from `routes` (404 for anything unrouted). Returns
    /// the base URL.
    async fn serve_fixture(routes: Routes) -> String {
        let routes = Arc::new(routes);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let routes = Arc::clone(&routes);
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Drain the header block before replying — the split-request
                    // flake shape `test_support::serve_fixed` documents.
                    let mut acc = Vec::with_capacity(2048);
                    let mut chunk = [0u8; 2048];
                    loop {
                        match sock.read(&mut chunk).await {
                            Ok(0) => break,
                            Ok(n) => {
                                acc.extend_from_slice(&chunk[..n]);
                                if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => return,
                        }
                    }
                    let head = String::from_utf8_lossy(&acc);
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_owned();
                    let (status, body) = match routes.get(&path) {
                        Some((s, b)) => (*s, b.clone()),
                        None => (404, b"not found".to_vec()),
                    };
                    let reason = if status == 200 { "OK" } else { "Not Found" };
                    let resp = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// A `.tar.gz` holding one executable that prints `version_line`.
    ///
    /// Why: Check 5 probes the staged binary with `--version`, so the fixture
    /// must be genuinely executable — a byte blob would prove nothing about the
    /// version gate.
    fn fake_tarball(binary: &str, version_line: &str) -> Vec<u8> {
        use flate2::{write::GzEncoder, Compression};
        let script = format!("#!/bin/sh\necho '{version_line}'\n");
        let data = script.as_bytes();
        let enc = GzEncoder::new(Vec::new(), Compression::fast());
        let mut ar = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        ar.append_data(&mut header, binary, data).unwrap();
        ar.into_inner().unwrap().finish().unwrap()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }

    /// The asset suffix the code under test will pick for this host.
    fn suffix_for(crate_name: &str, target: &str) -> String {
        glibc::select_asset_suffix(crate_name, target, glibc::host_glibc_version()).suffix
    }

    /// How a fixture release deviates from a valid one.
    #[derive(Clone, Copy, PartialEq)]
    enum Flaw {
        /// A correct, installable release.
        None,
        /// Tag published, but the asset 404s.
        AssetMissing,
        /// Asset served, but its published checksum names other bytes.
        BadChecksum,
        /// Asset and checksum fine, but the binary reports another version.
        WrongBinaryVersion,
    }

    /// Builds the releases JSON and asset routes for a set of fixture releases.
    struct Fixture {
        target: &'static str,
        tags: Vec<String>,
        routes: Routes,
    }

    impl Fixture {
        fn new(target: &'static str) -> Self {
            Self {
                target,
                tags: Vec::new(),
                routes: HashMap::new(),
            }
        }

        /// Publish `crate_name` at `version`, deviating per `flaw`.
        fn publish(mut self, crate_name: &str, version: &str, flaw: Flaw) -> Self {
            self.tags.push(format!("{crate_name}-v{version}"));
            if flaw == Flaw::AssetMissing {
                return self;
            }
            let reported = if flaw == Flaw::WrongBinaryVersion {
                "9.9.9"
            } else {
                version
            };
            let tarball = fake_tarball(crate_name, &format!("{crate_name} {reported}"));
            let digest = if flaw == Flaw::BadChecksum {
                sha256_hex(b"entirely different bytes")
            } else {
                sha256_hex(&tarball)
            };
            let file =
                release::asset_filename(crate_name, version, &suffix_for(crate_name, self.target));
            let key = format!("/dl/{crate_name}-v{version}/{file}");
            self.routes.insert(
                format!("{key}.sha256"),
                (200, format!("{digest}  {file}\n").into_bytes()),
            );
            self.routes.insert(key, (200, tarball));
            self
        }

        /// The SHA-256 of the artifact published for `crate_name` at `version`.
        fn digest_of(&self, crate_name: &str, version: &str) -> String {
            let file =
                release::asset_filename(crate_name, version, &suffix_for(crate_name, self.target));
            let key = format!("/dl/{crate_name}-v{version}/{file}");
            sha256_hex(&self.routes.get(&key).expect("published asset").1)
        }

        async fn start(mut self) -> String {
            let json = format!(
                "[{}]",
                self.tags
                    .iter()
                    .map(|t| format!(r#"{{"tag_name":"{t}","prerelease":false}}"#))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            self.routes
                .insert("/releases".to_owned(), (200, json.into_bytes()));
            serve_fixture(self.routes).await
        }
    }

    /// Drive the pinned entry point against a fixture, into `install_dir`.
    async fn run(
        base: &str,
        tools: &[PinnedTool],
        install_dir: &Path,
    ) -> Result<Vec<PinnedInstall>, PinnedError> {
        let releases_url = format!("{base}/releases");
        let download_base = format!("{base}/dl");
        let endpoints = Endpoints {
            releases_url: &releases_url,
            download_base: &download_base,
        };
        install_pinned_set_at(&reqwest::Client::new(), &endpoints, tools, install_dir).await
    }

    /// Assert nothing landed in the install directory — the "nothing was
    /// installed" half of every fail-closed claim.
    fn assert_nothing_installed(install_dir: &Path) {
        let found: Vec<String> = std::fs::read_dir(install_dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            found.is_empty(),
            "fail-closed violated: {} contains {found:?}",
            install_dir.display()
        );
    }

    /// The host target, or `None` when this host publishes no prebuilts.
    fn tier1() -> Option<&'static str> {
        platform::current_target()
    }

    /// Why: A pin is normally proved by the binary of the same name; requiring
    /// callers to repeat it would be noise.
    /// What: Asserts the default and the override.
    /// Test: This is the test.
    #[test]
    fn pinned_tool_defaults_binary_to_crate_name() {
        let t = PinnedTool::new("trusty-review", "0.9.1");
        assert_eq!(t.binary, "trusty-review");
        assert_eq!(t.version, "0.9.1");
        assert!(t.sha256.is_none());
        assert_eq!(t.with_binary("tr").binary, "tr");
    }

    /// Why: The happy path must actually install, or the fail-closed tests below
    /// could pass for the trivial reason that nothing ever installs.
    /// What: Publishes a valid release, pins it, asserts the binary lands and the
    /// reported version equals the pin.
    /// Test: This is the test.
    #[tokio::test]
    async fn pinned_install_places_and_reports_the_pinned_version() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("demo-tool", "1.2.3", Flaw::None)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();

        let installed = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
            .await
            .expect("a valid pinned release must install");

        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].version, "1.2.3");
        assert!(dir.path().join("demo-tool").exists());
    }

    /// Why: THE headline guarantee. `try_install_prebuilt` resolves `latest`, so
    /// pinning a version that was never published would silently install the
    /// newest one. Here a newer release EXISTS and must not be substituted.
    /// What: Publishes 1.2.3 only; pins 1.0.0. Asserts `VersionNotPublished`
    /// listing what is published, and an empty install dir.
    /// Test: This is the test.
    #[tokio::test]
    async fn fails_closed_when_the_pinned_version_was_never_published() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("demo-tool", "1.2.3", Flaw::None)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();

        let err = run(&base, &[PinnedTool::new("demo-tool", "1.0.0")], dir.path())
            .await
            .expect_err("an unpublished pin must fail closed, never resolve to latest");

        match &err {
            PinnedError::VersionNotPublished {
                version, available, ..
            } => {
                assert_eq!(version, "1.0.0");
                assert_eq!(available, &vec!["1.2.3".to_owned()]);
            }
            other => panic!("expected VersionNotPublished, got {other:?}"),
        }
        assert!(err.to_string().contains("nothing was installed"));
        assert_nothing_installed(dir.path());
    }

    /// Why: A tampered or corrupted artifact must never be installed, and must
    /// never degrade into a source build.
    /// What: Publishes an asset whose `.sha256` names other bytes; asserts
    /// `ChecksumMismatch` carrying both digests, and an empty install dir.
    /// Test: This is the test.
    #[tokio::test]
    async fn fails_closed_on_a_checksum_mismatch() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("demo-tool", "1.2.3", Flaw::BadChecksum)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();

        let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
            .await
            .expect_err("a checksum mismatch must fail closed");

        match &err {
            PinnedError::ChecksumMismatch {
                expected, actual, ..
            } => {
                assert_ne!(expected, actual, "the test fixture must actually disagree");
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
        assert!(err.to_string().contains("nothing was installed"));
        assert_nothing_installed(dir.path());
    }

    /// Why: A caller-pinned digest is what makes a re-uploaded asset at the same
    /// tag detectable — the published sidecar would agree with the new bytes.
    /// What: Publishes a valid release, pins a DIFFERENT digest; asserts
    /// `PinnedChecksumMismatch` and an empty install dir.
    /// Test: This is the test.
    #[tokio::test]
    async fn fails_closed_when_the_artifact_does_not_match_a_caller_pinned_digest() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("demo-tool", "1.2.3", Flaw::None)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let wrong = "0".repeat(64);

        let err = run(
            &base,
            &[PinnedTool::new("demo-tool", "1.2.3").with_sha256(&wrong)],
            dir.path(),
        )
        .await
        .expect_err("a caller-pinned digest mismatch must fail closed");

        match &err {
            PinnedError::PinnedChecksumMismatch { pinned, actual, .. } => {
                assert_eq!(pinned, &wrong);
                assert_ne!(actual, &wrong);
            }
            other => panic!("expected PinnedChecksumMismatch, got {other:?}"),
        }
        assert_nothing_installed(dir.path());
    }

    /// Why: The matching-digest case must still install, or the test above would
    /// pass because caller digests are simply always rejected.
    /// What: Pins the artifact's real digest; asserts it installs.
    /// Test: This is the test.
    #[tokio::test]
    async fn a_matching_caller_pinned_digest_installs() {
        let Some(target) = tier1() else { return };
        let fixture = Fixture::new(target).publish("demo-tool", "1.2.3", Flaw::None);
        let digest = fixture.digest_of("demo-tool", "1.2.3");
        let base = fixture.start().await;
        let dir = tempfile::tempdir().unwrap();

        let installed = run(
            &base,
            &[PinnedTool::new("demo-tool", "1.2.3").with_sha256(&digest)],
            dir.path(),
        )
        .await
        .expect("a matching pinned digest must install");

        assert_eq!(installed[0].version, "1.2.3");
    }

    /// Why: A published tag whose asset is missing (a release leg that failed, or
    /// a platform never built) must be terminal — this is precisely where
    /// `try_install_prebuilt` 404s and silently builds from source instead.
    /// What: Publishes the tag with no asset routes; asserts `ArtifactUnavailable`
    /// naming the URL, and an empty install dir.
    /// Test: This is the test.
    #[tokio::test]
    async fn fails_closed_when_the_artifact_is_unavailable() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("demo-tool", "1.2.3", Flaw::AssetMissing)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();

        let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
            .await
            .expect_err("a missing artifact must fail closed, never fall back");

        match &err {
            PinnedError::ArtifactUnavailable { url, .. } => {
                assert!(url.contains("demo-tool-v1.2.3"), "unexpected url: {url}");
            }
            other => panic!("expected ArtifactUnavailable, got {other:?}"),
        }
        assert!(err.to_string().contains("nothing was installed"));
        assert_nothing_installed(dir.path());
    }

    /// Why: Every URL-level check can pass while the artifact still contains the
    /// wrong build — a mis-tagged release. Only probing the binary catches it,
    /// and probing it in staging is what keeps the failure from installing.
    /// What: Serves a correctly-named, correctly-checksummed asset whose binary
    /// reports 9.9.9; asserts `VersionMismatch` and an empty install dir.
    /// Test: This is the test.
    #[tokio::test]
    async fn fails_closed_when_the_downloaded_binary_reports_another_version() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("demo-tool", "1.2.3", Flaw::WrongBinaryVersion)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();

        let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
            .await
            .expect_err("a binary reporting another version must fail closed");

        match &err {
            PinnedError::VersionMismatch {
                pinned, reported, ..
            } => {
                assert_eq!(pinned, "1.2.3");
                assert_eq!(reported, "9.9.9");
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
        assert!(err.to_string().contains("nothing was installed"));
        assert_nothing_installed(dir.path());
    }

    /// Why: The archive must contain the binary that proves the pin; otherwise
    /// the version gate would be silently skipped.
    /// What: Pins a binary name absent from the archive; asserts `BinaryMissing`
    /// and an empty install dir.
    /// Test: This is the test.
    #[tokio::test]
    async fn fails_closed_when_the_archive_lacks_the_binary_that_proves_the_pin() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("demo-tool", "1.2.3", Flaw::None)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();

        let err = run(
            &base,
            &[PinnedTool::new("demo-tool", "1.2.3").with_binary("not-in-archive")],
            dir.path(),
        )
        .await
        .expect_err("a missing binary must fail closed");

        assert!(
            matches!(err, PinnedError::BinaryMissing { .. }),
            "got {err:?}"
        );
        assert_nothing_installed(dir.path());
    }

    /// Why: The multi-tool guarantee `trusty-audit` depends on — a set that
    /// half-installs would let an audit run with one pinned tool and one stale
    /// one, which is the mixed-version result pinning exists to prevent.
    /// What: Two tools, the FIRST fully valid and the second's asset missing.
    /// Asserts the error is the second tool's AND the first tool's binary was
    /// never placed, despite having verified successfully.
    /// Test: This is the test.
    #[tokio::test]
    async fn a_set_installs_nothing_when_any_tool_fails() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("good-tool", "1.0.0", Flaw::None)
            .publish("bad-tool", "2.0.0", Flaw::AssetMissing)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();

        let err = run(
            &base,
            &[
                PinnedTool::new("good-tool", "1.0.0"),
                PinnedTool::new("bad-tool", "2.0.0"),
            ],
            dir.path(),
        )
        .await
        .expect_err("one failing tool must fail the whole set");

        assert!(
            matches!(err, PinnedError::ArtifactUnavailable { .. }),
            "got {err:?}"
        );
        assert!(
            !dir.path().join("good-tool").exists(),
            "all-or-nothing violated: the verified tool was installed anyway"
        );
        assert_nothing_installed(dir.path());
    }

    /// Why: The set path must install every tool when all of them verify — the
    /// positive half of all-or-nothing.
    /// What: Two valid tools; asserts both land at their pinned versions.
    /// Test: This is the test.
    #[tokio::test]
    async fn a_set_installs_every_tool_when_all_verify() {
        let Some(target) = tier1() else { return };
        let base = Fixture::new(target)
            .publish("tool-a", "1.0.0", Flaw::None)
            .publish("tool-b", "2.0.0", Flaw::None)
            .start()
            .await;
        let dir = tempfile::tempdir().unwrap();

        let installed = run(
            &base,
            &[
                PinnedTool::new("tool-a", "1.0.0"),
                PinnedTool::new("tool-b", "2.0.0"),
            ],
            dir.path(),
        )
        .await
        .expect("all valid tools must install");

        assert_eq!(installed.len(), 2);
        assert_eq!(installed[0].version, "1.0.0");
        assert_eq!(installed[1].version, "2.0.0");
        assert!(dir.path().join("tool-a").exists());
        assert!(dir.path().join("tool-b").exists());
    }

    /// Why: An unreachable release list must not be mistaken for "not published",
    /// because the two call for different operator action.
    /// What: Points the resolver at a port that refuses; asserts
    /// `ReleaseLookupFailed`, not `VersionNotPublished`.
    /// Test: This is the test.
    #[tokio::test]
    async fn fails_closed_when_the_release_list_is_unreachable() {
        if tier1().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // The crate's existing guaranteed-to-refuse loopback address.
        let base = format!("http://{}", crate::commands::test_support::dead_addr());
        let err = run(&base, &[PinnedTool::new("demo-tool", "1.2.3")], dir.path())
            .await
            .expect_err("an unreachable release list must fail closed");

        assert!(
            matches!(err, PinnedError::ReleaseLookupFailed { .. }),
            "got {err:?}"
        );
        assert_nothing_installed(dir.path());
    }
}
