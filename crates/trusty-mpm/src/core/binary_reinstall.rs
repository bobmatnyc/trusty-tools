//! Which reinstall route, if any, the running binary's provenance permits.
//!
//! Why: `tm reinstall --binary` must not guess. [`crate::core::binary_provenance`]
//! already answers "where did this binary come from?" for `tm doctor`, but it
//! answers in prose — a [`CheckStatus`](crate::core::doctor::CheckStatus) and a
//! message, which is the right shape for a report and the wrong shape for a
//! decision. Re-deriving the answer at the call site would put a second,
//! drifting copy of the classification next to a command that REPLACES an
//! executable, and the #4033 incident is exactly what happens when provenance
//! is assumed: ten trusty-* binaries installed from worktrees, one from a
//! directory macOS had reaped, one binary served by two conflicting installs.
//!
//! What: [`reinstall_route`] runs the same guards `provenance_report` does and
//! returns a [`ReinstallRoute`] — a registry reinstall, a path reinstall from
//! the recorded source directory, or a refusal carrying the reason. Every
//! ambiguity refuses: no ledger, no ledger record, two installs providing the
//! same binary, a running executable that is not the file cargo installed, a
//! version disagreement, a reaped path source, a git or unrecognised source. A
//! refusal is a correct outcome, not a failure to classify.
//!
//! Test: the `tests` module below covers every branch.

use std::path::{Path, PathBuf};

use crate::core::binary_provenance::{CargoInstall, InstallSource, describe, same_file};

/// How `--binary` may reinstall this executable.
///
/// Why: the two viable routes need different machinery — the registry route
/// goes through `trusty_common::update`, the path route re-runs
/// `cargo install --path` — and everything else must stop.
/// What: `Registry` and `Path` carry what their route needs; `Refuse` carries
/// the operator-facing reason.
/// Test: every `route_*` test below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReinstallRoute {
    /// Installed from crates.io — upgrade through the shared update path.
    Registry {
        /// The crates.io package name to install.
        package: String,
    },
    /// Installed with `cargo install --path`, and that directory still exists.
    Path {
        /// The crates.io package name, for the message.
        package: String,
        /// The source directory to reinstall from.
        dir: PathBuf,
    },
    /// Provenance does not permit an automatic reinstall. Carries why.
    Refuse(String),
}

/// Decide the reinstall route for the running binary.
///
/// Why: see the module doc. This is a pure function over injected inputs —
/// including the filesystem probe — so every branch is testable without
/// touching a real `~/.cargo`.
/// What: `bin_name` is the running executable's file name, `running_version`
/// its compiled-in `CARGO_PKG_VERSION`, `running_exe` its resolved path,
/// `cargo_bin_dir` the directory cargo installs into, `ledger` the parsed
/// installs (`None` = unreadable), and `source_exists` the probe for a path
/// install's source directory. Refuses, in order: unreadable ledger; no record
/// providing `bin_name`; more than one such record; a running executable that
/// is not the file cargo placed; a ledger version that disagrees with what is
/// running; a path install whose source is gone; a git or unrecognised source.
/// Test: `route_refuses_without_a_ledger`, `route_refuses_when_not_in_ledger`,
/// `route_refuses_on_duplicate_installs`,
/// `route_refuses_when_running_binary_is_not_the_cargo_install`,
/// `route_refuses_on_version_disagreement`,
/// `route_refuses_when_path_source_is_gone`, `route_refuses_for_a_git_install`,
/// `route_reinstalls_a_live_path_install`,
/// `route_upgrades_a_registry_install`.
pub fn reinstall_route(
    bin_name: &str,
    running_version: &str,
    running_exe: &Path,
    cargo_bin_dir: &Path,
    ledger: Option<&[CargoInstall]>,
    source_exists: &dyn Fn(&Path) -> bool,
) -> ReinstallRoute {
    let Some(ledger) = ledger else {
        return refuse(format!(
            "cargo's install ledger (`$CARGO_HOME/.crates2.json`) is missing or unreadable, so \
             how `{bin_name}` was installed cannot be determined. Reinstall it yourself with \
             the command you originally used"
        ));
    };

    let providers: Vec<&CargoInstall> = ledger
        .iter()
        .filter(|i| i.bins.iter().any(|b| b == bin_name))
        .collect();

    if providers.is_empty() {
        return refuse(format!(
            "`{bin_name}` is not recorded in cargo's install ledger — it was placed by the \
             prebuilt installer, a package manager, or built in place, so there is no install \
             command to repeat (issue #4033)"
        ));
    }
    if providers.len() > 1 {
        let sources: Vec<String> = providers
            .iter()
            .map(|i| format!("{} {} from {}", i.package, i.version, describe(&i.source)))
            .collect();
        return refuse(format!(
            "`{bin_name}` is provided by {} separate installs ({}) — reinstalling one would not \
             determine what runs. Remove the shadowing install first (issue #4033)",
            providers.len(),
            sources.join("; ")
        ));
    }
    let record = providers[0];

    // #4622 review HIGH-2: the record was matched by BASENAME. Reinstalling
    // cargo's copy would not replace a different file that shadows it on PATH.
    let installed = cargo_bin_dir.join(bin_name);
    if !same_file(running_exe, &installed) {
        return refuse(format!(
            "the running `{bin_name}` is `{}`, but cargo's ledger describes a DIFFERENT file \
             (`{}`). Reinstalling the cargo install would not replace what is running — check \
             `which -a {bin_name}` for a shadowing copy (issue #4033, ADR-0021)",
            running_exe.display(),
            installed.display(),
        ));
    }

    if record.version != running_version {
        return refuse(format!(
            "the running `{bin_name}` reports version {running_version}, but cargo's ledger \
             records {} {} — the binary on disk is not the one cargo tracks, so a reinstall \
             would not replace what is actually running (issue #4033)",
            record.package, record.version,
        ));
    }

    match &record.source {
        InstallSource::Registry => ReinstallRoute::Registry {
            package: record.package.clone(),
        },
        InstallSource::Path(dir) if source_exists(dir) => ReinstallRoute::Path {
            package: record.package.clone(),
            dir: dir.clone(),
        },
        InstallSource::Path(dir) => refuse(format!(
            "`{bin_name}` {running_version} was installed with `cargo install --path` from `{}`, \
             and that directory NO LONGER EXISTS — there is nothing left to rebuild from. \
             Install from crates.io instead: `cargo install {} --locked` (issue #4033)",
            dir.display(),
            record.package,
        )),
        InstallSource::Git(rev) => refuse(format!(
            "`{bin_name}` {running_version} was installed from git ({rev}). Reinstalling would \
             re-resolve that reference, which is a different binary than the pinned commit you \
             are running — do it deliberately rather than through `--binary` (issue #4033)"
        )),
        InstallSource::Other(raw) => refuse(format!(
            "`{bin_name}` {running_version} was installed from an unrecognised source (`{raw}`), \
             so its reinstall command cannot be classified (issue #4033)"
        )),
    }
}

/// Wrap a reason as a refusal.
fn refuse(reason: String) -> ReinstallRoute {
    ReinstallRoute::Refuse(reason)
}

#[cfg(test)]
#[path = "binary_reinstall_tests.rs"]
mod tests;
