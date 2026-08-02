//! Where the running binary came from, and whether that source still exists
//! (issue #4033).
//!
//! Why: `tm doctor`'s health model is liveness plus file state — it answers "is
//! it running?" but never "is what's running what you think it is?". On
//! 2026-07-26 ten trusty-* binaries on the owner's machine were installed with
//! `cargo install --path` from git worktrees rather than from crates.io:
//! trusty-search sat at 0.39.0 while 0.39.1 shipped two crash-safety fixes,
//! tctl was seven patch versions stale, trusty-channels was installed TWICE
//! from two different worktrees serving conflicting binaries, and one install
//! source lived under `/Users/masa/tmp/`, a directory macOS reaps. A path
//! install is invisible to registry update detection, and once its source
//! directory is gone there is no upgrade path at all — yet every probe stayed
//! green. A test run, benchmark, or bug repro against such a binary produces a
//! confident wrong answer.
//!
//! What: [`parse_cargo_installs`] reads cargo's own install ledger
//! (`$CARGO_HOME/.crates2.json`) into [`CargoInstall`] records, classifying each
//! package's [`InstallSource`] from the package-id spec cargo stores as the map
//! key. [`provenance_report`] turns those records plus the running binary's
//! identity into a `tm doctor` verdict. It is a pure function over injected
//! inputs — including the filesystem probe — so every branch is testable.
//!
//! UNVERIFIABLE REPORTS UNKNOWN. A missing or unparseable ledger, or a binary
//! the ledger does not cover (a prebuilt-installer drop into `~/.local/bin`, a
//! package-manager install, a build run in place), yields
//! [`CheckStatus::Unknown`] and never `Ok` — the check has learned nothing about
//! provenance in those cases and must not render as a clean bill of health.
//!
//! Test: the `tests` module below covers every source classification and every
//! verdict branch.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::core::doctor::CheckStatus;

/// How cargo obtained a package it installed.
///
/// Why: the four cases carry different upgrade stories. A registry install is
/// the only one `cargo install-update` / a version comparison against crates.io
/// can reason about; a path install bypasses update detection entirely and can
/// outlive its own source; a git install pins a commit that may be unreachable.
/// What: parsed from the source fragment of cargo's package-id spec (the
/// parenthesised tail of a `.crates2.json` map key).
/// Test: `classifies_registry_source`, `classifies_path_source`,
/// `classifies_git_source`, `classifies_unknown_source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// `registry+https://github.com/rust-lang/crates.io-index` — the only
    /// source a published-version comparison can be made against.
    Registry,
    /// `git+<url>#<rev>` — pinned to a commit, not a published version.
    Git(String),
    /// `path+file://<dir>` — a local `cargo install --path` install.
    Path(PathBuf),
    /// A source fragment this parser does not recognise. Never treated as
    /// healthy: an unclassified source is an unverified one.
    Other(String),
}

/// One package cargo records as installed.
///
/// Why: `provenance_report` needs the package's version and source alongside
/// the binary names it provides, because the question ("is the `tm` I am
/// running the `tm` cargo installed?") is asked by BINARY name while cargo keys
/// its ledger by PACKAGE.
/// What: name, version, source, and the binaries the install placed on disk.
/// Test: `parses_a_real_ledger`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoInstall {
    /// Package name (e.g. `trusty-mpm`).
    pub package: String,
    /// Installed version (e.g. `1.3.1`).
    pub version: String,
    /// Where cargo got it.
    pub source: InstallSource,
    /// Binary names this install placed in `$CARGO_HOME/bin`.
    pub bins: Vec<String>,
}

/// Shape of the `.crates2.json` entries this module reads.
#[derive(Deserialize)]
struct Crates2Entry {
    #[serde(default)]
    bins: Vec<String>,
}

/// Shape of the `.crates2.json` document this module reads.
#[derive(Deserialize)]
struct Crates2Doc {
    #[serde(default)]
    installs: std::collections::BTreeMap<String, Crates2Entry>,
}

/// Parse cargo's `.crates2.json` install ledger.
///
/// Why: this file is cargo's OWN record of what it installed and from where —
/// authoritative for the question, and already on disk, so the check needs no
/// network and no heuristics over `~/.cargo/bin` filenames.
/// What: each map key is a package-id spec of the form
/// `"<name> <version> (<source>)"`; the value carries the binary names. Entries
/// whose key does not match that shape are skipped rather than guessed at. A
/// document that does not parse at all yields `None`, which the caller reports
/// as `Unknown` — never as an empty (and therefore clean-looking) ledger.
/// Test: `parses_a_real_ledger`, `parse_returns_none_for_garbage`,
/// `parse_skips_malformed_keys`.
pub fn parse_cargo_installs(json: &str) -> Option<Vec<CargoInstall>> {
    let doc: Crates2Doc = serde_json::from_str(json).ok()?;
    let mut out = Vec::new();
    for (key, entry) in doc.installs {
        let Some((head, source)) = key.rsplit_once(" (") else {
            continue;
        };
        let source = source.strip_suffix(')').unwrap_or(source);
        let Some((package, version)) = head.rsplit_once(' ') else {
            continue;
        };
        out.push(CargoInstall {
            package: package.to_string(),
            version: version.to_string(),
            source: classify_source(source),
            bins: entry.bins,
        });
    }
    out.sort_by(|a, b| a.package.cmp(&b.package).then(a.version.cmp(&b.version)));
    Some(out)
}

/// Classify a package-id source fragment.
///
/// Why: kept separate from [`parse_cargo_installs`] so every prefix shape is
/// unit-testable without constructing a JSON document.
/// What: recognises `registry+`, `git+`, and `path+file://` prefixes; anything
/// else becomes [`InstallSource::Other`] carrying the raw fragment.
/// Test: `classifies_registry_source`, `classifies_path_source`,
/// `classifies_git_source`, `classifies_unknown_source`.
fn classify_source(fragment: &str) -> InstallSource {
    if fragment.starts_with("registry+") {
        InstallSource::Registry
    } else if let Some(rest) = fragment.strip_prefix("git+") {
        InstallSource::Git(rest.to_string())
    } else if let Some(rest) = fragment.strip_prefix("path+file://") {
        InstallSource::Path(PathBuf::from(rest))
    } else {
        InstallSource::Other(fragment.to_string())
    }
}

/// Build the `tm doctor` verdict for the running binary's provenance.
///
/// Why: see the module doc — this is the "is what's running what you think it
/// is?" observation doctor's health model lacked (#4033). Every filesystem and
/// environment input is a parameter so the whole decision table is testable
/// without touching a real `~/.cargo`.
/// What: `bin_name` is the running executable's file name, `running_version` its
/// compiled-in `CARGO_PKG_VERSION`, `running_exe` its resolved path (for the
/// message), `ledger` the parsed installs (`None` = unreadable), and
/// `source_exists` the probe used to ask whether a path install's source
/// directory survives. The verdict, worst-first:
///
/// - `Unknown` — no ledger, or no ledger record provides `bin_name`. Provenance
///   was not determined; saying `Ok` here is the defect class this whole change
///   exists to close.
/// - `Fail` — the same binary is provided by MORE THAN ONE install (the #4033
///   trusty-channels case: two worktrees, conflicting binaries), or the ledger's
///   recorded version disagrees with the version actually running.
/// - `Fail` — a path install whose source directory is GONE: no provenance, no
///   upgrade path, and nothing left to rebuild from.
/// - `Warn` — a path, git, or unrecognised-source install whose source survives:
///   it works, but it is invisible to registry update detection.
/// - `Ok` — a registry install whose recorded version matches what is running.
///
/// Test: `unknown_without_a_ledger`, `unknown_when_bin_is_not_in_the_ledger`,
/// `fails_on_duplicate_installs`, `fails_on_version_disagreement`,
/// `fails_when_path_source_is_gone`, `warns_for_a_live_path_install`,
/// `warns_for_a_git_install`, `ok_for_a_matching_registry_install`.
pub fn provenance_report(
    bin_name: &str,
    running_version: &str,
    running_exe: Option<&Path>,
    ledger: Option<&[CargoInstall]>,
    source_exists: &dyn Fn(&Path) -> bool,
) -> (CheckStatus, String) {
    let where_from = match running_exe {
        Some(p) => format!(" (running `{}`)", p.display()),
        None => String::new(),
    };

    let Some(ledger) = ledger else {
        return (
            CheckStatus::Unknown,
            format!(
                "cargo's install ledger (`$CARGO_HOME/.crates2.json`) is missing or unreadable \
                 — cannot verify how `{bin_name}` was installed{where_from}"
            ),
        );
    };

    let providers: Vec<&CargoInstall> = ledger
        .iter()
        .filter(|i| i.bins.iter().any(|b| b == bin_name))
        .collect();

    if providers.is_empty() {
        return (
            CheckStatus::Unknown,
            format!(
                "`{bin_name}` is not recorded in cargo's install ledger{where_from} — it was \
                 placed by the prebuilt installer, a package manager, or built in place, so its \
                 provenance and upgrade path cannot be verified from here (issue #4033)"
            ),
        );
    }

    if providers.len() > 1 {
        let sources: Vec<String> = providers
            .iter()
            .map(|i| format!("{} {} from {}", i.package, i.version, describe(&i.source)))
            .collect();
        return (
            CheckStatus::Fail,
            format!(
                "`{bin_name}` is provided by {} separate installs ({}) — whichever wrote last \
                 wins and the others are silently shadowed, so what runs is not determined by \
                 version{where_from} (issue #4033)",
                providers.len(),
                sources.join("; ")
            ),
        );
    }

    let record = providers[0];

    if record.version != running_version {
        return (
            CheckStatus::Fail,
            format!(
                "the running `{bin_name}` reports version {running_version}, but cargo's ledger \
                 records {} {} installed from {} — the binary on disk is not the one cargo \
                 tracks, so an upgrade will not replace what is actually running{where_from} \
                 (issue #4033)",
                record.package,
                record.version,
                describe(&record.source),
            ),
        );
    }

    match &record.source {
        InstallSource::Registry => (
            CheckStatus::Ok,
            format!(
                "`{bin_name}` {running_version} installed from crates.io; cargo's ledger \
                 agrees with the running version{where_from}"
            ),
        ),
        InstallSource::Path(dir) if !source_exists(dir) => (
            CheckStatus::Fail,
            format!(
                "`{bin_name}` {running_version} was installed with `cargo install --path` from \
                 `{}`, and that directory NO LONGER EXISTS — the binary has no provenance and \
                 no upgrade path; reinstall from crates.io (`cargo install {} --locked`) \
                 {where_from} (issue #4033)",
                dir.display(),
                record.package,
            ),
        ),
        InstallSource::Path(dir) => (
            CheckStatus::Warn,
            format!(
                "`{bin_name}` {running_version} was installed with `cargo install --path` from \
                 `{}` — a path install is invisible to registry update detection, so a newer \
                 published version will never be offered; reinstall with \
                 `cargo install {} --locked` once the change is published{where_from} \
                 (issue #4033, ADR-0021)",
                dir.display(),
                record.package,
            ),
        ),
        InstallSource::Git(rev) => (
            CheckStatus::Warn,
            format!(
                "`{bin_name}` {running_version} was installed from git ({rev}) — pinned to a \
                 commit, so registry update detection will never offer a newer published \
                 version{where_from} (issue #4033)"
            ),
        ),
        InstallSource::Other(raw) => (
            CheckStatus::Warn,
            format!(
                "`{bin_name}` {running_version} was installed from an unrecognised source \
                 (`{raw}`) — its upgrade path cannot be classified{where_from} (issue #4033)"
            ),
        ),
    }
}

/// Render an [`InstallSource`] for an operator-facing message.
fn describe(source: &InstallSource) -> String {
    match source {
        InstallSource::Registry => "crates.io".to_string(),
        InstallSource::Git(rev) => format!("git ({rev})"),
        InstallSource::Path(p) => format!("path `{}`", p.display()),
        InstallSource::Other(raw) => format!("`{raw}`"),
    }
}

#[cfg(test)]
#[path = "binary_provenance_tests.rs"]
mod tests;
