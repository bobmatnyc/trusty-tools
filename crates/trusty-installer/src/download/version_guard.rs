//! No-downgrade guard for `tctl install` (#10584).
//!
//! Why: `tctl install` resolves `latest` — the newest GitHub release on the
//! prebuilt path, crates.io's newest on the `cargo install` fallback — and
//! placed it over whatever was installed. On a host running a newer dev build
//! that silently replaced the dev build with an older release.
//!
//! What: [`probe_installed_build`] reads the version of the binary already at
//! the install destination; [`check_no_downgrade`] refuses a target older than
//! it; [`guard_cargo_fallback`] applies the same check to crates.io's newest
//! version before `cargo install` runs. Every guard fails closed: a version
//! that cannot be read or parsed is a refusal, never a pass.
//!
//! Test: `tests` (the sibling `version_guard_tests.rs`), plus
//! `mod_tests::a_release_older_than_the_installed_build_is_refused_before_download`.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The crates.io API root `cargo install` resolves its newest version from.
const CRATES_IO_API: &str = "https://crates.io/api/v1/crates";
/// How long an installed binary's `--version` may run before it is killed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the crates.io lookup may take before the guard refuses.
const CRATES_IO_TIMEOUT: Duration = Duration::from_secs(15);

/// The build already installed at a destination `tctl install` would write to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBuild {
    /// The concrete path whose `--version` was read.
    pub path: PathBuf,
    /// The semver version that binary reported.
    pub version: String,
}

/// Why `tctl install` refused to place a release (#10584).
///
/// Why: The install command reports a refusal as a member failure; a typed
/// error lets `install_all` tell it apart from a routine "no prebuilt" skip,
/// which an optional member would otherwise render as success.
/// What: one variant per fail-closed arm; each message names the crate, the
/// installed path, and every version involved.
/// Test: `version_guard_tests::newer_installed_build_is_refused_naming_both_versions`,
/// `version_guard_tests::an_unreadable_installed_binary_is_refused`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DowngradeRefusal {
    /// The installed build is newer than the release that would replace it.
    #[error(
        "refusing to install {crate_name} {target}: the installed build at {} is {installed}, \
         which is newer; nothing was replaced (remove that file first to install {target})",
        path.display()
    )]
    NewerInstalled {
        /// The crate being installed.
        crate_name: String,
        /// The installed build's version.
        installed: String,
        /// The version the install would have placed.
        target: String,
        /// The installed binary's path.
        path: PathBuf,
    },
    /// A binary exists at the destination but its version could not be read.
    #[error(
        "refusing to install {crate_name}: could not read the version of the binary already at \
         {} ({reason}); it may be a newer build, so nothing was replaced (remove that file \
         first to reinstall)",
        path.display()
    )]
    UnreadableInstalled {
        /// The crate being installed.
        crate_name: String,
        /// The binary whose version could not be read.
        path: PathBuf,
        /// What went wrong reading it.
        reason: String,
    },
    /// The version the install would place could not be determined.
    #[error(
        "refusing to install {crate_name}: the installed build at {} is {installed}, and the \
         version that would replace it could not be determined ({reason}); nothing was replaced",
        path.display()
    )]
    UnknownTarget {
        /// The crate being installed.
        crate_name: String,
        /// The installed build's version.
        installed: String,
        /// The installed binary's path.
        path: PathBuf,
        /// What went wrong determining the target.
        reason: String,
    },
}

/// Refuse to replace `installed` with an older `target`.
///
/// Why: the single comparison both install branches (prebuilt, cargo) share.
/// What: compares by semver precedence, so build metadata (`+sha`) never
/// orders two builds. Equal or newer `target` → `Ok`; older → `NewerInstalled`;
/// either side unparseable → refusal (fail closed).
/// Test: `version_guard_tests::newer_installed_build_is_refused_naming_both_versions`,
/// `version_guard_tests::equal_or_older_installed_build_proceeds`,
/// `version_guard_tests::unparseable_versions_are_refused`.
pub(crate) fn check_no_downgrade(
    crate_name: &str,
    installed: &InstalledBuild,
    target: &str,
) -> Result<(), DowngradeRefusal> {
    let have = semver::Version::parse(&installed.version).map_err(|e| {
        DowngradeRefusal::UnreadableInstalled {
            crate_name: crate_name.to_owned(),
            path: installed.path.clone(),
            reason: format!("version {:?} is not semver: {e}", installed.version),
        }
    })?;
    let got = semver::Version::parse(target).map_err(|e| DowngradeRefusal::UnknownTarget {
        crate_name: crate_name.to_owned(),
        installed: installed.version.clone(),
        path: installed.path.clone(),
        reason: format!("version {target:?} is not semver: {e}"),
    })?;
    if got.cmp_precedence(&have) == Ordering::Less {
        return Err(DowngradeRefusal::NewerInstalled {
            crate_name: crate_name.to_owned(),
            installed: installed.version.clone(),
            target: target.to_owned(),
            path: installed.path.clone(),
        });
    }
    Ok(())
}

/// Read the build already installed at any of `paths`, if one exists.
///
/// Why: the guard needs the installed version BEFORE anything is fetched.
/// What: probes each existing path's `--version`; returns the highest semver
/// found, or `None` when no path exists. A binary that fails to run, prints no
/// version token, or prints a non-semver one is `UnreadableInstalled` — the
/// install must not overwrite what it cannot identify.
/// Test: `version_guard_tests::probe_reads_the_installed_version`,
/// `version_guard_tests::an_unreadable_installed_binary_is_refused`,
/// `version_guard_tests::probe_is_none_when_nothing_is_installed`.
pub(crate) async fn probe_installed_build(
    crate_name: &str,
    paths: &[&Path],
) -> Result<Option<InstalledBuild>, DowngradeRefusal> {
    probe_installed_build_within(crate_name, paths, PROBE_TIMEOUT).await
}

/// [`probe_installed_build`] with a caller-supplied per-binary timeout (test seam).
/// Test: `version_guard_tests::a_hung_probe_is_killed_at_the_timeout`.
pub(crate) async fn probe_installed_build_within(
    crate_name: &str,
    paths: &[&Path],
    limit: Duration,
) -> Result<Option<InstalledBuild>, DowngradeRefusal> {
    let mut best: Option<(semver::Version, InstalledBuild)> = None;
    for (i, path) in paths.iter().enumerate() {
        if !path.exists() || paths[..i].contains(path) {
            continue;
        }
        let unreadable = |reason: String| DowngradeRefusal::UnreadableInstalled {
            crate_name: crate_name.to_owned(),
            path: path.to_path_buf(),
            reason,
        };
        let line = read_version_line(path, limit).await.map_err(unreadable)?;
        let version = crate::commands::update_engine::extract_version_from_line(&line)
            .ok_or_else(|| unreadable(format!("no version in `--version` output {line:?}")))?;
        let parsed = semver::Version::parse(&version)
            .map_err(|e| unreadable(format!("version {version:?} is not semver: {e}")))?;
        let newer = best
            .as_ref()
            .is_none_or(|(v, _)| parsed.cmp_precedence(v) == Ordering::Greater);
        if newer {
            let build = InstalledBuild {
                path: path.to_path_buf(),
                version,
            };
            best = Some((parsed, build));
        }
    }
    Ok(best.map(|(_, build)| build))
}

/// Run `<path> --version` and return its trimmed stdout, killing it at `limit`.
///
/// Why (#10584 review, HIGH): the shared trusty-common probe drops a timed-out
/// child without killing it, and this probe runs before every install member,
/// so a hung binary would be orphaned.
/// What: spawns with `kill_on_drop`, reads stdout while waiting; on timeout
/// sends SIGKILL and reaps the child before returning `Err`. A non-zero exit
/// or spawn failure is also `Err`.
/// Test: `version_guard_tests::a_hung_probe_is_killed_at_the_timeout`.
async fn read_version_line(path: &Path, limit: Duration) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let shown = path.display();
    let mut child = tokio::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn `{shown} --version`: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("`{shown} --version` has no stdout pipe"))?;
    let run = tokio::time::timeout(limit, async {
        let mut out = Vec::new();
        let (read, status) = tokio::join!(stdout.read_to_end(&mut out), child.wait());
        read.map_err(|e| format!("reading `{shown} --version` output: {e}"))?;
        let status = status.map_err(|e| format!("waiting for `{shown} --version`: {e}"))?;
        Ok::<_, String>((status, out))
    })
    .await;
    match run {
        Ok(Ok((status, out))) if status.success() => {
            Ok(String::from_utf8_lossy(&out).trim().to_owned())
        }
        Ok(Ok((status, _))) => Err(format!("`{shown} --version` exited with {status}")),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            // SIGKILL, then wait, so no orphan or zombie outlives the probe.
            let _ = child.kill().await;
            Err(format!(
                "`{shown} --version` did not finish within {limit:?}; killed"
            ))
        }
    }
}

/// Refuse a `cargo install` fallback that would replace a newer build.
///
/// Why: `cargo install <crate>` places crates.io's newest version, which can
/// be older than a dev build exactly as a GitHub release can.
/// What: with nothing installed, `Ok`. Otherwise reads crates.io's newest
/// stable version and applies [`check_no_downgrade`]; a failed lookup is
/// `UnknownTarget` (fail closed).
/// Test: `version_guard_tests::cargo_fallback_refuses_an_older_crates_io_release`,
/// `version_guard_tests::cargo_fallback_refuses_when_crates_io_is_unreachable`.
pub(crate) async fn guard_cargo_fallback(
    crate_name: &str,
    installed: Option<&InstalledBuild>,
) -> Result<(), DowngradeRefusal> {
    let client = super::http_client();
    guard_cargo_fallback_at(
        &client,
        CRATES_IO_API,
        crate_name,
        installed,
        CRATES_IO_TIMEOUT,
    )
    .await
}

/// [`guard_cargo_fallback`] against a caller-supplied API root and per-request
/// timeout (test seam). A timeout is a failed lookup: `UnknownTarget`.
/// Test: `version_guard_tests::cargo_fallback_refuses_when_crates_io_stalls`.
pub(crate) async fn guard_cargo_fallback_at(
    client: &reqwest::Client,
    api_base: &str,
    crate_name: &str,
    installed: Option<&InstalledBuild>,
    limit: Duration,
) -> Result<(), DowngradeRefusal> {
    let Some(installed) = installed else {
        return Ok(());
    };
    let target = crates_io_latest_at(client, api_base, crate_name, limit)
        .await
        .map_err(|e| DowngradeRefusal::UnknownTarget {
            crate_name: crate_name.to_owned(),
            installed: installed.version.clone(),
            path: installed.path.clone(),
            reason: format!("crates.io lookup failed: {e:#}"),
        })?;
    check_no_downgrade(crate_name, installed, &target)
}

/// crates.io's newest stable version of `crate_name`.
async fn crates_io_latest_at(
    client: &reqwest::Client,
    api_base: &str,
    crate_name: &str,
    limit: Duration,
) -> anyhow::Result<String> {
    use anyhow::Context;

    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(rename = "crate")]
        krate: Krate,
    }
    #[derive(serde::Deserialize)]
    struct Krate {
        max_stable_version: Option<String>,
    }

    let url = format!("{api_base}/{crate_name}");
    let body: Body = client
        .get(&url)
        // #10584 review: the shared client has no timeout; a stall is a hang.
        .timeout(limit)
        // crates.io refuses requests without a descriptive User-Agent.
        .header(
            "User-Agent",
            "trusty-installer (https://github.com/bobmatnyc/trusty-tools)",
        )
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("{url} returned an error status"))?
        .json()
        .await
        .with_context(|| format!("parsing {url}"))?;
    body.krate
        .max_stable_version
        .with_context(|| format!("{url} lists no stable version"))
}

#[cfg(test)]
#[path = "version_guard_tests.rs"]
mod tests;
