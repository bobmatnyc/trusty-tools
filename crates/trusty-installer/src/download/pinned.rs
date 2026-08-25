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
//! [`PinnedError`] naming what was pinned, what arrived, and what the install
//! directory now holds — "nothing was installed" for every variant but
//! [`PinnedError::PlacementInterrupted`], which lists the files it could not
//! avoid leaving. There is no fallback path — `cargo install` is never reachable
//! from this module.
//!
//! This is ADDITIVE: `try_install_prebuilt`'s latest-plus-fallback semantics are
//! untouched, because its existing callers depend on them.
//!
//! ACCEPTED TRADE-OFF — check 5 executes the downloaded binary. Proving a pin by
//! reading the binary's own `--version` means running a freshly downloaded,
//! not-independently-signed artifact inside the installer's process and user
//! context, unattended. `try_install_prebuilt` never does this: it places what it
//! downloads without executing it. The checksum that gates the execution is
//! self-published by the same release pipeline that would be compromised in the
//! attack this worries about, so it is not an independent check — a pipeline that
//! can serve a malicious tarball can serve a matching `.sha256` beside it. We
//! accept it because a mis-tagged or mis-built asset passes every URL-level and
//! digest-level check, and executing the binary is the only way to catch it; a
//! caller wanting a second, independent gate supplies its own digest via
//! [`PinnedTool::with_sha256`].
//!
//! Test: `tests` drives the whole pipeline against a loopback fixture server
//! (the crate's established stub-server pattern — no live network, no new
//! dev-dependency); each fail-closed arm has its own test.

use std::path::{Path, PathBuf};

use crate::download::{fetch, glibc, release};

// #5970: "can this be installed without installing it" — the question a consumer
// had to either install or grow a second resolver to answer. Its `resolve_pin` is
// checks 1 and 2 of `stage_one` below, shared rather than copied.
mod preflight;

pub use preflight::{preflight_pinned_set, PinnedPreflight};

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

/// Why a pinned install failed, and what it left on disk.
///
/// Why: A GUI has to render these, and an operator has to act on them, so the
/// pin and the thing that arrived must both survive as structured fields rather
/// than being flattened into one string. The `Display` text states explicitly
/// what happened to the install directory, because the defect this type exists
/// to prevent is a caller treating a failure as a degraded success.
///
/// What: One variant per failure the pipeline can distinguish. Every variant
/// except [`PinnedError::PlacementInterrupted`] means no tool was placed, and
/// says so; `PlacementInterrupted` is the one case where files remain, and it
/// names them rather than claiming a clean directory.
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
    ///
    /// #6164: when the crate IS published and only this version is missing, the
    /// message names release-list lag as the likely cause. That reading was
    /// unavailable to the operator who hit it — `trusty-audit audit` failed half
    /// an hour after trusty-review 0.23.0 went live, reported `0.22.1` as the
    /// newest published version, and read as a hard version-not-found error; a
    /// manual retry with no other change succeeded. The lookup already retries
    /// (`preflight::RETRY_AFTER`), so reaching this means the wait was too
    /// short, which is exactly when the suggestion is worth making.
    #[error(
        "{crate_name} {version} is not a published stable release, so it could not \
         be installed; nothing was installed. Published stable versions: {}.{}",
        if available.is_empty() { "none".to_owned() } else { available.join(", ") },
        if available.is_empty() {
            String::new()
        } else {
            format!(
                " {crate_name} itself is published, so if {version} was released in the last \
                 half hour this is most likely a release list that has not caught up yet — \
                 wait and run the same command again"
            )
        }
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

    /// A filesystem step failed before any file was published under its final
    /// name.
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

    /// The commit phase failed PART-WAY. The install directory is NOT clean.
    ///
    /// Why: Every other variant can honestly say nothing was installed because
    /// the failure happened before the first commit rename. This one cannot, so
    /// it says the opposite and names what survives — a false "nothing was
    /// installed" in a supply-chain path is worse than the failure itself.
    #[error(
        "installing {crate_name} {version} failed while committing files into the \
         install directory, AFTER other files had already been placed. \
         THE INSTALL DIRECTORY IS NOT CLEAN — these files remain and must be \
         removed before retrying: {}. Crates left on disk: {}: {source}",
        placed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
        crates_on_disk.join(", ")
    )]
    PlacementInterrupted {
        /// The crate whose commit rename failed.
        crate_name: String,
        /// The version that was pinned.
        version: String,
        /// Every crate with at least one file left in the install directory.
        crates_on_disk: Vec<String>,
        /// Every file this call left in the install directory, in commit order.
        placed: Vec<PathBuf>,
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
/// been observed reporting it. On `Err`, the guarantee is
/// [`install_pinned_set`]'s: no file was placed, EXCEPT for
/// [`PinnedError::PlacementInterrupted`], which names what survives. A one-tool
/// set can still reach that variant when its archive ships several binaries and
/// a later one fails to commit.
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
    // install_pinned_set's postcondition: on Ok, the returned vector has one
    // entry per input tool, and this call always passes exactly one. An empty
    // vector here would mean install_pinned_set violated its own documented
    // postcondition — a bug in that function, not a state any caller input can
    // reach — so this is a programmer-error invariant, not a typed error: a
    // `PinnedError` here could only fire after install_pinned_set already
    // returned `Ok`, making its "nothing was installed" text false in the one
    // case it could occupy.
    Ok(installed
        .pop()
        .expect("install_pinned_set must return exactly one entry for a one-element input"))
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
/// has one entry per input tool, in order.
///
/// On `Err`, exactly one of two states holds, and the error says which:
/// - Every variant except [`PinnedError::PlacementInterrupted`] — no file from
///   this call exists under its final name in `install_dir`, including files
///   from tools that verified successfully. A hidden `.<name>.tmp.<pid>` scratch
///   file can survive if the cleanup that removes it itself fails; nothing
///   executes or resolves under that name.
/// - [`PinnedError::PlacementInterrupted`] — commit renames had already begun
///   and the directory is NOT clean. The variant lists every file left behind
///   and every crate they belong to. It says so instead of claiming a clean
///   directory.
///
/// What: Phase 1 stages every tool into a temp dir (resolve exact tag → download
/// → verify checksums → extract → probe `--version`). Phase 2 is copy-then-
/// commit: every binary of every tool is first copied into `install_dir` under a
/// hidden temporary name, and only after ALL copies succeed is anything renamed
/// to its final name. That reduces the window in which a partial set can exist
/// to a sequence of same-directory renames. `cargo install` is never invoked on
/// any path.
///
/// Test: `tests::a_set_installs_every_tool_when_all_verify`,
/// `tests::a_set_installs_nothing_when_any_tool_fails` (a phase-1 failure),
/// `tests::a_set_places_nothing_when_a_later_tool_cannot_be_placed` (a phase-2
/// failure), `tests::an_interrupted_commit_names_the_files_it_left_behind`, plus
/// one test per [`PinnedError`] arm.
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

    // Phase 2 — every tool verified. Copy the whole set into place under
    // temporary names first, then commit. #5517: a bare per-tool place loop
    // could leave tool 1 installed while the error said nothing was.
    let pending = copy_set_into_install_dir(&staged, install_dir)?;
    commit_set(pending)
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

    // Checks 1 and 2 — a prebuilt exists for this host, and the EXACT pinned
    // version is published. #5970 moved them into `preflight` so the dry-run
    // check and this install ask the same two questions of the same code.
    let (target, resolved) = preflight::resolve_pin(client, endpoints, tool).await?;

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
    // This EXECUTES the downloaded artifact; see the module doc's accepted
    // trade-off. Do not remove the check to avoid it — removing it removes the
    // only gate that catches a mis-built asset.
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

    // #5495: place executables only. Release tarballs also ship `LICENSE` and
    // `README.md`, and since every crate in this workspace ships the same
    // filenames, a multi-tool set collided on `tools/LICENSE` and could never
    // install — found by running `taudit install` against the real
    // tga/trusty-analyze/trusty-review triple. Filtering here rather than in the
    // collision check keeps a genuine two-binaries-one-name conflict an error.
    let placeable = executables_among(&extract_dir, &binary_names);

    Ok(Staged {
        tool: tool.clone(),
        extract_dir,
        binary_names: placeable,
    })
}

/// Narrow an extracted archive's files to the ones worth installing.
///
/// Why: `fetch::extract_binaries` returns every regular file in the archive, so
/// its result names documentation as well as binaries. Installing a directory of
/// executables does not want `LICENSE`, and two tools shipping one both breaks
/// the set (#5495).
///
/// What: on unix, the files carrying an execute bit. Elsewhere the mode is not
/// available, so every file is kept and the pre-existing behaviour stands —
/// the pinned path is macOS/Linux-facing today (#5473 scopes the consumer to
/// macOS arm64), and a Windows collision would still fail closed rather than
/// mis-install.
///
/// Test: `tests::documentation_files_are_not_installed`.
fn executables_among(dir: &Path, names: &[String]) -> Vec<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        names
            .iter()
            .filter(|name| {
                std::fs::metadata(dir.join(name)).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            })
            .cloned()
            .collect()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        names.to_vec()
    }
}

/// One staged tool copied into the install directory under temporary names,
/// awaiting its commit renames.
#[derive(Debug)]
struct Pending {
    crate_name: String,
    version: String,
    binary_path: PathBuf,
    /// `(temporary path, final path)` per binary, in commit order.
    renames: Vec<(PathBuf, PathBuf)>,
}

/// Copy every binary of every staged tool into `install_dir` under a hidden
/// temporary name, publishing nothing.
///
/// Why: The copy is the step that can plausibly fail — source unreadable, disk
/// full, `install_dir` not writable. Doing all of them before the first commit
/// rename is what keeps a mid-set failure from leaving one tool installed while
/// the error claims none was.
///
/// # Postconditions
/// On `Ok`, one [`Pending`] per input tool, in order, every temporary file
/// written and executable. On `Err`, every temporary file this call created has
/// been removed (best effort) and no file exists under a final name.
///
/// What: Rejects a final path already occupied by a directory and a final path
/// two tools in the set both claim — both would surface later as a confusing
/// mid-commit failure. Then copies via `fetch::stage_binary`. A name whose
/// source file is absent is a third rejection (#5810), not a skip: placing a
/// subset while returning `Ok` would report a partial set as a complete one.
///
/// Test: `tests::a_set_places_nothing_when_a_later_tool_cannot_be_placed`,
/// `tests::a_set_is_rejected_when_two_tools_claim_the_same_binary_name`,
/// `tests::a_binary_missing_from_the_staged_set_is_never_silently_skipped`.
fn copy_set_into_install_dir(
    staged: &[Staged],
    install_dir: &Path,
) -> Result<Vec<Pending>, PinnedError> {
    let io = |s: &Staged, e: anyhow::Error| PinnedError::Io {
        crate_name: s.tool.crate_name.clone(),
        version: s.tool.version.clone(),
        source: e,
    };
    if let Some(first) = staged.first() {
        std::fs::create_dir_all(install_dir).map_err(|e| {
            io(
                first,
                anyhow::Error::new(e).context(format!(
                    "creating the install directory {}",
                    install_dir.display()
                )),
            )
        })?;
    }

    let mut pending: Vec<Pending> = Vec::with_capacity(staged.len());
    let mut claimed: Vec<PathBuf> = Vec::new();
    for s in staged {
        let mut renames = Vec::new();
        for name in &s.binary_names {
            let dest = install_dir.join(name);
            let problem = if dest.is_dir() {
                Some(format!(
                    "a directory already occupies {}, so `{name}` cannot be placed there",
                    dest.display()
                ))
            } else if claimed.contains(&dest) {
                Some(format!(
                    "two tools in this set both install `{name}` to {}",
                    dest.display()
                ))
            } else {
                None
            };
            if let Some(problem) = problem {
                discard(temporaries(&pending).chain(temporaries_of(&renames)));
                return Err(io(s, anyhow::anyhow!(problem)));
            }

            match fetch::stage_binary(&s.extract_dir, install_dir, name) {
                // #5810: `stage_binary` answers `Ok(None)` when the source file
                // is not there, and this arm used to `continue`. The pinned
                // path then placed fewer binaries than it named and still
                // returned `Ok` — a partial set reported as a complete one,
                // which is the mixed-version outcome the pin exists to prevent.
                // Fail closed here, before the first commit rename, so the
                // "nothing was installed" text stays true.
                Ok(None) => {
                    discard(temporaries(&pending).chain(temporaries_of(&renames)));
                    return Err(io(
                        s,
                        anyhow::anyhow!(
                            "`{name}` was staged for installation but is not present in {}",
                            s.extract_dir.display()
                        ),
                    ));
                }
                Ok(Some(tmp)) => {
                    claimed.push(dest.clone());
                    renames.push((tmp, dest));
                }
                Err(e) => {
                    discard(temporaries(&pending).chain(temporaries_of(&renames)));
                    return Err(io(
                        s,
                        e.context("copying binaries into the install directory"),
                    ));
                }
            }
        }
        pending.push(Pending {
            crate_name: s.tool.crate_name.clone(),
            version: s.tool.version.clone(),
            binary_path: install_dir.join(&s.tool.binary),
            renames,
        });
    }
    Ok(pending)
}

/// Remove temporary files that will never be committed, best effort. A leftover
/// is a hidden scratch name nothing resolves, not an installed tool.
fn discard<'a>(tmps: impl Iterator<Item = &'a PathBuf>) {
    for tmp in tmps {
        let _ = std::fs::remove_file(tmp);
    }
}

/// The temporary halves of a rename list.
fn temporaries_of(renames: &[(PathBuf, PathBuf)]) -> impl Iterator<Item = &PathBuf> {
    renames.iter().map(|(tmp, _)| tmp)
}

/// The temporary halves of every rename across several pending tools.
fn temporaries(pending: &[Pending]) -> impl Iterator<Item = &PathBuf> {
    pending.iter().flat_map(|p| temporaries_of(&p.renames))
}

/// Rename every copied binary from its temporary name to its final one.
///
/// Why: Same-directory renames are the cheapest and least failure-prone step
/// available, which is why every fallible operation was moved ahead of them.
/// They are still not infallible, so a failure part-way reports what it left
/// rather than the "nothing was installed" text the other variants carry.
///
/// # Postconditions
/// On `Ok`, one [`PinnedInstall`] per input, in order. On `Err`, either the very
/// first rename failed and nothing was committed ([`PinnedError::Io`], with the
/// remaining temporaries removed), or renames had already succeeded and
/// [`PinnedError::PlacementInterrupted`] names every file left behind.
///
/// Test: `tests::an_interrupted_commit_names_the_files_it_left_behind`,
/// `tests::a_set_installs_every_tool_when_all_verify`.
fn commit_set(pending: Vec<Pending>) -> Result<Vec<PinnedInstall>, PinnedError> {
    let mut installed: Vec<PinnedInstall> = Vec::with_capacity(pending.len());
    let mut placed: Vec<PathBuf> = Vec::new();
    let mut crates_on_disk: Vec<String> = Vec::new();

    for (idx, p) in pending.iter().enumerate() {
        let mut paths = Vec::with_capacity(p.renames.len());
        for (n, (tmp, dest)) in p.renames.iter().enumerate() {
            if let Err(e) = std::fs::rename(tmp, dest) {
                discard(temporaries_of(&p.renames[n..]).chain(temporaries(&pending[idx + 1..])));
                let source = anyhow::Error::new(e)
                    .context(format!("renaming into place: {}", dest.display()));
                if placed.is_empty() {
                    return Err(PinnedError::Io {
                        crate_name: p.crate_name.clone(),
                        version: p.version.clone(),
                        source,
                    });
                }
                return Err(PinnedError::PlacementInterrupted {
                    crate_name: p.crate_name.clone(),
                    version: p.version.clone(),
                    crates_on_disk,
                    placed,
                    source,
                });
            }
            // Record the crate as on-disk the moment its FIRST file commits, so
            // a failure part-way through one tool still names it.
            if crates_on_disk.last() != Some(&p.crate_name) {
                crates_on_disk.push(p.crate_name.clone());
            }
            placed.push(dest.clone());
            paths.push(dest.clone());
        }
        installed.push(PinnedInstall {
            crate_name: p.crate_name.clone(),
            version: p.version.clone(),
            binary_path: p.binary_path.clone(),
            paths,
        });
    }
    Ok(installed)
}

// The fixture-server suite lives in its own file: it is ~550 lines, and an
// `#[cfg(all(test, …))]` module body counts toward the 500-SLOC production cap
// (only the bare `#[cfg(test)] mod x { … }` shape is excluded, per #5153).
// `pinned/tests.rs` is classified test-file by basename, so it gets the 3000 cap.
// `unix` because the fixtures are executable /bin/sh scripts; every target this
// crate publishes prebuilts for is unix.
#[cfg(all(test, unix))]
mod tests;
