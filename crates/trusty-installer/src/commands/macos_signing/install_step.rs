//! The install-time sign step: every installed trusty binary, one report line
//! each.
//!
//! Why (#11587, owner ruling Dsf "sign at install only; no CI signing"): a
//! Keychain item trusts the binary that created it by designated requirement.
//! An ad-hoc `cargo install` binary has its cdhash as that requirement, so
//! every upgrade re-prompts. Before this step `tctl install` signed only the
//! search set and `tm`, never read the signature back, and trusty-browser
//! signed itself only on `install-service` / `install-host`.
//!
//! What: [`sign_binaries_with`] walks a list of binary names against an
//! injectable `codesign`. A binary that already carries a Developer ID
//! signature with its table identifier is left alone; any other is signed,
//! verified and attested by `strict::sign_one`. Every present signable binary
//! yields one [`BinarySigning`], and `Signed` is set only after the
//! attestation passes. [`InstallSigner`] is the state `commands::install`
//! holds for one run: it signs each member's binaries as they land, then
//! sweeps the install directory for signable binaries other installs placed
//! (trusty-secrets, trusty-browser).
//!
//! Test: `tests::sign_binaries_reports_each_binary_on_its_own` and the rest of
//! `install_step_tests.rs`, against a fake `codesign` script. No test runs the
//! real `codesign` or `security`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{all_signable_binaries, codesign_identifier, set_of, strict, use_hardened_runtime};

/// How one binary's signing ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignStatus {
    /// It already carried a Developer ID signature with its identifier.
    AlreadySigned,
    /// Signed now, verified, and read back as Developer ID.
    Signed,
    /// No Developer ID identity is configured; the binary was not signed.
    NoIdentity,
    /// An identity is configured, and signing, verifying or attesting failed.
    Failed,
}

/// One binary's signing outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BinarySigning {
    /// The binary name, e.g. `trusty-browser-mcp`.
    pub binary: String,
    /// The path that was checked and, when needed, signed.
    pub path: PathBuf,
    /// How it ended.
    pub status: SignStatus,
    /// The cause of a `Failed` outcome; empty otherwise.
    pub detail: String,
}

impl BinarySigning {
    /// Whether the binary carries an attested Developer ID signature now.
    pub fn is_signed(&self) -> bool {
        matches!(self.status, SignStatus::AlreadySigned | SignStatus::Signed)
    }

    /// The report line for this binary.
    ///
    /// Why (#11587): fail-open guard. Only an attested outcome may read as
    /// signed; every other one says `NOT signed` and why.
    /// Test: `tests::install_step_lines_never_call_an_unsigned_binary_signed`.
    pub fn line(&self) -> String {
        let (name, path) = (&self.binary, self.path.display());
        match self.status {
            SignStatus::AlreadySigned => format!("{name}: already Developer-ID signed ({path})"),
            SignStatus::Signed => {
                format!("{name}: signed with Developer ID and attested ({path})")
            }
            SignStatus::NoIdentity => format!(
                "warning: {name}: NOT signed ({path}): no Developer ID Application identity is \
                 configured, so it keeps an ad-hoc signature and macOS re-prompts for Keychain \
                 access after each upgrade"
            ),
            SignStatus::Failed => format!("error: {name}: NOT signed ({path}): {}", self.detail),
        }
    }
}

/// Whether `display` (`codesign -dvv` output) shows a Developer ID signature
/// with `identifier` and, when `team` is given, that Team ID.
fn already_signed(display: &str, identifier: &str, team: Option<&str>) -> bool {
    let wanted = format!("Identifier={identifier}");
    display.lines().any(|l| l.trim() == wanted)
        && strict::attest_developer_id(display, team).is_ok()
}

/// Sign every present signable binary of `binaries` in `dir`, one outcome each.
///
/// Why (#11587): the single per-binary step of the install; a failure on one
/// binary never stops the next, and each is reported on its own.
/// What: skips a name with no [`set_of`] entry or no file in `dir`. Reads the
/// signature with `codesign -dvv`; an attested Developer ID signature with the
/// binary's [`codesign_identifier`] (and the identity's Team ID, when one is
/// configured) is `AlreadySigned`. Otherwise `identity` `None` is
/// `NoIdentity`, and `Some` runs `strict::sign_one` with Hardened Runtime per
/// [`use_hardened_runtime`]`(set, false)`: `Signed` on success, `Failed` with
/// the cause otherwise.
/// Test: `tests::sign_binaries_reports_each_binary_on_its_own`,
/// `tests::sign_binaries_without_an_identity_signs_nothing`,
/// `tests::sign_binaries_leaves_an_already_signed_binary_alone`.
pub(crate) fn sign_binaries_with(
    codesign: &OsStr,
    dir: &Path,
    binaries: &[&str],
    identity: Option<&str>,
) -> Vec<BinarySigning> {
    let team = identity.and_then(strict::expected_team_id);
    let mut outcomes = Vec::new();
    for &name in binaries {
        let (Some(set), path) = (set_of(name), dir.join(name)) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        let signed_before = strict::codesign_display(codesign, &path)
            .is_ok_and(|d| already_signed(&d, codesign_identifier(name), team));
        let (status, detail) = match (signed_before, identity) {
            (true, _) => (SignStatus::AlreadySigned, String::new()),
            (false, None) => (SignStatus::NoIdentity, String::new()),
            (false, Some(id)) => {
                match strict::sign_one(codesign, &path, id, use_hardened_runtime(set, false)) {
                    Ok(()) => (SignStatus::Signed, String::new()),
                    Err(e) => (SignStatus::Failed, format!("{e:#}")),
                }
            }
        };
        outcomes.push(BinarySigning {
            binary: name.to_owned(),
            path,
            status,
            detail,
        });
    }
    outcomes
}

/// The signing state of one `tctl install` run.
///
/// Why (#11587): the identity is probed once per run, each member is signed
/// as it lands (before its daemon starts), and the closing sweep must not sign
/// a binary twice.
/// What: `codesign` is `None` off macOS, which makes every method a no-op.
/// Test: `tests::install_signer_sweep_skips_what_a_member_signed`,
/// `tests::an_inert_install_signer_reports_nothing`.
#[derive(Debug, Default)]
pub struct InstallSigner {
    codesign: Option<OsString>,
    identity: Option<String>,
    outcomes: Vec<BinarySigning>,
}

impl InstallSigner {
    /// The live signer: `codesign` and the Developer ID identity probe on
    /// macOS (`TRUSTY_SIGN_IDENTITY`, else `security find-identity`); inert
    /// elsewhere.
    pub fn probe() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                codesign: Some(OsString::from("codesign")),
                identity: super::has_developer_id_cert(),
                outcomes: Vec::new(),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::default()
        }
    }

    /// A signer over `codesign` with `identity`; the test seam.
    #[cfg(test)]
    pub(crate) fn with_codesign(codesign: &Path, identity: Option<&str>) -> Self {
        Self {
            codesign: Some(codesign.as_os_str().to_owned()),
            identity: identity.map(str::to_owned),
            outcomes: Vec::new(),
        }
    }

    /// Sign the signable binaries of `binaries` beside the `placed` binary.
    ///
    /// What: `Err` names every one of them whose outcome this run is `Failed`
    /// (#11587), including one an earlier call already failed; `NoIdentity`
    /// is `Ok`, as the install treats it (a warning only).
    pub fn sign_member(&mut self, placed: &Path, binaries: &[String]) -> Result<(), String> {
        let Some(dir) = placed.parent() else {
            return Ok(());
        };
        let names: Vec<&str> = binaries.iter().map(String::as_str).collect();
        self.sign_in(dir, &names);
        let failed: Vec<String> = self
            .outcomes
            .iter()
            .filter(|o| o.status == SignStatus::Failed)
            .filter(|o| names.iter().any(|n| o.path == dir.join(n)))
            .map(|o| format!("{}: NOT signed: {}", o.binary, o.detail))
            .collect();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    /// Sign a placed member, then run `start` only if no binary failed.
    ///
    /// Why (#11587, code-critic on PR #12137): a service started before its
    /// binary is signed runs ad-hoc, and one started after a failed sign is
    /// fail-open. `tctl install` and `tctl upgrade` both start or restart a
    /// daemon through this one gate, so neither can reorder or skip it.
    /// What: [`Self::sign_member`] first; `Err` (its failure list) means
    /// `start` never ran. Otherwise `Ok(start())`.
    /// Test: `tests::sign_then_start_signs_before_it_starts`,
    /// `tests::a_failed_sign_never_starts_the_service`,
    /// `tests::every_service_start_goes_through_the_sign_gate`.
    pub fn sign_then_start<T>(
        &mut self,
        placed: &Path,
        binaries: &[String],
        start: impl FnOnce() -> T,
    ) -> Result<T, String> {
        self.sign_member(placed, binaries)?;
        Ok(start())
    }

    /// Sign every signable binary in `dir` this run has not handled yet.
    pub fn sweep(&mut self, dir: &Path) {
        self.sign_in(dir, &all_signable_binaries());
    }

    fn sign_in(&mut self, dir: &Path, names: &[&str]) {
        let Some(codesign) = &self.codesign else {
            return;
        };
        let todo: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| !self.outcomes.iter().any(|o| o.path == dir.join(n)))
            .collect();
        #[cfg(target_os = "macos")]
        {
            for name in todo.iter().filter(|_| self.identity.is_some()) {
                let path = dir.join(name);
                if path.is_file() && set_of(name).is_some() {
                    super::warn_on_identifier_change(&path, name);
                }
            }
        }
        let new = sign_binaries_with(codesign, dir, &todo, self.identity.as_deref());
        self.outcomes.extend(new);
    }

    /// Every outcome, in the order the binaries were handled.
    pub fn into_outcomes(self) -> Vec<BinarySigning> {
        self.outcomes
    }
}

/// The report lines for `outcomes`, and whether any binary lacked an identity
/// (the caller then prints `signing_persistence_tip` once).
pub fn report_lines(outcomes: &[BinarySigning]) -> (Vec<String>, bool) {
    let lines = outcomes.iter().map(BinarySigning::line).collect();
    let no_identity = outcomes.iter().any(|o| o.status == SignStatus::NoIdentity);
    (lines, no_identity)
}

#[cfg(test)]
#[path = "install_step_tests.rs"]
mod tests;
