//! `tm doctor` row for credentials in a LaunchAgent plist (#8236).
//!
//! Why: `trusty_common::launchd::LaunchdConfig::render_plist` now refuses to
//! write a credential into a plist, which protects every FUTURE install. It
//! does nothing for the plaintext already sitting in
//! `~/Library/LaunchAgents/com.trusty.*.plist` on a host that installed before
//! the guard, or whose unit was hand-edited and is never regenerated — which is
//! the whole of the #8236 report. This is the detection half; the in-place
//! remediation is [`super::doctor_launchd_secrets_repair`].
//!
//! What: [`scan_launch_agents`] is the shared scan and
//! [`check_launchd_plist_secrets`] the read-only row. Every finding names KEYS
//! and the file's permission MODE; neither ever reads a credential into a
//! message or a log line.
//!
//! The scan classifies each credential key two ways, because `--fix` treats
//! them differently: a key the credential REGISTRY maps to a provider can be
//! migrated into the store and then removed, while one it does not map has
//! nowhere to go — removing it would disable the feature it configures while
//! protecting nothing a rotation would not. Both are reported; only the first
//! is ever stripped.
//!
//! Test: `doctor_launchd_secrets_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_common::credential_registry::provider_for_env_var;
use trusty_common::launchd_secrets::{credential_entries, is_binary_plist};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name, shared by the check and every repair step.
pub(crate) const CHECK_NAME: &str = "launchd_secrets";

/// Filename prefix of the LaunchAgents this row judges.
///
/// Why: `--fix` rewrites files, and the only files tm may rewrite are the ones
/// tm's own installers generate. A foreign agent's plist is the operator's, and
/// is neither reported nor touched.
const TRUSTY_PLIST_PREFIX: &str = "com.trusty.";

/// Widest permission bits a plist that ever held a credential may carry.
///
/// Why: the #8236 host's `com.trusty.mpm.plist` was `0644` — readable by every
/// process running as the user and by every backup. `0600` is the only mode
/// that is not. Reported even after the credential is removed, because the
/// mode is what decided the blast radius of the next mistake.
/// Test: `row_flags_a_world_readable_plist`.
const EXPECTED_MODE: u32 = 0o600;

/// What one installed plist was found to hold.
///
/// Why: the check and the repair share one scan shape so they cannot disagree
/// about which files are implicated.
/// What: the path, its permission mode, the credential keys split by whether
/// the registry can route them, and — when the file could not be judged at all
/// — the reason. Never a value.
/// Test: `scan_names_the_key_not_the_value`,
/// `scan_splits_registry_mapped_keys_from_unmapped_ones`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlistFinding {
    /// The plist scanned.
    pub path: PathBuf,
    /// Unix permission bits, when they could be read.
    pub mode: Option<u32>,
    /// Credential keys the registry maps to a provider — `--fix` migrates these.
    pub migratable: Vec<String>,
    /// Credential-shaped keys with no registry mapping — reported, never stripped.
    pub unmapped: Vec<String>,
    /// Why the file could not be judged, when it could not be.
    pub unreadable: Option<String>,
    /// The file is a binary (`bplist00`) property list.
    ///
    /// Why (#8236 item 4, owner ruling 2026-09-21): this round neither reads
    /// nor rewrites a binary plist, so the exposure can be neither confirmed
    /// nor removed, and the remedy is one command the operator must run. That
    /// is a FAILING row, not an unknown one — an unknown invites "probably
    /// fine", and a plist that once held a credential most likely still does.
    /// Test: `row_fails_on_a_binary_plist`.
    pub binary_plist: bool,
}

impl PlistFinding {
    /// Does this finding require operator action?
    pub(crate) fn actionable(&self) -> bool {
        !self.migratable.is_empty() || !self.unmapped.is_empty() || self.unreadable.is_some()
    }

    /// Every credential key found, migratable or not, in document order.
    pub(crate) fn keys(&self) -> Vec<String> {
        let mut all = self.migratable.clone();
        all.extend(self.unmapped.iter().cloned());
        all
    }

    /// `0644`-style rendering of the mode, or `unknown`.
    pub(crate) fn mode_text(&self) -> String {
        self.mode
            .map_or_else(|| "unknown".to_string(), |m| format!("{m:04o}"))
    }

    /// Is the file readable by anyone other than its owner?
    fn too_wide(&self) -> bool {
        self.mode.is_some_and(|m| m & 0o777 & !EXPECTED_MODE != 0)
    }
}

/// Scan `<home>/Library/LaunchAgents` for trusty plists holding credentials.
///
/// Why: one scan, used by the row and by the repair, so `tm doctor` and
/// `tm doctor --fix` can never report different files.
/// What: reads every `com.trusty.*.plist` in the directory AS BYTES, refuses a
/// binary plist with an actionable reason before any text parsing (#8236 item
/// 4 — a `bplist00` file decoded as text has no `<key>` in it and would read as
/// CLEAN), and otherwise runs [`credential_entries`] over it, keeping only the
/// key names. A file that cannot be read or parsed yields a finding with
/// `unreadable` set — never a silent skip, because "could not read" is the one
/// answer that must not render as clean.
///
/// # Errors
///
/// When the directory cannot be listed, or when any single entry in it cannot
/// be resolved — an entry dropped from the walk is a file the scan did not
/// judge, and the caller has to hear that as UNKNOWN rather than as one fewer
/// clean plist. An ABSENT directory is `Ok(vec![])` (a host with no
/// LaunchAgents has no exposure).
///
/// Test: `scan_names_the_key_not_the_value`, `scan_reports_an_unparseable_plist`,
/// `scan_reports_an_unreadable_plist`, `scan_ignores_foreign_plists`,
/// `scan_is_empty_without_a_launch_agents_dir`,
/// `scan_errors_when_the_directory_cannot_be_listed`,
/// `scan_reports_a_binary_plist_as_unknown`,
/// `scan_splits_registry_mapped_keys_from_unmapped_ones`.
pub fn scan_launch_agents(home: &Path) -> std::io::Result<Vec<PlistFinding>> {
    let dir = home.join("Library/LaunchAgents");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut out = Vec::new();
    for entry in entries {
        // #8236: `?`, never `.flatten()` — a dropped entry is a plist nobody
        // judged, and the scan would report the host clean without it.
        let path = entry?.path();
        // Lossy, not `to_str()` — a name this cannot decode would be SKIPPED by
        // a `?`-less `to_str()` arm, which is the same silent drop as above.
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !name.starts_with(TRUSTY_PLIST_PREFIX) || !name.ends_with(".plist") {
            continue;
        }
        out.push(judge(path));
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Read and judge one plist.
fn judge(path: PathBuf) -> PlistFinding {
    let mode = read_mode(&path);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return unreadable(
                path,
                mode,
                format!("could not read it: {}", e.kind()),
                false,
            );
        }
    };
    if is_binary_plist(&bytes) {
        return unreadable(
            path,
            mode,
            format!(
                "it is a BINARY plist, which this scan cannot read — convert it with \
                 `plutil -convert xml1 {}` and re-run `tm doctor`",
                path.display()
            ),
            true,
        );
    }
    let Ok(xml) = String::from_utf8(bytes) else {
        return unreadable(path, mode, "it is not valid UTF-8".to_string(), false);
    };
    finding_for(path, mode, &xml)
}

/// A finding that names why the file could not be judged.
fn unreadable(path: PathBuf, mode: Option<u32>, why: String, binary_plist: bool) -> PlistFinding {
    PlistFinding {
        path,
        mode,
        migratable: Vec::new(),
        unmapped: Vec::new(),
        unreadable: Some(why),
        binary_plist,
    }
}

/// Unix permission bits of `path`, when they can be read.
fn read_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Judge one plist's text.
///
/// Why: separated from the directory walk so the parse-failure arm is testable
/// without a filesystem that can produce one.
/// What: splits the credential keys on [`provider_for_env_var`] — see the module
/// docs for why the two halves are treated differently.
/// Test: `scan_reports_an_unparseable_plist`,
/// `scan_splits_registry_mapped_keys_from_unmapped_ones`.
fn finding_for(path: PathBuf, mode: Option<u32>, xml: &str) -> PlistFinding {
    match credential_entries(xml) {
        Ok(entries) => {
            let (migratable, unmapped) = entries
                .into_iter()
                .map(|e| e.key)
                .partition(|key| provider_for_env_var(key).is_some());
            PlistFinding {
                path,
                mode,
                migratable,
                unmapped,
                unreadable: None,
                binary_plist: false,
            }
        }
        Err(e) => unreadable(path, mode, e.reason, false),
    }
}

/// The `launchd_secrets` doctor row.
///
/// Why: #8236 was found by a human running `plutil -p` on a plist while looking
/// for a log path. Nothing in the diagnostic looked, so the exposure had no
/// expiry date.
/// What: `Fail` when any trusty LaunchAgent carries a credential-keyed
/// `EnvironmentVariables` entry, naming the file, the KEY, and the file's MODE.
/// `Unknown` — never `Ok` — when the directory or a plist could not be read or
/// parsed, because a scan that did not run has not shown the host clean. `Ok`
/// otherwise, still flagging any plist wider than `0600`.
/// Test: `row_fails_and_names_the_key_not_the_value`, `row_is_ok_when_clean`,
/// `row_is_unknown_when_a_plist_cannot_be_parsed`,
/// `row_is_unknown_when_the_directory_cannot_be_listed`,
/// `row_flags_a_world_readable_plist`,
/// `row_says_an_unmapped_key_is_not_stripped`.
pub(crate) fn check_launchd_plist_secrets(home: &Path) -> DoctorCheck {
    match scan_launch_agents(home) {
        Ok(findings) => build_row(&findings),
        Err(e) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "could not list ~/Library/LaunchAgents ({}) — whether a LaunchAgent \
                 holds a plaintext credential is UNKNOWN",
                e.kind()
            ),
        ),
    }
}

/// The pure verdict behind [`check_launchd_plist_secrets`].
///
/// Why: keeps the arms unit-testable without a real home directory.
/// What: see [`check_launchd_plist_secrets`]. A credential found outranks an
/// unreadable file — a confirmed exposure is worse news than an unknown one —
/// but the Fail message still NAMES the files it could not judge, so ranking
/// the statuses never drops the unjudged file from the report.
/// Test: `row_fails_and_names_the_key_not_the_value`, `row_is_ok_when_clean`,
/// `row_is_unknown_when_a_plist_cannot_be_parsed`,
/// `row_fails_on_a_binary_plist`,
/// `row_fail_still_names_the_plists_it_could_not_judge`.
fn build_row(findings: &[PlistFinding]) -> DoctorCheck {
    let unreadable: Vec<String> = findings
        .iter()
        .filter_map(|f| {
            f.unreadable
                .as_ref()
                .map(|why| format!("{} ({why})", f.path.display()))
        })
        .collect();

    let exposed: Vec<&PlistFinding> = findings
        .iter()
        .filter(|f| !f.migratable.is_empty() || !f.unmapped.is_empty())
        .collect();
    if !exposed.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            fail_message(&exposed, &unreadable),
        );
    }

    // #8236 item 4 (owner ruling 2026-09-21): a binary plist is the one
    // unreadable cause this round can neither judge nor repair, and its remedy
    // is a single command. It fails the row rather than leaving an unknown,
    // because an unknown on a file that once held a credential gets ignored.
    let binary: Vec<String> = findings
        .iter()
        .filter(|f| f.binary_plist)
        .map(|f| f.path.display().to_string())
        .collect();
    if !binary.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "{} LaunchAgent plist(s) are BINARY and cannot be read or repaired here: {} — \
                 whether they hold a plaintext credential is UNKNOWN, and `tm doctor --fix` \
                 refuses them. Convert each with `plutil -convert xml1 <path>`, then re-run \
                 `tm doctor`",
                binary.len(),
                binary.join("; ")
            ),
        );
    }

    if !unreadable.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "could not judge {} LaunchAgent plist(s): {} — whether they hold a \
                 plaintext credential is UNKNOWN",
                unreadable.len(),
                unreadable.join("; ")
            ),
        );
    }

    let wide: Vec<String> = findings
        .iter()
        .filter(|f| f.too_wide())
        .map(|f| format!("{} is mode {}", f.path.display(), f.mode_text()))
        .collect();
    if wide.is_empty() {
        DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "no plaintext credential in {} trusty LaunchAgent plist(s)",
                findings.len()
            ),
        )
    } else {
        DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "no plaintext credential found, but {} plist(s) are readable beyond their \
                 owner ({}); `chmod 600` them so the next mistake is not a disclosure",
                wide.len(),
                wide.join("; ")
            ),
        )
    }
}

/// The Fail message: what was found, in which file, at which mode.
///
/// Why: split out so the message stays readable and `build_row` stays short.
/// What: one clause per implicated file, then the unmapped-key caveat, then the
/// rotation instruction — which applies whether or not `--fix` can strip
/// anything, because the value was readable for as long as it sat there.
/// Test: `row_fails_and_names_the_key_not_the_value`,
/// `row_says_an_unmapped_key_is_not_stripped`.
fn fail_message(exposed: &[&PlistFinding], unreadable: &[String]) -> String {
    let detail = exposed
        .iter()
        .map(|f| {
            format!(
                "{} (mode {}; {})",
                f.path.display(),
                f.mode_text(),
                f.keys().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    let unmapped: Vec<String> = exposed
        .iter()
        .flat_map(|f| f.unmapped.iter().cloned())
        .collect();
    let caveat = if unmapped.is_empty() {
        String::new()
    } else {
        format!(
            ". `tm doctor --fix` will NOT remove {} — no credential provider is registered \
             for it, so there is nowhere to migrate the value and stripping it would break \
             the feature it configures; move it out by hand",
            unmapped.join(", ")
        )
    };
    let unjudged = if unreadable.is_empty() {
        String::new()
    } else {
        format!(
            ". {} further plist(s) could not be judged at all: {}",
            unreadable.len(),
            unreadable.join("; ")
        )
    };

    format!(
        "a LaunchAgent plist holds a plaintext credential — the file is user-readable and \
         lands in every backup: {detail}. Run `tm doctor --fix --yes` to migrate each \
         registered credential into the credential store and remove it from the plist, then \
         ROTATE those credentials: removing a value does not un-expose it{caveat}{unjudged}"
    )
}

#[cfg(test)]
#[path = "doctor_launchd_secrets_tests.rs"]
mod tests;
