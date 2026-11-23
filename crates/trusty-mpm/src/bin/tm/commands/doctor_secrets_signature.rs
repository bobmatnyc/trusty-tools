//! The `secrets_signature` row of `tm doctor` (#11580, split from #11429).
//!
//! Why: `cargo install` re-signs the ad-hoc signed `trusty-secrets` binary,
//! so its code-directory hash (cdhash) changes. The Keychain ACL on each vault
//! item still pins the old cdhash, and the next read or write raises a macOS
//! Keychain prompt that blocks the call until someone clicks it. Doctor said
//! nothing about it, so the first sign was a timed-out `tm secrets set`.
//! What: finds `trusty-secrets` on `PATH`, reads its cdhash with
//! `codesign -dvvv`, and reads the ACLs of the trusty Keychain items with
//! `security dump-keychain -a` (attributes and ACLs only, never a value; no
//! `-d`). Each item whose service starts with `trusty-secrets` or `trusty/`
//! is graded: its ACL names the current cdhash (or any app) → trusted; it
//! names this binary or another cdhash → the next access prompts (WARN); it
//! names neither → access is denied (FAIL). No `security` on `PATH` means no
//! macOS Keychain and the row is OK. Every call is bounded by [`CALL_BUDGET`];
//! a timeout or a refused probe is UNKNOWN, never OK.
//! Test: `doctor_secrets_signature_tests.rs`;
//! `tests/tm_doctor_secrets_signature.rs` drives the real binary against
//! `codesign` and `security` shims on `PATH`.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use trusty_mpm::core::bounded_proc::{BoundedError, run_bounded};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

use super::doctor_gcloud::Run;

/// This check's name.
pub(crate) const CHECK: &str = "secrets_signature";

/// Ceiling on one `codesign` or `security` call. Both read local state; ten
/// seconds also catches a `security` call parked on a Keychain dialog.
pub(crate) const CALL_BUDGET: Duration = Duration::from_secs(10);

/// The remedy every drift row names.
pub(crate) const REMEDY: &str = "run `tm secrets unlock` and click Always Allow to re-approve \
     the new signature (stable signing identity: #11587)";

/// A 40-hex cdhash in an ACL: `cdhash H"<hex>"` or a `cdhash:<hex>` partition.
static ACL_CDHASH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)cdhash(?:\s+H"|:)([0-9a-f]{40})"#).expect("static regex compiles")
});

/// The `CDHash=<hex>` line `codesign -dvvv` writes.
static CODESIGN_CDHASH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^CDHash=([0-9a-fA-F]{40})\s*$").expect("static regex compiles")
});

/// The `"svce"<blob>="<service>"` attribute line of one dumped item.
static SERVICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\s*"svce"<blob>="([^"]*)""#).expect("static regex compiles")
});

/// The seam between the row's grading and the real processes.
///
/// Test: `doctor_secrets_signature_tests.rs` scripts every outcome.
pub(crate) trait ProbeRunner {
    /// Run `program <args>` once and report how it ended.
    fn run(&mut self, program: &str, args: &[&OsStr]) -> Run;
}

/// The runner `tm doctor` uses: the real programs, bounded, stdin closed.
pub(crate) struct LiveProbe;

impl ProbeRunner for LiveProbe {
    fn run(&mut self, program: &str, args: &[&OsStr]) -> Run {
        let mut cmd = Command::new(program);
        cmd.args(args).stdin(Stdio::null());
        match run_bounded(cmd, CALL_BUDGET) {
            Ok(out) => Run::Exited {
                success: out.status.success(),
                code: out.status.code(),
                stdout: out.stdout,
                stderr: out.stderr,
            },
            Err(BoundedError::Spawn(e)) if e.kind() == ErrorKind::NotFound => Run::NotFound,
            Err(BoundedError::TimedOut) => Run::TimedOut,
            Err(e) => Run::Error(format!("`{program}` {e}")),
        }
    }
}

/// How one trusty Keychain item's ACL treats the installed binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemTrust {
    /// The ACL names the current cdhash, or allows every application.
    Trusted,
    /// The ACL names this binary or an older cdhash: the next access prompts.
    Prompt,
    /// The ACL does not name this binary at all: access is denied.
    Denied,
}

/// Grade every trusty item in a `security dump-keychain -a` output.
///
/// What: splits the dump at each `keychain:` line, keeps the items whose
/// `svce` starts with `trusty-secrets` or `trusty/`, and grades each one
/// against `cdhash` (lowercase hex) and the binary's path text.
/// Test: `grade_items_*` in `doctor_secrets_signature_tests.rs`.
pub(crate) fn grade_items(dump: &str, cdhash: &str, binary: &Path) -> Vec<(String, ItemTrust)> {
    let binary = binary.display().to_string();
    let mut graded = Vec::new();
    for block in split_items(dump) {
        let Some(service) = SERVICE.captures(block).and_then(|c| c.get(1)) else {
            continue;
        };
        let service = service.as_str();
        if !(service.starts_with("trusty-secrets") || service.starts_with("trusty/")) {
            continue;
        }
        let pinned: Vec<String> = ACL_CDHASH
            .captures_iter(block)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_ascii_lowercase()))
            .collect();
        let trust = if pinned.iter().any(|h| h == cdhash) || block.contains("applications: <null>")
        {
            ItemTrust::Trusted
        } else if !pinned.is_empty() || block.contains(&binary) {
            ItemTrust::Prompt
        } else {
            ItemTrust::Denied
        };
        graded.push((service.to_string(), trust));
    }
    graded
}

/// The dump cut into one slice per item, each starting at its `keychain:` line.
fn split_items(dump: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = dump
        .match_indices("keychain: ")
        .filter(|(i, _)| *i == 0 || dump.as_bytes()[i - 1] == b'\n')
        .map(|(i, _)| i)
        .collect();
    starts.push(dump.len());
    starts.windows(2).map(|w| &dump[w[0]..w[1]]).collect()
}

/// The first `name` file found in the directories of `path_var`.
///
/// Test: `find_on_path_*` in `doctor_secrets_signature_tests.rs`.
pub(crate) fn find_on_path(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// The `secrets_signature` row, read from this process's `PATH`.
///
/// Why: the entry `tm doctor` calls; the seam form is [`signature_row_with`].
/// Test: `tm_doctor_flags_a_trusty_secrets_signature_drift` drives it through
/// the real binary.
pub(crate) fn secrets_signature_row() -> DoctorCheck {
    let path_var = std::env::var_os("PATH");
    signature_row_with(
        find_on_path("trusty-secrets", path_var.as_deref()).as_deref(),
        &mut LiveProbe,
    )
}

/// Grade the installed `trusty-secrets` signature against the vault ACLs.
///
/// Why (#11580): a re-signed binary meets a Keychain prompt on its next
/// access; the row flags that before a caller times out on it.
/// What: `binary` is the `trusty-secrets` on `PATH`, or `None`. Reads the
/// cdhash, then the ACLs, and folds the per-item grades: any Denied → FAIL,
/// any Prompt → WARN, else OK. Each probe failure is UNKNOWN with its reason.
/// Test: `signature_row_*` in `doctor_secrets_signature_tests.rs`.
pub(crate) fn signature_row_with(
    binary: Option<&Path>,
    runner: &mut dyn ProbeRunner,
) -> DoctorCheck {
    let row = |status, msg: String| DoctorCheck::new(CHECK, status, msg);
    let Some(binary) = binary else {
        return row(
            CheckStatus::Ok,
            "trusty-secrets not on PATH; nothing to check".into(),
        );
    };
    // #11580: the ACL list first — no `security` means no macOS Keychain.
    let dump_args = [OsStr::new("dump-keychain"), OsStr::new("-a")];
    let dump = match runner.run("security", &dump_args) {
        Run::Exited {
            success: true,
            stdout,
            ..
        } => stdout,
        Run::NotFound => {
            return row(
                CheckStatus::Ok,
                "no macOS Keychain (`security` not on PATH)".into(),
            );
        }
        other => {
            return row(
                CheckStatus::Unknown,
                format!("cannot read Keychain ACLs: {}", reason("security", &other)),
            );
        }
    };
    let cdhash_args = [OsStr::new("-dvvv"), binary.as_os_str()];
    let cdhash = match runner.run("codesign", &cdhash_args) {
        Run::Exited {
            success: true,
            stdout,
            stderr,
            ..
        } => {
            let text = format!("{stdout}\n{stderr}");
            match CODESIGN_CDHASH.captures(&text).and_then(|c| c.get(1)) {
                Some(m) => m.as_str().to_ascii_lowercase(),
                None => {
                    return row(
                        CheckStatus::Unknown,
                        format!("no CDHash in `codesign -dvvv {}`", binary.display()),
                    );
                }
            }
        }
        other => {
            return row(
                CheckStatus::Unknown,
                format!("cannot read the signature: {}", reason("codesign", &other)),
            );
        }
    };
    grade_row(&grade_items(&dump, &cdhash, binary), &cdhash, binary)
}

/// Fold the per-item grades into one row.
fn grade_row(graded: &[(String, ItemTrust)], cdhash: &str, binary: &Path) -> DoctorCheck {
    let short = &cdhash[..cdhash.len().min(12)];
    let with = |trust: ItemTrust| {
        graded
            .iter()
            .filter(|(_, t)| *t == trust)
            .map(|(s, _)| s.as_str())
            .collect::<Vec<&str>>()
    };
    let (denied, prompt) = (with(ItemTrust::Denied), with(ItemTrust::Prompt));
    if graded.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            "no trusty-secrets Keychain items yet",
        );
    }
    if !denied.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Fail,
            format!(
                "access denied: the ACL of {} does not trust {} (cdhash {short}); {REMEDY}",
                denied.join(", "),
                binary.display()
            ),
        );
    }
    if !prompt.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Warn,
            format!(
                "signature drift: {} pins an older cdhash than {short}, so the next access \
                 raises a Keychain prompt; {REMEDY}",
                prompt.join(", ")
            ),
        );
    }
    DoctorCheck::new(
        CHECK,
        CheckStatus::Ok,
        format!("{} item(s) trust cdhash {short}", graded.len()),
    )
}

/// One line on why a probe gave no usable answer.
fn reason(program: &str, run: &Run) -> String {
    match run {
        Run::Exited { code, stderr, .. } => {
            let first = stderr.lines().next().unwrap_or("").trim();
            match code {
                Some(c) => format!("`{program}` exited {c}: {first}"),
                None => format!("`{program}` was killed by a signal"),
            }
        }
        Run::NotFound => format!("`{program}` not on PATH"),
        Run::TimedOut => format!(
            "`{program}` timed out after {}s (a Keychain dialog may be open)",
            CALL_BUDGET.as_secs()
        ),
        Run::Error(e) => e.clone(),
    }
}

#[cfg(test)]
#[path = "doctor_secrets_signature_tests.rs"]
mod tests;
