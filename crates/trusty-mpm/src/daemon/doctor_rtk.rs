//! The `tm doctor` `rtk` check — is the `rtk` binary on PATH for `tm compress`.
//!
//! Why: `tm compress` shells out to `rtk` and falls back to a slower native
//! compressor when the binary is absent. The fallback is silent, so an install
//! that never pulled rtk in reads exactly like one that did. rtk became an
//! install dependency in #7311 (the Homebrew formula `depends_on "rtk"`), and
//! `cargo install` users still have to install it themselves — this check is
//! what makes the difference visible.
//! What: [`check_rtk`] resolves the binary through the shared
//! [`trusty_common::bin_resolve::resolve_binary`] and folds the outcome via
//! [`build_rtk_check`]. Advisory only — `Warn` when absent, never `Fail`, since
//! the native fallback keeps compression working.
//! Test: `doctor_rtk_tests.rs` covers every branch of the fold with an injected
//! resolver, so no test depends on the real PATH.
//!
//! rtk is a BINARY dependency only: `rtk init` / `rtk init -g` install a
//! competing PreToolUse Bash hook, and tm invokes rtk directly, so no message
//! here ever suggests running them.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// How long `rtk --version` gets to answer before the check reports presence
/// without a version.
///
/// Why: doctor is not latency-sensitive, but a wedged binary must not stall it.
const RTK_VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// The remediation sentence every absent-rtk message ends with.
///
/// Why: the install path and the anti-`rtk init` warning have to travel
/// together — an operator who reads only "install rtk" is one search away from
/// running `rtk init`, which installs a PreToolUse Bash hook that competes with
/// tm's own.
/// What: a single literal, shared by the doctor message and asserted by
/// `build_rtk_check_absent_warns_with_remediation`.
/// Test: `build_rtk_check_absent_warns_with_remediation`,
/// `rtk_messages_only_mention_rtk_init_as_a_prohibition`.
// #7311: rtk is an install dependency; never run rtk init.
pub(super) const RTK_REMEDIATION: &str =
    "install with `brew install rtk`; do not run `rtk init`, tm invokes rtk directly";

/// Report whether `rtk` is available to `tm compress`.
///
/// Why: see the module header — the rtk-vs-native compression path is otherwise
/// invisible.
/// What: resolves `rtk` through the shared binary resolver (which sees the
/// Homebrew and user bin dirs a launchd-minimal `PATH` omits), probes
/// `rtk --version` under [`RTK_VERSION_TIMEOUT`], and folds both through
/// [`build_rtk_check`].
/// Test: the fold is covered by `doctor_rtk_tests.rs`; this wrapper only binds
/// the two real probes to it.
// #7311: rtk is an install dependency; never run rtk init.
pub(super) fn check_rtk() -> DoctorCheck {
    build_rtk_check(
        &|name| trusty_common::bin_resolve::resolve_binary(name),
        &probe_rtk_version,
    )
}

/// Fold a resolver result into a [`DoctorCheck`] (pure, given pure closures).
///
/// Why: keeping the verdict logic behind injected closures makes both branches
/// unit-testable without a real `rtk` on the test machine's PATH.
/// What: `Ok` naming the resolved path (and the version when the probe
/// answered) when the resolver finds the binary; `Warn` carrying
/// [`RTK_REMEDIATION`] when it does not. Never `Fail` — `tm compress` still
/// works through its native fallback, just more slowly.
/// Test: `build_rtk_check_present_is_ok`,
/// `build_rtk_check_present_without_version_is_ok`,
/// `build_rtk_check_absent_warns_with_remediation`,
/// `rtk_check_is_advisory_only`, `rtk_messages_only_mention_rtk_init_as_a_prohibition`.
pub(super) fn build_rtk_check(
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
    probe_version: &dyn Fn(&Path) -> Option<String>,
) -> DoctorCheck {
    let Some(path) = resolve("rtk") else {
        return DoctorCheck::new(
            "rtk",
            CheckStatus::Warn,
            format!(
                "`rtk` is not on PATH — `tm compress` runs its slower native \
                 fallback instead of the rtk binary. {RTK_REMEDIATION}"
            ),
        );
    };
    let shown = path.display();
    let message = match probe_version(&path) {
        Some(version) => format!("rtk found at `{shown}` ({version})"),
        None => format!("rtk found at `{shown}` (version probe did not answer)"),
    };
    DoctorCheck::new("rtk", CheckStatus::Ok, message)
}

/// Run `<path> --version` under [`RTK_VERSION_TIMEOUT`] and return its first line.
///
/// Why: the version is the cheapest evidence that the resolved file is a
/// runnable rtk rather than a same-named stray, but a wedged binary must not
/// hold the doctor run open.
/// What: bounded spawn; returns `None` on spawn failure, non-zero exit, empty
/// output, or timeout. The caller reports presence either way.
/// Test: side-effecting (spawns a process); the fold's tests inject a fake
/// version probe instead — `build_rtk_check_present_without_version_is_ok`.
fn probe_rtk_version(path: &Path) -> Option<String> {
    let path = path.to_path_buf();
    crate::core::gh_account::run_bounded(RTK_VERSION_TIMEOUT, move || {
        let out = std::process::Command::new(&path)
            .arg("--version")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let first = text.lines().next()?.trim().to_owned();
        (!first.is_empty()).then_some(first)
    })
}

#[cfg(test)]
#[path = "doctor_rtk_tests.rs"]
mod tests;
