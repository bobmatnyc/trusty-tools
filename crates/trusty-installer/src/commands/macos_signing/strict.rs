//! The strict sign → verify → attest pipeline behind `tctl sign <target>`.
//!
//! Why (#11587): `codesign --verify --deep --strict` accepts an ad-hoc
//! signature, so before this module `tctl sign` could print "signed with
//! Developer ID" for a binary that carried no Team ID. An ad-hoc identity is
//! keyed to the cdhash, which is the identity that re-prompts every Keychain
//! item after an upgrade (#11786 AC 6-8). The step now reads the signature back
//! and refuses to report success unless it names a Developer ID Application
//! leaf and the expected Team ID.
//!
//! What: [`sign_set_with`] runs the whole pipeline for one set against an
//! injectable `codesign` program, so its error arms run in tests with a fake.
//! [`attest_developer_id`] and [`expected_team_id`] are the pure checks.
//! `mod.rs` keeps the macOS-only wrappers that pass the real `codesign`.
//!
//! Test: `tests::sign_set_with_fails_closed_on_every_bad_signature`,
//! `tests::sign_set_with_accepts_a_matching_developer_id_signature`,
//! `tests::attest_developer_id_rejects_every_non_developer_id_shape`,
//! `tests::expected_team_id_reads_only_a_well_formed_suffix`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context;

use super::{binaries_for_set, codesign_identifier, use_hardened_runtime, SignSetError};

/// Sign one binary with `codesign --force [--options runtime --timestamp]
/// --identifier <id> --sign <identity>`; `Err` carries codesign's stderr.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn codesign_sign(
    codesign: &OsStr,
    path: &Path,
    identity: &str,
    hardened: bool,
) -> anyhow::Result<()> {
    let binary_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let identifier = codesign_identifier(binary_name);

    let mut args: Vec<&str> = vec!["--force"];
    if hardened {
        args.extend(["--options", "runtime", "--timestamp"]);
    }
    args.extend(["--identifier", identifier, "--sign", identity]);

    let out = std::process::Command::new(codesign)
        .args(&args)
        .arg(path)
        .output()
        .with_context(|| format!("running codesign on {}", path.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("codesign failed for {}: {stderr}", path.display());
    }
    Ok(())
}

/// Run `codesign --verify --deep --strict`; `Err` carries the verifier output.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn codesign_verify(codesign: &OsStr, path: &Path) -> anyhow::Result<()> {
    let out = std::process::Command::new(codesign)
        .args(["--verify", "--deep", "--strict"])
        .arg(path)
        .output()
        .with_context(|| format!("running codesign --verify on {}", path.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "post-sign verification failed for {}: {stderr}",
            path.display()
        );
    }
    Ok(())
}

/// Read `codesign -dvv <path>` (it writes its summary to stderr).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn codesign_display(codesign: &OsStr, path: &Path) -> anyhow::Result<String> {
    let out = std::process::Command::new(codesign)
        .arg("-dvv")
        .arg(path)
        .output()
        .with_context(|| format!("running codesign -dvv on {}", path.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        anyhow::bail!(
            "could not read the signature of {} back: {stderr}",
            path.display()
        );
    }
    Ok(stderr)
}

/// The Team ID a Developer ID identity name carries, e.g. `4JH68XUHC5` from
/// `Developer ID Application: Bob Matsuoka (4JH68XUHC5)`.
///
/// Why (#11587): the attestation compares the signed binary's
/// `TeamIdentifier` with the team the operator asked for, and the identity
/// name is the only place that request is written down.
/// What: the text inside a trailing `(…)` when it is exactly 10 ASCII
/// uppercase letters or digits (Apple's Team ID shape); `None` otherwise,
/// e.g. for a SHA-1 fingerprint passed through `TRUSTY_SIGN_IDENTITY`.
/// Test: `tests::expected_team_id_reads_only_a_well_formed_suffix`.
pub fn expected_team_id(identity: &str) -> Option<&str> {
    let inner = identity.trim_end().strip_suffix(')')?;
    let (_, team) = inner.rsplit_once('(')?;
    let well_formed = team.len() == 10
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    well_formed.then_some(team)
}

/// Check `codesign -dvv` output for a Developer ID signature.
///
/// Why (#11587): fail-open guard. A signature that is ad-hoc, self-signed, or
/// from another team gives the binary a designated requirement that does not
/// survive the next upgrade, so reporting it as success hides the exact
/// failure this signing step exists to prevent.
/// What: `Err(reason)` when the output shows an ad-hoc signature
/// (`Signature=adhoc` or an `adhoc` CodeDirectory flag), when the first
/// `Authority=` line is not a `Developer ID Application:` certificate, when
/// `TeamIdentifier` is missing or `not set`, or when `expected_team` is `Some`
/// and differs from it. `Ok(())` otherwise.
/// Test: `tests::attest_developer_id_rejects_every_non_developer_id_shape`,
/// `tests::attest_developer_id_accepts_the_expected_team`.
pub fn attest_developer_id(display: &str, expected_team: Option<&str>) -> Result<(), String> {
    let field = |key: &str| {
        display
            .lines()
            .find_map(|line| line.trim().strip_prefix(key))
            .map(str::trim)
    };
    let adhoc_flag = display
        .lines()
        .any(|line| line.trim_start().starts_with("CodeDirectory") && line.contains("adhoc"));
    if field("Signature=") == Some("adhoc") || adhoc_flag {
        return Err("the signature is ad-hoc (no Developer ID certificate, no Team ID)".to_owned());
    }
    match field("Authority=") {
        Some(leaf) if leaf.starts_with("Developer ID Application:") => {}
        Some(leaf) => {
            return Err(format!(
                "the leaf certificate is `{leaf}`, not a Developer ID Application certificate"
            ));
        }
        None => return Err("the signature names no signing certificate".to_owned()),
    }
    let team = match field("TeamIdentifier=") {
        None | Some("") | Some("not set") => {
            return Err("the signature carries no Team ID (TeamIdentifier not set)".to_owned());
        }
        Some(team) => team,
    };
    match expected_team {
        Some(expected) if expected != team => Err(format!(
            "the signature carries Team ID {team}, but the identity asked for {expected}"
        )),
        _ => Ok(()),
    }
}

/// Sign, verify and attest every present binary of `set` with `codesign`.
///
/// Why (#11587): the strict `tctl sign` path in one platform-neutral function,
/// so a fake `codesign` can drive each failure arm in a test.
/// What: for every binary of `set` found in `install_dir`, signs it with
/// Hardened Runtime per [`use_hardened_runtime`]`(set, true)`, verifies it,
/// reads the signature back and runs [`attest_developer_id`] against
/// [`expected_team_id`]`(identity)`. Stops at the first failure with
/// [`SignSetError::Sign`]. Returns the signed paths;
/// [`SignSetError::UnknownSet`] / [`SignSetError::NoBinariesFound`] as
/// `sign_set_strict` documents.
/// Test: `tests::sign_set_with_fails_closed_on_every_bad_signature`,
/// `tests::sign_set_with_accepts_a_matching_developer_id_signature`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn sign_set_with(
    codesign: &OsStr,
    install_dir: &Path,
    set: &str,
    identity: &str,
) -> Result<Vec<PathBuf>, SignSetError> {
    let binaries = binaries_for_set(set);
    if binaries.is_empty() {
        return Err(SignSetError::UnknownSet(set.to_owned()));
    }
    let hardened = use_hardened_runtime(set, true);
    let expected_team = expected_team_id(identity);

    let mut signed = Vec::new();
    for name in binaries {
        let path = install_dir.join(name);
        if !path.exists() {
            continue;
        }
        codesign_sign(codesign, &path, identity, hardened).map_err(SignSetError::Sign)?;
        codesign_verify(codesign, &path).map_err(SignSetError::Sign)?;
        // #11587: `--verify --strict` passes an ad-hoc signature; read it back.
        let display = codesign_display(codesign, &path).map_err(SignSetError::Sign)?;
        attest_developer_id(&display, expected_team).map_err(|reason| {
            SignSetError::Sign(anyhow::anyhow!(
                "{} is not Developer-ID signed after codesign: {reason}. Refusing to report \
                 success; check `codesign -dvv {}`",
                path.display(),
                path.display()
            ))
        })?;
        signed.push(path);
    }
    if signed.is_empty() {
        return Err(SignSetError::NoBinariesFound {
            set: set.to_owned(),
            dir: install_dir.display().to_string(),
        });
    }
    Ok(signed)
}
