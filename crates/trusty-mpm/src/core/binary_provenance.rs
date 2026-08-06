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
//! # Three provenance classes, not two (#4964)
//!
//! The check was written when a binary was either cargo's (it sits at
//! `$CARGO_HOME/bin/<name>` and the ledger describes it) or not cargo's (it sits
//! somewhere else). Epic #4964 consolidates every install destination onto
//! `$CARGO_HOME/bin`, which makes a THIRD class reachable: a file the prebuilt
//! downloader placed at the exact path a stale ledger record describes. Cargo
//! writes no metadata for it, so the record still names the previous version.
//!
//! Cargo updates its ledger whenever it writes a binary, so a ledger/binary
//! version disagreement at the same path can only mean a non-cargo writer
//! replaced the file. Which of the two is NEWER decides what that means, and the
//! two directions carry opposite operator consequences:
//!
//! | State | Verdict | Why |
//! |---|---|---|
//! | running NEWER than the ledger record | `Unknown` | the ledger is stale; the running binary is the current one. Same substance as the same file sitting in `~/.local/bin`, which already reports `Unknown` — so the verdict does not change when #4964 Phase 3 flips the destination |
//! | running OLDER than the ledger record | `Fail` | an older binary overwrote a newer install: the upgrade the operator believes landed is not what executes. This is the #4033 defect class verbatim and keeps its severity |
//! | the two cannot be ordered as semver | `Fail` | conservative — an unverifiable mismatch is still a mismatch |
//!
//! Known limit, stated rather than papered over: when the downloader places the
//! SAME version cargo last recorded, the check reports `Ok`. It cannot tell the
//! two writers apart in that case and does not need to — its claim is that the
//! running version agrees with cargo's record, and that claim holds.
//!
//! Test: the `tests` module below covers every source classification and every
//! verdict branch.

use std::cmp::Ordering;
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
/// - `Unknown` — the running executable is NOT the file cargo's ledger
///   describes. Attribution by BASENAME alone is unsound: on the #4033 host
///   `which -a tm` returns `~/.cargo/bin/tm` (1.3.1) AND `~/.local/bin/tm`
///   (0.19.29), so a `tm` cargo never installed would otherwise inherit the
///   cargo record and, if the versions happened to agree, report `Ok —
///   installed from crates.io` (#4622 review, HIGH-2).
/// - `Fail` — the same binary is provided by MORE THAN ONE install (the #4033
///   trusty-channels case: two worktrees, conflicting binaries).
/// - the ledger's recorded version disagrees with the version actually running:
///   see [`version_skew_verdict`] — `Unknown` when the running binary is the
///   newer of the two, `Fail` when it is the older (#4964).
/// - `Fail` — a path install whose source directory is GONE: no provenance, no
///   upgrade path, and nothing left to rebuild from.
/// - `Warn` — a path, git, or unrecognised-source install whose source survives:
///   it works, but it is invisible to registry update detection. ADR-0021 makes
///   `Warn` the CORRECT and terminal verdict for a path install; #4964 does not
///   change that.
/// - `Ok` — a registry install whose recorded version matches what is running.
///
/// Test: `unknown_without_a_ledger`, `unknown_when_bin_is_not_in_the_ledger`,
/// `fails_on_duplicate_installs`, `fails_on_version_disagreement`,
/// `unknown_when_downloader_replaced_the_cargo_install`,
/// `fails_when_versions_cannot_be_ordered`,
/// `downloader_placed_binary_is_unknown_in_either_directory`,
/// `fails_when_path_source_is_gone`, `warns_for_a_live_path_install`,
/// `warns_for_a_git_install`, `ok_for_a_matching_registry_install`.
pub fn provenance_report(
    bin_name: &str,
    running_version: &str,
    running_exe: &Path,
    cargo_bin_dir: &Path,
    ledger: Option<&[CargoInstall]>,
    source_exists: &dyn Fn(&Path) -> bool,
) -> (CheckStatus, String) {
    let where_from = format!(" (running `{}`)", running_exe.display());

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

    // #4622 review HIGH-2: the ledger record was matched by BASENAME. Before
    // believing anything it says about the running process, confirm the running
    // process IS the file cargo placed. `which -a tm` on the #4033 host returns
    // two different binaries; without this, the shadowing one inherits the
    // other's provenance.
    let installed = cargo_bin_dir.join(bin_name);
    if !same_file(running_exe, &installed) {
        return (
            CheckStatus::Unknown,
            format!(
                "the running `{bin_name}` is `{}`, but cargo's ledger describes a DIFFERENT                  file — `{}` ({} {} from {}). The running binary was not installed by cargo,                  so its provenance and upgrade path cannot be verified from here, and                  upgrading the cargo install would not replace it. Check `which -a {bin_name}`                  for a shadowing copy (issue #4033, ADR-0021)",
                running_exe.display(),
                installed.display(),
                record.package,
                record.version,
                describe(&record.source),
            ),
        );
    }

    if record.version != running_version {
        return version_skew_verdict(bin_name, running_version, record, &installed, &where_from);
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

/// Which of the ledger record and the running binary is the newer one.
///
/// Why (#4964): the running binary and cargo's record sit at the SAME path, and
/// cargo rewrites its ledger whenever it writes — so a version disagreement can
/// only mean a non-cargo writer replaced the file. That is a defect in one
/// direction and a sanctioned install in the other, and a single `Fail` cannot
/// say which. Ordering the two versions is what separates them.
/// What: three states, named for which side is stale.
/// Test: `skew_is_ledger_stale_when_running_is_newer`,
/// `skew_is_running_stale_when_ledger_is_newer`,
/// `skew_is_unordered_for_unparseable_versions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VersionSkew {
    /// The running binary is NEWER than cargo's record: a writer that keeps no
    /// cargo metadata (the prebuilt installer, a package manager, a manual
    /// copy) replaced the file, and the ledger has not caught up.
    LedgerStale,
    /// The running binary is OLDER than cargo's record: something overwrote a
    /// newer install with an older binary.
    RunningStale,
    /// At least one of the two versions is not parseable semver, so they cannot
    /// be ordered at all.
    Unordered,
}

/// Order a ledger version against the running binary's version.
///
/// Why: kept separate from [`version_skew_verdict`] so the ordering rule is
/// unit-testable without constructing a whole ledger record, and so the
/// unparseable case is a deliberate third outcome rather than a silent `false`.
/// What: parses both with the canonical `semver` parser; any parse failure
/// yields [`VersionSkew::Unordered`]. Equal versions never reach here — the
/// caller only calls it on a disagreement — but an `Equal` comparison maps to
/// `Unordered` so no branch is left implicit.
/// Test: `skew_is_ledger_stale_when_running_is_newer`,
/// `skew_is_running_stale_when_ledger_is_newer`,
/// `skew_is_unordered_for_unparseable_versions`.
pub(crate) fn classify_version_skew(ledger_version: &str, running_version: &str) -> VersionSkew {
    let (Ok(ledger), Ok(running)) = (
        semver::Version::parse(ledger_version),
        semver::Version::parse(running_version),
    ) else {
        return VersionSkew::Unordered;
    };
    match running.cmp(&ledger) {
        Ordering::Greater => VersionSkew::LedgerStale,
        Ordering::Less => VersionSkew::RunningStale,
        Ordering::Equal => VersionSkew::Unordered,
    }
}

/// The verdict for a running binary whose version disagrees with cargo's record
/// at the same path.
///
/// Why (#4964): before consolidation this was one `Fail`, because the only way
/// to reach it was a non-cargo writer clobbering the cargo bin directory. After
/// #4964 Phase 3 the prebuilt downloader writes there BY DESIGN, and reporting
/// its clean, successful, cargo-free upgrade as `Fail` would manufacture a
/// health failure out of the fix. Widening the whole branch to `Warn` was
/// rejected: it would also downgrade the real defect (an older binary sitting on
/// top of a newer install), which is the #4033 incident itself.
/// What: `Unknown` for [`VersionSkew::LedgerStale`] — the same verdict the same
/// file gets today when it sits in `~/.local/bin`, so #4964's destination flip
/// leaves the operator's `tm doctor` output unchanged; `Fail` for
/// [`VersionSkew::RunningStale`] and [`VersionSkew::Unordered`].
/// Test: `unknown_when_downloader_replaced_the_cargo_install`,
/// `fails_on_version_disagreement`, `fails_when_versions_cannot_be_ordered`,
/// `downloader_placed_binary_is_unknown_in_either_directory`.
fn version_skew_verdict(
    bin_name: &str,
    running_version: &str,
    record: &CargoInstall,
    installed: &Path,
    where_from: &str,
) -> (CheckStatus, String) {
    let recorded = format!(
        "{} {} from {}",
        record.package,
        record.version,
        describe(&record.source)
    );
    match classify_version_skew(&record.version, running_version) {
        VersionSkew::LedgerStale => (
            CheckStatus::Unknown,
            format!(
                "the running `{bin_name}` is version {running_version}, NEWER than the {recorded} \
                 cargo's ledger records for that same file `{}`. Cargo rewrites its ledger \
                 whenever it writes, so this binary was placed by a writer that keeps no cargo \
                 metadata — the prebuilt installer (`tctl install` / `tctl upgrade`), a package \
                 manager, or a manual copy. The binary is NOT stale; what cannot be verified \
                 from here is where it came from, and `cargo install --list` will keep reporting \
                 {}{where_from} (issues #4033, #4964, ADR-0021)",
                installed.display(),
                record.version,
            ),
        ),
        VersionSkew::RunningStale => (
            CheckStatus::Fail,
            format!(
                "the running `{bin_name}` is version {running_version}, OLDER than the {recorded} \
                 cargo's ledger records for that same file `{}` — an older binary is sitting on \
                 top of a newer install, so an upgrade the ledger says landed is not what is \
                 executing{where_from} (issue #4033)",
                installed.display(),
            ),
        ),
        VersionSkew::Unordered => (
            CheckStatus::Fail,
            format!(
                "the running `{bin_name}` reports version {running_version}, but cargo's ledger \
                 records {recorded} for that same file `{}`, and the two cannot be ordered as \
                 semantic versions — the binary on disk is not the one cargo tracks and which of \
                 them is newer cannot be determined{where_from} (issue #4033)",
                installed.display(),
            ),
        ),
    }
}

/// Do two paths name the same file on disk?
///
/// Why (#4622 review, HIGH-2): the running executable and cargo's install
/// location must be compared as FILES, not as strings — `current_exe()` resolves
/// symlinks on macOS while the ledger-derived path may not, and `~/.cargo/bin`
/// is commonly a symlink farm. A string-only comparison would report a false
/// mismatch on a correctly installed host.
/// What: compares canonicalised paths when both canonicalise (i.e. both exist),
/// otherwise falls back to a literal comparison. A path that cannot be
/// canonicalised is not silently treated as equal.
/// #4033 follow-up: `pub(crate)` so `core::binary_reinstall` decides the
/// same-file question with this exact implementation rather than a second copy
/// that could drift on the symlink case.
/// Test: `unknown_when_running_binary_is_not_the_cargo_install`,
/// `ok_for_a_matching_registry_install`.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Render an [`InstallSource`] for an operator-facing message.
///
/// `pub(crate)` so `core::binary_reinstall`'s refusal messages describe the
/// same ledger with the same words this module's doctor verdict does.
pub(crate) fn describe(source: &InstallSource) -> String {
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
