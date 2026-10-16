//! Remove a standalone `trusty-embedderd-py` install before `cargo install`
//! of a package that now ships its launcher binary (#9974).
//!
//! Why: trusty-embedderd-py 0.2.0 is library-only; trusty-search ships the
//! `trusty-embedderd-py` launcher binary instead (#9468, owner ruling D94). A
//! machine that ran `cargo install trusty-embedderd-py` 0.1.x still has cargo
//! install metadata naming that package as the binary's owner, and
//! `cargo install trusty-search` refuses to write a binary another package
//! owns.
//!
//! What: reads `<cargo root>/.crates2.json`, plans
//! `cargo uninstall trusty-embedderd-py --root <root>` ahead of the install
//! when the old package owns the launcher, and runs the plan through an
//! injectable [`CargoRunner`]. A failed uninstall stops the plan with
//! [`StandaloneLauncherError`] before the install runs.
//!
//! Test: `standalone_launcher_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The package that owned the launcher binary before 0.2.0.
pub(crate) const STANDALONE_PACKAGE: &str = "trusty-embedderd-py";

/// The launcher binary whose ownership moved to trusty-search.
pub(crate) const LAUNCHER_BIN: &str = "trusty-embedderd-py";

/// The metadata file cargo keeps in its install root (format v2).
const CRATES2_FILE: &str = ".crates2.json";

/// A failure that stops the cargo-install plan before the install runs.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StandaloneLauncherError {
    /// `cargo uninstall` of the standalone package failed, so the install
    /// that follows would refuse on the binary's ownership.
    #[error(
        "`cargo uninstall {package} --root {}` failed: {detail}. The standalone \
         `{package}` package still owns the `{LAUNCHER_BIN}` binary, so \
         `cargo install {crate_name}` would refuse. Remove it by hand with \
         `cargo uninstall {package}`, then rerun the install.",
        .root.display()
    )]
    UninstallFailed {
        package: String,
        crate_name: String,
        root: PathBuf,
        detail: String,
    },
}

/// Whether cargo's install metadata records the standalone launcher package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LauncherRecord {
    /// The standalone package is recorded as the owner of the launcher bin.
    Recorded,
    /// No such record, or no readable metadata.
    NotRecorded,
}

/// One cargo command in an install plan, in execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CargoStep {
    /// `cargo uninstall <package> --root <root>`, run so `for_crate` can
    /// take over the launcher binary.
    Uninstall {
        package: String,
        root: PathBuf,
        for_crate: String,
    },
    /// `cargo install <crate_name> --locked`.
    Install { crate_name: String },
}

/// The slice of `.crates2.json` this module reads. Unknown fields are ignored.
#[derive(serde::Deserialize)]
struct Crates2 {
    #[serde(default)]
    installs: BTreeMap<String, Crates2Entry>,
}

/// One `installs` entry, keyed by `"<name> <version> (<source>)"`.
#[derive(serde::Deserialize)]
struct Crates2Entry {
    #[serde(default)]
    bins: Vec<String>,
}

/// Runs the cargo commands of a plan. Tests inject a recording fake.
pub(crate) trait CargoRunner {
    /// Run `cargo uninstall <package> --root <root>`; `Err` carries cargo's
    /// stderr or the spawn failure.
    async fn uninstall(&self, package: &str, root: &Path) -> Result<(), String>;
    /// Run `cargo install <crate_name> --locked`.
    async fn install(&self, crate_name: &str) -> anyhow::Result<()>;
}

/// Whether `.crates2.json` text records the standalone package as the owner
/// of the launcher binary.
///
/// Why: the uninstall must only run when cargo would refuse the install, and
/// only for the one package D94 retired — never for another crate.
/// What: matches the package name (the first word of the `installs` key)
/// exactly, and requires `bins` to contain the launcher.
/// Test: `fixture_with_standalone_launcher_plans_uninstall_before_install`,
/// `fixture_without_standalone_launcher_plans_no_uninstall`.
///
/// # Errors
/// Returns the JSON error when `json` does not parse as v2 install metadata.
pub(crate) fn launcher_recorded(json: &str) -> Result<bool, serde_json::Error> {
    let parsed: Crates2 = serde_json::from_str(json)?;
    Ok(parsed.installs.iter().any(|(pkg_id, entry)| {
        pkg_id.split_whitespace().next() == Some(STANDALONE_PACKAGE)
            && entry.bins.iter().any(|b| b == LAUNCHER_BIN)
    }))
}

/// Read `<root>/.crates2.json` and report whether the standalone launcher is
/// recorded.
///
/// Why: a malformed file is treated as "no record" with a logged warning, not
/// as an error. Cargo reads the same file during `cargo install` and stops
/// with its own "failed to parse crate metadata" error naming it, so the
/// install still fails loudly and nothing is silently skipped. Failing here
/// instead would block installs cargo itself accepts whenever this narrower
/// reader disagrees with a future cargo format.
/// What: a missing file is `NotRecorded` silently; an unreadable or malformed
/// file is `NotRecorded` with a `warn` log.
/// Test: `missing_crates2_is_not_recorded`, `malformed_crates2_is_not_recorded`.
pub(crate) fn read_launcher_record(root: &Path) -> LauncherRecord {
    let path = root.join(CRATES2_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LauncherRecord::NotRecorded,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read cargo install metadata; assuming no standalone {STANDALONE_PACKAGE} install");
            return LauncherRecord::NotRecorded;
        }
    };
    match launcher_recorded(&text) {
        Ok(true) => LauncherRecord::Recorded,
        Ok(false) => LauncherRecord::NotRecorded,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "malformed cargo install metadata; assuming no standalone {STANDALONE_PACKAGE} install");
            LauncherRecord::NotRecorded
        }
    }
}

/// Pure resolution of cargo's install root.
///
/// Why: the metadata read must look where `cargo install` writes, and the
/// uninstall must act on the same root.
/// What: a non-empty `CARGO_INSTALL_ROOT` wins; otherwise the parent of
/// [`trusty_common::bin_resolve::canonical_bin_dir_from`] (`$CARGO_HOME`, else
/// `~/.cargo`) — the rule `self_update::cargo_install_dir` uses. Cargo's
/// `install.root` config key is not read.
/// Test: `install_root_prefers_cargo_install_root`,
/// `install_root_falls_back_to_cargo_home_then_home`.
pub(crate) fn cargo_install_root_from(
    home: Option<&Path>,
    install_root: Option<&str>,
    cargo_home: Option<&str>,
) -> Option<PathBuf> {
    match install_root {
        Some(r) if !r.is_empty() => Some(PathBuf::from(r)),
        _ => trusty_common::bin_resolve::canonical_bin_dir_from(home, cargo_home)
            .and_then(|bin| bin.parent().map(Path::to_path_buf)),
    }
}

/// The cargo commands that install `crate_name`, in order.
///
/// Why: a recorded standalone launcher makes `cargo install` refuse, so its
/// uninstall must come first; with no record nothing extra may run.
/// What: prepends `Uninstall` only when `crate_name` ships the launcher bin
/// (per the shared `installed_binaries` table), is not the standalone package
/// itself, the record is `Recorded` and a root is known. Always ends with
/// `Install`.
/// Test: `fixture_with_standalone_launcher_plans_uninstall_before_install`,
/// `fixture_without_standalone_launcher_plans_no_uninstall`,
/// `crate_without_the_launcher_plans_no_uninstall`.
pub(crate) fn plan_cargo_install(
    crate_name: &str,
    root: Option<&Path>,
    record: LauncherRecord,
) -> Vec<CargoStep> {
    let ships_launcher = crate_name != STANDALONE_PACKAGE
        && trusty_common::bin_resolve::installed_binaries(crate_name)
            .iter()
            .any(|b| b == LAUNCHER_BIN);
    let mut steps = Vec::with_capacity(2);
    if let (true, LauncherRecord::Recorded, Some(root)) = (ships_launcher, record, root) {
        steps.push(CargoStep::Uninstall {
            package: STANDALONE_PACKAGE.to_owned(),
            root: root.to_path_buf(),
            for_crate: crate_name.to_owned(),
        });
    }
    steps.push(CargoStep::Install {
        crate_name: crate_name.to_owned(),
    });
    steps
}

/// Run a plan in order, stopping at the first failure.
///
/// Why: Fail-Open Check — an install after a failed uninstall is certain to
/// refuse, so the operator gets the manual fix instead of cargo's ownership
/// error.
/// What: a failed `Uninstall` returns [`StandaloneLauncherError::UninstallFailed`]
/// and no later step runs; `Install` errors propagate unchanged.
/// Test: `uninstall_failure_is_typed_and_skips_the_install`.
///
/// # Errors
/// The first step's failure.
pub(crate) async fn run_cargo_steps<R: CargoRunner>(
    steps: &[CargoStep],
    runner: &R,
) -> anyhow::Result<()> {
    for step in steps {
        match step {
            CargoStep::Uninstall {
                package,
                root,
                for_crate,
            } => {
                tracing::info!(%package, root = %root.display(), %for_crate, "removing standalone launcher package before cargo install");
                runner.uninstall(package, root).await.map_err(|detail| {
                    StandaloneLauncherError::UninstallFailed {
                        package: package.clone(),
                        crate_name: for_crate.clone(),
                        root: root.clone(),
                        detail,
                    }
                })?;
            }
            CargoStep::Install { crate_name } => runner.install(crate_name).await?,
        }
    }
    Ok(())
}

/// The real cargo, spawned as a child process.
pub(crate) struct SystemCargo {
    /// Capture the install's stdio ([`trusty_common::update::perform_upgrade_captured`])
    /// instead of inheriting it, for callers driving a live terminal display.
    pub(crate) captured: bool,
}

impl CargoRunner for SystemCargo {
    async fn uninstall(&self, package: &str, root: &Path) -> Result<(), String> {
        let out = tokio::process::Command::new("cargo")
            .arg("uninstall")
            .arg(package)
            .arg("--root")
            .arg(root)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| format!("could not run cargo: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!(
            "{} ({})",
            String::from_utf8_lossy(&out.stderr).trim(),
            out.status
        ))
    }

    async fn install(&self, crate_name: &str) -> anyhow::Result<()> {
        if self.captured {
            trusty_common::update::perform_upgrade_captured(crate_name).await
        } else {
            trusty_common::update::perform_upgrade(crate_name).await
        }
    }
}

/// `cargo install <crate_name> --locked`, first removing a standalone
/// `trusty-embedderd-py` install that would make cargo refuse (#9974).
///
/// Why: the tctl install and upgrade cargo-fallback paths share one entry so
/// neither can skip the ownership cleanup.
/// What: resolves the install root from the environment, reads its
/// `.crates2.json`, plans with [`plan_cargo_install`] and runs the plan with
/// [`SystemCargo`].
/// Test: the decision and ordering are covered by
/// `fixture_with_standalone_launcher_plans_uninstall_before_install` and
/// `uninstall_failure_is_typed_and_skips_the_install`; this wrapper is the
/// env read and the real spawn.
///
/// # Errors
/// [`StandaloneLauncherError::UninstallFailed`], or the install's failure.
pub(crate) async fn cargo_install(crate_name: &str, captured: bool) -> anyhow::Result<()> {
    let root = cargo_install_root_from(
        dirs::home_dir().as_deref(),
        std::env::var("CARGO_INSTALL_ROOT").ok().as_deref(),
        std::env::var("CARGO_HOME").ok().as_deref(),
    );
    let record = root
        .as_deref()
        .map_or(LauncherRecord::NotRecorded, read_launcher_record);
    let steps = plan_cargo_install(crate_name, root.as_deref(), record);
    run_cargo_steps(&steps, &SystemCargo { captured }).await
}

#[cfg(test)]
#[path = "standalone_launcher_tests.rs"]
mod tests;
