//! Assembling the deliverable that goes back, and saying where it landed.
//!
//! Why: #5499 is the client's last responsibility. Everything before it produces
//! artifacts scattered across the working directory — per-repository reports
//! under `out/`, extract databases under `extract/`, the verified tool triple
//! under `state/` — and none of that is something a recipient can attach to an
//! email. This module reduces them to one file and prints its path.
//!
//! What: [`assemble`] collects what a completed sweep produced, generates the
//! files that explain it ([`generated`]), and writes a single zip.
//! [`ReturnPackage`] is what a front end renders — the path, every member, and
//! every repository left out.
//!
//! ## Unencrypted, deliberately
//!
//! The zip carries no encryption and no password. That is the same posture as
//! the engagement config that arrives (#5473): the recipient can open the file
//! and read exactly what they are about to send before they send it. Encrypting
//! it would defend against nobody — they hold the plaintext either way — while
//! removing the one property the handoff's transparency premise rests on. The
//! return channel is manual (#5642), so there is no service dependency to design
//! around and no size ceiling to design against (#5480, lifted by the owner).
//!
//! ## What it will not put in the file
//!
//! Three refusals, all before a single byte reaches the destination:
//!
//! - **A link to content from elsewhere, anywhere under `out/` or `extract/`.**
//!   Packaging one would read a file outside the working directory into an
//!   archive that leaves the recipient's network. Symlinks and hardlinks both,
//!   and they need different checks: a symlink says what it is in its file type,
//!   while a hardlink is an ordinary directory entry on an inode another entry
//!   also names, so only its link count gives it away. See
//!   [`AuditError::UnsafePackageEntry`] and [`refuse_if_hardlinked`].
//! - **The engagement credential, in any member.** [`crate::config::SecretKey`]
//!   has no `Serialize`, so this crate cannot write the key into a file it
//!   generates — but the members are files OTHER programs wrote, and no type
//!   governs those. Every member's bytes are scanned as they are copied — see
//!   "Credential scanning covers every byte, database included" below. See
//!   [`AuditError::CredentialInPackage`].
//! - **A report with no collection database.** The owner's ruling on the
//!   deliverable (2026-08-18) is the database and the report together, so
//!   [`collect_extract`] refuses the whole assembly when an audited
//!   repository's `extract/<stem>.db` is missing or the `extract/` directory
//!   itself does not exist, rather than shipping the report alone with no
//!   record that its database never arrived (#5862). See
//!   [`AuditError::MissingExtractDatabase`].
//!
//! All three are refusals rather than omissions, and the package is built in a
//! temporary file that is removed on any of them, so a refused assembly leaves
//! no partial zip a recipient could send by mistake.
//!
//! ## Credential scanning covers every byte, database included
//!
//! [`credential_scan::copy_member`] reads each member through a plain
//! byte-oriented sliding window — it has no notion of file format, so it scans
//! a SQLite database exactly as it scans a `.md` report: as an opaque byte
//! stream searched for every configured secret's exact bytes. Nothing about
//! `extract/<stem>.db` exempts it from that scan; the widened test
//! `super::package_tests::an_ordinary_member_still_packages_under_the_widened_scan`
//! and the straddling-window test
//! `crate::package::credential_scan::credential_scan_tests::a_credential_split_across_two_reads_is_caught`
//! both exercise the same scan every member goes through, database or not.
//!
//! What it does NOT do: parse the database's schema or rows, so it cannot tell
//! a secret sitting in a legitimate free-text column (a commit message, a
//! pasted token in a work-item title) from one that leaked — every such value
//! that happens to match a configured secret's bytes trips the same refusal a
//! credential in a log file would. That is the same content boundary the
//! generated README states: the database carries no file content, diffs,
//! patches, hunks, or blobs, but it does carry whatever free text a human
//! pasted into a commit message or work-item title.
//!
//! ## The boundary with signing (#5481)
//!
//! This module signs nothing and hashes nothing. #5481 owns the ed25519 manifest
//! — filename plus SHA-256 per delivered file, signed with the per-engagement
//! key — and writes it into `out/`. Everything under `out/` is packaged
//! verbatim, so that manifest and its signature ride along with no change here.
//! Until #5481 lands, the generated README says the package is unsigned rather
//! than leaving the recipient to infer it.
//!
//! Test: `super::package_tests`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::config::EngagementConfig;
use crate::error::AuditError;
use crate::run::github_issues::{self, GithubAccess};
use crate::run::{RepoRun, RunReport};
use crate::workdir::{Area, WorkDir};

// #5862: adding `AuditError::MissingExtractDatabase` pushed this file one line
// past the 500-SLOC production cap. The credential scan is the most
// self-contained unit here — it depends on nothing this file does besides
// `Archive` and `start`, both of which a child module sees without any
// visibility change — so it moved rather than anything else splitting.
mod credential_scan;

// #6080, #6245: the members this crate generates rather than collects — the
// README, `package.toml`, `reports/index.md`, and `failures/index.md`. Split
// out when the index pushed this file past the 500-SLOC production cap. This
// file decides what goes into the archive; that one writes the files describing
// the result.
mod generated;

use generated::{
    Generated, exclusions, render_failures, render_index, render_metadata, render_readme,
};

/// Filename of the return package, when the recipient does not choose one.
pub const PACKAGE_FILE_NAME: &str = "audit-return-package.zip";

/// The generated metadata member: engagement labels, tool versions, coverage.
pub const METADATA_ENTRY: &str = "package.toml";

/// The generated member that tells the recipient what they are about to send.
pub const README_ENTRY: &str = "README.md";

/// Directory inside the zip holding one subdirectory per audited repository.
pub const REPORTS_PREFIX: &str = "reports";

/// The generated member that explains what the reports are and links to them.
///
/// Why: inside `reports/` rather than at the package root, because that is where
/// the sweep's own index sits relative to the report directories — `out/` maps
/// to `reports/`, so `<stem>/report.md` and `../extract/<stem>.db` resolve
/// identically in both layouts. At the root it would need a different frame from
/// every other index this crate writes (#6080).
/// Test: `super::package_tests::the_package_index_links_resolve_inside_the_zip`.
pub const INDEX_ENTRY: &str = "reports/index.md";

/// Directory inside the zip holding the tga extract databases (#5479).
pub const EXTRACT_PREFIX: &str = "extract";

/// Directory inside the zip holding the record of every repository that failed.
///
/// Why (#6245): a failed target used to leave NOTHING in the package — no log,
/// no record, just absence from `reports/`. A recipient could not tell "this
/// repository failed" from "this repository was never attempted", and
/// diagnosing either meant re-running the sweep on the machine that had it. Two
/// of 59 repositories exited 1 on the 2026-08-25 run and shipped no trace at
/// all.
/// What: one `failures/<stem>.log` per failed repository — the child's own
/// combined output, copied through the same credential scan every other
/// collected member goes through — plus a generated [`FAILURES_ENTRY`] naming
/// each one, why it failed, and where its log is.
/// Test: `super::package_tests::a_failed_repository_ships_its_log_and_a_record`.
pub const FAILURES_PREFIX: &str = "failures";

/// The generated member that names every repository that failed, and why.
pub const FAILURES_ENTRY: &str = "failures/index.md";

/// Where the package lands when nothing overrides it.
///
/// The work-dir ROOT, not an area under it: `assemble` walks `out/` and
/// `extract/`, so a package written into either would be swept into the next
/// one. The root is still inside the tree `rm -rf <work-dir>` removes, so the
/// default keeps `workdir`'s containment promise intact.
pub fn default_destination(work: &WorkDir) -> PathBuf {
    work.root().join(PACKAGE_FILE_NAME)
}

/// One file in the finished package.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PackagedFile {
    /// Path inside the zip.
    pub entry: String,
    /// Where it came from, or `None` for a member this module generated.
    pub source: Option<PathBuf>,
    /// Uncompressed size.
    pub bytes: u64,
}

/// The finished package: what to send, and what is not in it.
///
/// Why: closure condition 3 of #5499 is that the client SURFACES the finished
/// zip. A path alone does not do that — a recipient about to email a file to an
/// auditor needs to know what is in it and what is missing from it, and
/// `excluded` is what stops a partial sweep being sent as a whole one.
/// What: the destination, every member, the uncompressed and on-disk sizes, and
/// one line per repository the package does not cover.
/// Test: `super::package_tests::a_partial_sweep_names_what_the_package_omits`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReturnPackage {
    /// The finished zip. This is the file to send back.
    pub path: PathBuf,
    /// Every member, in the order they were written.
    pub files: Vec<PackagedFile>,
    /// Total uncompressed size of the members.
    pub total_bytes: u64,
    /// Size of the zip on disk.
    pub packaged_bytes: u64,
    /// One line per target the package does not cover, and why.
    ///
    /// The sweep's own failures, plus whatever the caller passed as
    /// `unattempted` — a repository that never cloned reaches the sweep's
    /// records nowhere, so it can only arrive that way (#5824).
    pub excluded: Vec<String>,
}

/// Build the return package from what a completed sweep produced.
///
/// # Preconditions
/// `report` describes a sweep that finished, and at least one repository in it
/// succeeded. Both are checked; a sweep in which nothing was audited is
/// [`AuditError::NothingToPackage`], not an empty package.
///
/// # Postconditions
/// On `Ok`, `destination` is a complete zip carrying [`README_ENTRY`],
/// [`METADATA_ENTRY`], every file under each successful repository's output
/// directory, and every extract database belonging to one. Its
/// [`ReturnPackage::excluded`] names each repository it does not cover. On
/// `Err`, `destination` is untouched — the archive is built beside it and
/// renamed into place only after the last member is written.
///
/// What: collects the members, refuses a symlink among them, writes the archive
/// to a sibling temporary file while scanning every byte for the engagement
/// credential, then renames.
///
/// `github_token` is scanned for verbatim — this function trusts the caller
/// to have already confirmed it is the SAME credential the sweep collected
/// under. [`from_checkpoint`] is that confirmation (#5980 CRITICAL — the
/// account-switch follow-up); calling `assemble` directly with a `github_token`
/// from a different `gh` account than the sweep used will NOT catch an older
/// token a file carries — there is nothing here to compare against.
/// Test: `super::package_tests`.
///
/// # Errors
///
/// [`AuditError::NothingToPackage`] when no repository was audited,
/// [`AuditError::UnsafePackageEntry`] for a symlink or hardlink under `out/` or
/// `extract/`,
/// [`AuditError::CredentialInPackage`] when a member carries the engagement key,
/// [`AuditError::MissingExtractDatabase`] when an audited repository has no
/// collection database under `extract/`,
/// and [`AuditError::Package`] for any read, write, or rename failure.
/// Assemble from the sweep's own record, refusing one that did not finish.
///
/// Why: #5824 gave packaging a second caller. The completion check used to live
/// in `Session::package`, so the chain would either have had to duplicate it or
/// to skip it by passing the report it already held in memory — and skipping it
/// is how a sweep that died three repositories into six gets sent as a whole
/// engagement. One function, both callers, one precondition.
///
/// The completion signal is [`crate::run::RunProgress::complete`], not the
/// record's mere presence: since #5494 the record is written after every
/// repository, so an unfinished sweep leaves one behind.
/// What: reads `state/run-progress.toml`, requires it to be complete, and hands
/// its report to [`assemble`].
/// Test: `crate::session::session_tests::packaging_before_any_sweep_is_refused`,
/// `crate::session::session_tests::packaging_an_unfinished_sweep_is_refused`,
/// `crate::chain::chain_tests::the_chain_installs_collects_and_packages`.
///
/// `github_access` is a freshly-resolved credential (#5980 CRITICAL 4) —
/// packaging can run as a separate process from the sweep, so it never
/// inherits the sweep's own resolution. Before this function hands
/// [`GithubAccess::raw_token`] to [`assemble`] as the outbound scan's extra
/// needle, it first confirms that token is the SAME one the sweep recorded,
/// via [`github_issues::verify_unchanged`] — see that function's docs and
/// [`GithubAccess::fingerprint`]'s for the account-switch gap this closes.
/// A stated-but-unrefused gap from that check (the back-compat case) is
/// folded into the returned package's `excluded` lines alongside
/// `unattempted`.
///
/// # Errors
///
/// [`AuditError::NothingToPackage`] when no sweep has finished here,
/// [`AuditError::GithubCredentialChanged`] when the active GitHub credential
/// provably differs from the one the sweep collected under, and whatever
/// [`assemble`] fails with.
pub fn from_checkpoint(
    work: &WorkDir,
    config: &EngagementConfig,
    unattempted: &[String],
    destination: &Path,
    github_access: &GithubAccess,
) -> Result<ReturnPackage, AuditError> {
    let progress =
        crate::run::read_progress(work)?.ok_or_else(|| AuditError::NothingToPackage {
            reason: format!(
                "no sweep has finished in {} — run `trusty-audit run` first",
                work.root().display()
            ),
        })?;
    if !progress.complete {
        return Err(AuditError::NothingToPackage {
            reason: format!(
                "the last sweep in {} did not finish — {} recorded so far; \
                 run `trusty-audit run` to resume it",
                work.root().display(),
                match progress.repos.len() {
                    1 => "1 repository".to_owned(),
                    n => format!("{n} repositories"),
                }
            ),
        });
    }
    let credential_gap =
        github_issues::verify_unchanged(progress.github_credential.as_ref(), github_access)?;
    let mut excluded_extra = unattempted.to_vec();
    excluded_extra.extend(credential_gap);
    assemble(
        work,
        config,
        &progress.report(),
        &excluded_extra,
        destination,
        github_access.raw_token(),
    )
}

pub fn assemble(
    work: &WorkDir,
    config: &EngagementConfig,
    report: &RunReport,
    unattempted: &[String],
    destination: &Path,
    github_token: Option<&str>,
) -> Result<ReturnPackage, AuditError> {
    let audited: Vec<&RepoRun> = report
        .repos
        .iter()
        .filter(|r| r.result.succeeded())
        .collect();
    if audited.is_empty() {
        return Err(AuditError::NothingToPackage {
            reason: format!(
                "no repository was audited ({} attempted), so there is no report to return",
                report.repos.len()
            ),
        });
    }

    let mut collected = Vec::new();
    for run in &audited {
        let stem = output_stem(run);
        collect_dir(
            &run.output,
            &format!("{REPORTS_PREFIX}/{stem}"),
            &mut collected,
        )?;
        collect_extract(work, &stem, &run.repo.name, &mut collected)?;
    }
    // #6245: the log of every repository that failed, as a collected member —
    // so it goes through the same symlink, hardlink and credential guards the
    // reports do. A log that is not on disk is simply absent; the generated
    // record below still names the repository and why it failed.
    for run in report.failures() {
        collect_log(run, &mut collected)?;
    }
    collected.sort_by(|a, b| a.0.cmp(&b.0));

    // #5824: the sweep's own failures, then the targets that never reached it.
    let mut excluded = exclusions(report);
    excluded.extend(unattempted.iter().cloned());
    let generated = Generated {
        metadata: render_metadata(work, config, report, &audited, unattempted)?,
        readme: render_readme(config, &audited, &excluded),
        // #6080: built from `collected`, so the index links the members that are
        // actually going into this archive rather than the files the sweep left
        // on disk. Before `fill_archive`, because it is one of the members.
        index: render_index(work, report, &collected),
        // #6245: `None` on a clean sweep — a `failures/` directory holding an
        // index that says "none" is worse than no directory.
        failures: render_failures(report, &collected),
    };

    write_archive(
        destination,
        config,
        generated,
        collected,
        excluded,
        github_token,
    )
}

/// The directory name `run` wrote its output under, which is also the name its
/// extract database carries (`run::stem`).
fn output_stem(run: &RepoRun) -> String {
    run.output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        // A run report whose output path has no final component cannot have been
        // produced by `run::run_one`; falling back to the repository name keeps
        // the member addressable rather than panicking on a hand-edited record.
        .unwrap_or_else(|| run.repo.name.clone())
}

/// Refuse a file that another directory entry also names.
///
/// Why: the symlink refusal does not cover this, and the difference is not a
/// gap in the check but a property of the filesystem. A hardlink is not a link
/// TO anything — it is a second directory entry on the same inode, equal in
/// every way to the first. `is_symlink()` is false, `is_file()` is true, and
/// `canonicalize` returns the path it was given, because there is no other path
/// to resolve to. So containment checks on the resolved path cannot see it
/// either: the entry genuinely IS inside the working directory, and so is its
/// content, which is also somewhere else. The link COUNT is the only thing that
/// distinguishes it, which is why this check is the mechanism rather than a
/// path-based one.
///
/// What: refuses any regular file with more than one link.
///
/// The false-refusal this buys, stated plainly: a legitimate member that
/// happens to carry `nlink > 1` is refused even though its content is
/// perfectly in scope. Nothing in this crate's own pipeline produces one —
/// `tga audit` creates its reports and databases fresh, and a freshly created
/// file has exactly one link, which
/// `ordinary_audit_output_has_one_link_and_is_not_refused` asserts. Reaching
/// `nlink > 1` under `out/` or `extract/` takes a deliberate `ln`, `cp -l`, or
/// a hardlink-based backup tool pointed INTO the working directory. Refusing
/// that costs a recipient an error message they can fix by copying the file
/// instead of linking it; accepting it puts arbitrary content into an archive
/// that leaves their network. For a package built to be sent to someone else,
/// the refusal is the right side to err on.
///
/// On a non-unix target the link count is not available and this is a no-op —
/// acceptable because the epic scopes this client to macOS arm64 (#5473), and
/// widening the platform means revisiting the check, not inheriting a silent
/// hole.
/// Test: `super::package_tests::a_hardlinked_member_under_out_is_refused`,
/// `super::package_tests::a_hardlinked_member_under_extract_is_refused`,
/// `super::package_tests::ordinary_audit_output_has_one_link_and_is_not_refused`.
fn refuse_if_hardlinked(path: &Path) -> Result<(), AuditError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::symlink_metadata(path).map_err(|source| AuditError::Package {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.nlink() > 1 {
            return Err(AuditError::UnsafePackageEntry {
                path: path.to_path_buf(),
                kind: "hardlink",
            });
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Every file under `dir`, as `(entry, source)` pairs.
fn collect_dir(
    dir: &Path,
    prefix: &str,
    into: &mut Vec<(String, PathBuf)>,
) -> Result<(), AuditError> {
    // `follow_root_links` defaults to TRUE, which would follow `dir` itself
    // being a symlink even though every nested one is refused below. This crate
    // creates that directory itself (`run::run_one`), so it is not reachable
    // through its own sweep — set explicitly rather than resting on that, since
    // the caller that makes it reachable would be a later change, not this one.
    for entry in walkdir::WalkDir::new(dir)
        .follow_root_links(false)
        .sort_by_file_name()
    {
        let entry = entry.map_err(|e| AuditError::Package {
            path: dir.to_path_buf(),
            source: std::io::Error::other(e),
        })?;
        // `WalkDir` does not follow links by default, so a symlink arrives as an
        // entry rather than as the file it points at — which is what makes the
        // refusal possible at all.
        if entry.file_type().is_symlink() {
            return Err(AuditError::UnsafePackageEntry {
                path: entry.path().to_path_buf(),
                kind: "symlink",
            });
        }
        if !entry.file_type().is_file() {
            continue;
        }
        refuse_if_hardlinked(entry.path())?;
        let relative = entry
            .path()
            .strip_prefix(dir)
            .map_err(|e| AuditError::Package {
                path: entry.path().to_path_buf(),
                source: std::io::Error::other(e),
            })?;
        into.push((
            format!("{prefix}/{}", relative.to_string_lossy().replace('\\', "/")),
            entry.path().to_path_buf(),
        ));
    }
    Ok(())
}

/// One failed repository's child log, as a member under [`FAILURES_PREFIX`].
///
/// Why (#6245): the log is the only thing that says what the child was doing
/// when it stopped, and it was staying on the machine that ran the sweep — so a
/// recipient holding the package could not diagnose a failed repository without
/// re-running it. It ships through the collected-member path rather than as
/// generated text because it is bytes ANOTHER program wrote, and every such byte
/// goes through [`credential_scan::copy_member`]'s scan.
/// What: the same symlink and hardlink refusals [`collect_dir`] applies, then
/// `failures/<stem>.log`. A log that is not on disk — a child that never got far
/// enough to have one — is skipped in silence; [`render_failures`] names the
/// repository either way.
///
/// # Errors
///
/// [`AuditError::UnsafePackageEntry`] when the log is a symlink or is
/// hardlinked, for the same reason a report under `out/` would be.
/// Test: `super::package_tests::a_failed_repository_ships_its_log_and_a_record`.
fn collect_log(run: &RepoRun, into: &mut Vec<(String, PathBuf)>) -> Result<(), AuditError> {
    let Ok(metadata) = std::fs::symlink_metadata(&run.log) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(AuditError::UnsafePackageEntry {
            path: run.log.clone(),
            kind: "symlink",
        });
    }
    if !metadata.is_file() {
        return Ok(());
    }
    refuse_if_hardlinked(&run.log)?;
    into.push((
        format!("{FAILURES_PREFIX}/{}.log", output_stem(run)),
        run.log.clone(),
    ));
    Ok(())
}

/// One repository's extract database, and any sidecar SQLite left beside it.
///
/// Matching by PREFIX rather than by exact name is what picks up the `-wal` and
/// `-shm` files WAL mode leaves: a database shipped without them can be missing
/// the last committed transactions. The prefix match only WIDENS what a present
/// database brings along; it never substitutes for the primary `<stem>.db`
/// itself, which is checked by exact name below.
///
/// # Errors
///
/// [`AuditError::MissingExtractDatabase`] when `extract/<stem>.db` is not
/// present — including when `extract/` does not exist at all, which is the
/// same absence one level up (#5862). [`AuditError::UnsafePackageEntry`] for a
/// symlink or hardlink matching the prefix, and [`AuditError::Package`] for any
/// other read failure.
/// Test: `super::package_tests::an_audited_repo_with_no_extract_database_is_refused`,
/// `super::package_tests::an_audited_repo_with_no_extract_directory_at_all_is_refused`.
fn collect_extract(
    work: &WorkDir,
    stem: &str,
    repo_name: &str,
    into: &mut Vec<(String, PathBuf)>,
) -> Result<(), AuditError> {
    let dir = work.path(Area::Extract);
    let wanted = format!("{stem}.db");
    let missing = || AuditError::MissingExtractDatabase {
        repo: repo_name.to_owned(),
        expected: dir.join(&wanted),
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(missing()),
        Err(source) => return Err(AuditError::Package { path: dir, source }),
    };
    let mut found_primary = false;
    for entry in entries {
        let entry = entry.map_err(|source| AuditError::Package {
            path: dir.clone(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&wanted) {
            continue;
        }
        let kind = entry.file_type().map_err(|source| AuditError::Package {
            path: entry.path(),
            source,
        })?;
        if kind.is_symlink() {
            return Err(AuditError::UnsafePackageEntry {
                path: entry.path(),
                kind: "symlink",
            });
        }
        if kind.is_file() {
            refuse_if_hardlinked(&entry.path())?;
            found_primary |= name == wanted;
            into.push((format!("{EXTRACT_PREFIX}/{name}"), entry.path()));
        }
    }
    if !found_primary {
        return Err(missing());
    }
    Ok(())
}

/// Write the archive beside `destination`, then rename it into place.
///
/// The temporary file is what makes the two refusals meaningful: a credential
/// found in the last member removes a `.part` file rather than leaving a
/// finished-looking zip the recipient might send.
fn write_archive(
    destination: &Path,
    config: &EngagementConfig,
    generated: Generated,
    collected: Vec<(String, PathBuf)>,
    excluded: Vec<String>,
    github_token: Option<&str>,
) -> Result<ReturnPackage, AuditError> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AuditError::Package {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temporary = destination.with_extension("zip.part");
    let result = fill_archive(&temporary, config, generated, collected, github_token);
    let mut files = match result {
        Ok(files) => files,
        Err(e) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(e);
        }
    };
    // A failed rename leaves a COMPLETE archive at the `.part` path. Nothing
    // lands at the destination either way, so this is tidiness rather than a
    // leak — but a finished-looking zip sitting beside the destination is
    // exactly the thing a recipient might find and send, so it goes the same
    // way as the fill failure above.
    if let Err(source) = std::fs::rename(&temporary, destination) {
        let _ = std::fs::remove_file(&temporary);
        return Err(AuditError::Package {
            path: destination.to_path_buf(),
            source,
        });
    }

    let total_bytes = files.iter().map(|f| f.bytes).sum();
    let packaged_bytes = std::fs::metadata(destination).map(|m| m.len()).unwrap_or(0);
    files.shrink_to_fit();
    Ok(ReturnPackage {
        path: destination.to_path_buf(),
        files,
        total_bytes,
        packaged_bytes,
        excluded,
    })
}

fn fill_archive(
    temporary: &Path,
    config: &EngagementConfig,
    generated: Generated,
    collected: Vec<(String, PathBuf)>,
    github_token: Option<&str>,
) -> Result<Vec<PackagedFile>, AuditError> {
    let file = std::fs::File::create(temporary).map_err(|source| AuditError::Package {
        path: temporary.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let mut files = Vec::with_capacity(collected.len() + 3);

    let members = [
        (README_ENTRY, Some(generated.readme)),
        (METADATA_ENTRY, Some(generated.metadata)),
        (INDEX_ENTRY, Some(generated.index)),
        // #6245: absent on a clean sweep — see `Generated::failures`.
        (FAILURES_ENTRY, generated.failures),
    ];
    for (entry, text) in members.into_iter().filter_map(|(e, t)| t.map(|t| (e, t))) {
        // #6245: this member quotes the reason strings recorded for each failed
        // repository, and a reason can carry text a child produced. Generated or
        // not, it is scanned before it is written — the same bar every collected
        // member meets.
        credential_scan::refuse_if_credential(&text, entry, config, github_token)?;
        start(&mut zip, entry, text.len() as u64, temporary)?;
        zip.write_all(text.as_bytes())
            .map_err(|source| AuditError::Package {
                path: temporary.to_path_buf(),
                source,
            })?;
        files.push(PackagedFile {
            entry: entry.to_owned(),
            source: None,
            bytes: text.len() as u64,
        });
    }

    for (entry, source) in collected {
        let bytes = credential_scan::copy_member(
            &mut zip,
            &entry,
            &source,
            config,
            temporary,
            github_token,
        )?;
        files.push(PackagedFile {
            entry,
            source: Some(source),
            bytes,
        });
    }

    zip.finish().map_err(|e| AuditError::Package {
        path: temporary.to_path_buf(),
        source: std::io::Error::other(e),
    })?;
    Ok(files)
}

type Archive = zip::ZipWriter<std::io::BufWriter<std::fs::File>>;

fn start(zip: &mut Archive, entry: &str, bytes: u64, temporary: &Path) -> Result<(), AuditError> {
    let mut options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    // Zip64 only where it is needed: an extract database can exceed the 4 GiB
    // classic-zip ceiling, and every other member is better off in a form the
    // oldest unzip on the recipient's machine can open.
    if bytes >= u64::from(u32::MAX) {
        options = options.large_file(true);
    }
    zip.start_file(entry.to_owned(), options)
        .map_err(|e| AuditError::Package {
            path: temporary.to_path_buf(),
            source: std::io::Error::other(e),
        })
}

#[cfg(test)]
mod package_tests {
    use std::io::Read as _;

    use super::*;
    use crate::run::{RepoResult, RepoRun, SelectedRepo};

    const CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."
client = "Acme"

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    fn config() -> EngagementConfig {
        EngagementConfig::from_toml(CONFIG, Path::new("engagement.toml")).expect("parses")
    }

    fn work_in(dir: &Path) -> WorkDir {
        let work = WorkDir::new(dir.join("work"));
        work.create().expect("create");
        work
    }

    /// A repository that ran, with the report and database a real sweep leaves.
    fn audited(work: &WorkDir, stem: &str, name: &str) -> RepoRun {
        let output = work.path(Area::Output).join(stem);
        std::fs::create_dir_all(&output).expect("mkdir output");
        std::fs::write(
            output.join("manifest.toml"),
            "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"acme\"\npath = \"/r\"\n",
        )
        .expect("write manifest");
        std::fs::write(output.join("report.md"), "# Report\n").expect("write report");
        std::fs::write(
            work.path(Area::Extract).join(format!("{stem}.db")),
            b"SQLite format 3\0extract",
        )
        .expect("write db");
        RepoRun {
            repo: SelectedRepo {
                name: name.to_owned(),
                path: PathBuf::from(format!("repos/{name}")),
                github_slug: None,
                github_absent: None,
            },
            output,
            log: work.path(Area::Logs).join(format!("{stem}.log")),
            gaps: Vec::new(),
            resumed: false,
            duration_ms: None,
            result: RepoResult::Succeeded,
        }
    }

    fn failed(work: &WorkDir, stem: &str, name: &str) -> RepoRun {
        RepoRun {
            result: RepoResult::Failed {
                reason: "`tga audit` exited with code 3".to_owned(),
            },
            ..RepoRun {
                repo: SelectedRepo {
                    name: name.to_owned(),
                    path: PathBuf::from(format!("repos/{name}")),
                    github_slug: None,
                    github_absent: None,
                },
                output: work.path(Area::Output).join(stem),
                log: work.path(Area::Logs).join(format!("{stem}.log")),
                gaps: Vec::new(),
                resumed: false,
                duration_ms: None,
                result: RepoResult::Succeeded,
            }
        }
    }

    fn install_record(work: &WorkDir) {
        std::fs::write(
            crate::tools::record_path(work),
            "[[tools]]\ncrate_name = \"tga\"\nversion = \"2.9.4\"\nbinary = \"/w/tools/tga\"\n",
        )
        .expect("write record");
    }

    /// Every member name in the finished zip.
    fn entries(path: &Path) -> Vec<String> {
        let file = std::fs::File::open(path).expect("open package");
        let mut archive = zip::ZipArchive::new(file).expect("a readable zip");
        (0..archive.len())
            .map(|i| archive.by_index(i).expect("entry").name().to_owned())
            .collect()
    }

    fn read_entry(path: &Path, name: &str) -> String {
        let file = std::fs::File::open(path).expect("open package");
        let mut archive = zip::ZipArchive::new(file).expect("a readable zip");
        let mut entry = archive.by_name(name).expect("entry present");
        let mut text = String::new();
        entry.read_to_string(&mut text).expect("read entry");
        text
    }

    /// The whole path: a completed sweep becomes one openable file carrying the
    /// reports, the extract database, and the two generated members.
    #[test]
    fn a_completed_sweep_becomes_one_openable_zip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api")]);
        let destination = default_destination(&work);

        let package =
            assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");

        assert_eq!(package.path, destination);
        assert!(destination.is_file(), "the zip was not written");
        let names = entries(&destination);
        for expected in [
            "README.md",
            "package.toml",
            "reports/00-acme-api/manifest.toml",
            "reports/00-acme-api/report.md",
            "extract/00-acme-api.db",
        ] {
            assert!(names.contains(&expected.to_owned()), "{names:?}");
        }
        assert!(package.total_bytes > 0);
        assert!(package.excluded.is_empty());
    }

    /// 🔴 #6080: the RECIPIENT is the primary audience for an
    /// explain-the-contents page, and the zip is what they get. It carries one,
    /// with the versions responsible and one section per repository.
    ///
    /// Against `cc48e943d` this fails on the first assertion: the index existed
    /// only in the sweep's working directory, which never leaves the machine
    /// that ran the audit.
    #[test]
    fn the_package_carries_an_index_of_its_reports() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let mut run = audited(&work, "00-acme-api", "acme-api");
        run.duration_ms = Some(752_000);
        let report = RunReport::of(vec![run]);
        let destination = default_destination(&work);

        let package =
            assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");

        assert!(
            entries(&destination).contains(&INDEX_ENTRY.to_owned()),
            "{:?}",
            entries(&destination)
        );
        assert!(
            package.files.iter().any(|f| f.entry == INDEX_ENTRY),
            "the index must be reported as a member: {:?}",
            package.files
        );
        let index = read_entry(&destination, INDEX_ENTRY);
        assert!(index.contains("Reports: 1 of 1 repository"), "{index}");
        assert!(
            index.contains("| `tga` | 2.9.4 | recorded at install |"),
            "{index}"
        );
        assert!(index.contains("### acme-api"), "{index}");
        // The sweep's measurement travels with the package.
        assert!(index.contains("| acme-api | 12m 32s |"), "{index}");
        // And no log is offered, because the archive carries none.
        assert!(!index.contains("- log:"), "{index}");
    }

    /// 🔴 Every link the packaged index writes must name a member that is
    /// actually in the archive. `reports/index.md` is one directory deep, so a
    /// report resolves as `<stem>/…` and the extract database as `../extract/…`
    /// — and a frame error in either direction is a link the recipient cannot
    /// follow.
    #[test]
    fn the_package_index_links_resolve_inside_the_zip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api")]);
        let destination = default_destination(&work);
        assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");

        let index = read_entry(&destination, INDEX_ENTRY);
        let members = entries(&destination);
        let targets = markdown_link_targets(&index);
        assert!(!targets.is_empty(), "the index linked nothing: {index}");
        for target in &targets {
            let resolved = resolve_against(REPORTS_PREFIX, target);
            assert!(
                members.contains(&resolved),
                "the index links `{target}`, which resolves to `{resolved}` — \
                 not a member of the archive: {members:?}"
            );
        }
        // The two frames that matter, named rather than only inferred.
        assert!(
            targets.contains(&"00-acme-api/report.md".to_owned()),
            "{targets:?}"
        );
        assert!(
            targets.contains(&"../extract/00-acme-api.db".to_owned()),
            "{targets:?}"
        );
    }

    /// Every `](…)` destination in `text`, ignoring the angle-bracket form,
    /// which nothing in a packaged index produces (the stems are sanitized).
    fn markdown_link_targets(text: &str) -> Vec<String> {
        text.lines()
            .filter(|line| line.starts_with("- ["))
            .filter_map(|line| {
                let start = line.find("](")? + 2;
                let end = line[start..].find(')')? + start;
                Some(line[start..end].to_owned())
            })
            .collect()
    }

    /// A zip entry path, resolving `../` against the directory `base`.
    fn resolve_against(base: &str, target: &str) -> String {
        let mut parts: Vec<&str> = base.split('/').collect();
        for segment in target.split('/') {
            match segment {
                ".." => {
                    parts.pop();
                }
                "." => {}
                other => parts.push(other),
            }
        }
        parts.join("/")
    }

    /// A repository the package does not cover is still NAMED in its index, with
    /// the reason — the recipient should not have to diff the report directories
    /// against the README to learn one is missing.
    #[test]
    fn the_package_index_names_the_repository_it_does_not_cover() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![
            audited(&work, "00-acme-api", "acme-api"),
            failed(&work, "01-acme-web", "acme-web"),
        ]);
        let destination = default_destination(&work);
        assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");

        let index = read_entry(&destination, INDEX_ENTRY);
        assert!(index.contains("Reports: 1 of 2 repositories"), "{index}");
        assert!(index.contains("### acme-web"), "{index}");
        assert!(
            index.contains("No report — `tga audit` exited with code 3"),
            "{index}"
        );
        // It must not link a report directory the archive does not carry.
        assert!(
            !index.contains("01-acme-web/"),
            "the index linked an excluded repository's files: {index}"
        );
    }

    /// The transparency premise: a recipient can open it with no password, and
    /// the README says what they are sending.
    #[test]
    fn the_package_is_unencrypted_and_says_what_it_holds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api")]);
        let destination = default_destination(&work);
        assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");

        // `by_index` succeeding with no password IS the unencrypted property:
        // the zip crate returns `UnsupportedArchive` for an encrypted entry.
        let readme = read_entry(&destination, README_ENTRY);
        assert!(readme.contains("no encryption"), "{readme}");
        // #5479's exact claim, which is not "no code".
        assert!(
            readme.contains("no file content, diffs, patches, hunks, or blobs"),
            "{readme}"
        );
        assert!(
            readme.contains("#5481"),
            "the unsigned state must be stated"
        );

        let metadata = read_entry(&destination, METADATA_ENTRY);
        assert!(metadata.contains("client = \"Acme\""), "{metadata}");
        assert!(metadata.contains("version = \"2.9.4\""), "{metadata}");
    }

    /// The crate's central invariant, at the one place it can still be broken:
    /// the members are files other programs wrote, so no type governs them.
    #[test]
    fn a_member_carrying_the_credential_is_refused_and_leaves_no_zip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let run = audited(&work, "00-acme-api", "acme-api");
        std::fs::write(
            run.output.join("leaked.log"),
            "authorization: Bearer sk-or-v1-not-a-real-key\n",
        )
        .expect("write");
        let report = RunReport::of(vec![run]);
        let destination = default_destination(&work);

        let err = assemble(&work, &config(), &report, &[], &destination, None)
            .expect_err("a package carrying the key must not be produced");
        assert!(
            matches!(err, AuditError::CredentialInPackage { .. }),
            "{err:?}"
        );
        assert!(
            !destination.exists(),
            "a refused package must leave no file"
        );
        assert!(
            !destination.with_extension("zip.part").exists(),
            "the temporary must be removed too"
        );
    }

    /// The board credentials #5857 routes through this crate, on the same
    /// engagement that already carries the OpenRouter key.
    const BOARD_CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."
client = "Acme"

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"

[boards.jira]
url = "https://acme.atlassian.net"
email = "auditor@acme.example"
token = "jira-token-do-not-package-me"

[boards.linear]
api_key = "lin_api_do-not-package-me"
"#;

    fn config_with_boards() -> EngagementConfig {
        EngagementConfig::from_toml(BOARD_CONFIG, Path::new("engagement.toml")).expect("parses")
    }

    /// Package one member carrying `leaked`, and return what `assemble` did.
    fn packaging_a_member_carrying(
        work: &WorkDir,
        config: &EngagementConfig,
        leaked: &str,
    ) -> Result<ReturnPackage, AuditError> {
        packaging_a_member_carrying_with_token(work, config, leaked, None)
    }

    /// [`packaging_a_member_carrying`], with the `gh`-derived credential the
    /// caller wants in the needle set (#5980 CRITICAL 4).
    fn packaging_a_member_carrying_with_token(
        work: &WorkDir,
        config: &EngagementConfig,
        leaked: &str,
        github_token: Option<&str>,
    ) -> Result<ReturnPackage, AuditError> {
        install_record(work);
        let run = audited(work, "00-acme-api", "acme-api");
        std::fs::write(run.output.join("leaked.log"), leaked).expect("write");
        let report = RunReport::of(vec![run]);
        assemble(
            work,
            config,
            &report,
            &[],
            &default_destination(work),
            github_token,
        )
    }

    /// The JIRA token is a secret this crate now hands to a child, so the
    /// package guard owes it the refusal the OpenRouter key already gets.
    ///
    /// Against `5a615fe0e` this fails on the first assertion: `copy_member`
    /// scanned for `openrouter_key` alone, so the token was packaged and the
    /// engagement's own board credential left the recipient's network.
    #[test]
    fn a_member_carrying_the_jira_token_is_refused_and_leaves_no_zip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let destination = default_destination(&work);

        let err = packaging_a_member_carrying(
            &work,
            &config_with_boards(),
            "authorization: Basic jira-token-do-not-package-me\n",
        )
        .expect_err("a package carrying the JIRA token must not be produced");

        assert!(
            matches!(err, AuditError::CredentialInPackage { .. }),
            "{err:?}"
        );
        assert!(
            !destination.exists(),
            "a refused package must leave no file"
        );
        assert!(
            !destination.with_extension("zip.part").exists(),
            "the temporary must be removed too"
        );
    }

    /// #5980 CRITICAL 4: the `gh`-derived GitHub credential is a third source
    /// alongside the two board secrets above — it never lives in
    /// `EngagementConfig`, so `configured_secrets()` alone cannot see it.
    /// Before `secret_needles` took a `github_token` parameter, this member
    /// packaged clean and the token left the recipient's network.
    #[test]
    fn a_member_carrying_the_github_token_is_refused_and_leaves_no_zip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let destination = default_destination(&work);
        const TOKEN: &str = "ghp_do-not-package-me";

        let err = packaging_a_member_carrying_with_token(
            &work,
            &config(),
            "auth error: bad credentials (token ghp_do-not-package-me rejected)\n",
            Some(TOKEN),
        )
        .expect_err("a package carrying the GitHub token must not be produced");

        assert!(
            matches!(err, AuditError::CredentialInPackage { .. }),
            "{err:?}"
        );
        assert!(
            !destination.exists(),
            "a refused package must leave no file"
        );
    }

    /// #5980 follow-up CRITICAL: the critic's own proof, inverted, through
    /// the real `from_checkpoint` entry point both `session::package` and
    /// `chain::assemble` call. A file the sweep wrote carries the SWEEP-TIME
    /// account's token; packaging under a DIFFERENT account's token must
    /// refuse instead of silently shipping it — a freshly re-resolved token
    /// can never be the needle that catches an older one.
    ///
    /// Against the commit before this test existed, this scenario packaged
    /// successfully with the sweep-time token intact in the deliverable —
    /// `from_checkpoint` took a bare `Option<&str>` and had no checkpoint
    /// comparison at all.
    #[test]
    fn packaging_refuses_when_the_active_github_credential_has_changed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let stem = "00-acme-api";
        let run = audited(&work, stem, "acme-api");
        // What a child could echo at sweep time: the SWEEP-TIME token.
        std::fs::write(
            run.output.join("leaked.log"),
            "auth error: bad credentials (token ghp_sweep_time_account_A_token rejected)\n",
        )
        .expect("write");

        let sweep_time = GithubAccess::with_token("ghp_sweep_time_account_A_token");
        crate::run::checkpoint::write_progress(
            &work,
            &crate::run::RunProgress::finished(
                &RunReport::of(vec![run]),
                github_issues::GithubCredentialRecord::of(&sweep_time),
            ),
        )
        .expect("write checkpoint");

        let package_time = GithubAccess::with_token("ghp_package_time_account_B_token");
        let destination = default_destination(&work);
        let err = from_checkpoint(&work, &config(), &[], &destination, &package_time)
            .expect_err("a mismatched GitHub credential must refuse packaging");
        assert!(
            matches!(err, AuditError::GithubCredentialChanged),
            "{err:?}"
        );
        assert!(
            !destination.exists(),
            "a refused package must leave no file"
        );
    }

    /// The matched-fingerprint counterpart: the SAME account both times still
    /// packages, and the sweep-time token is caught by the outbound scan like
    /// any other member.
    #[test]
    fn packaging_proceeds_when_the_github_credential_matches() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let stem = "00-acme-api";
        let run = audited(&work, stem, "acme-api");

        let access = GithubAccess::with_token("ghp_same_account_both_times");
        crate::run::checkpoint::write_progress(
            &work,
            &crate::run::RunProgress::finished(
                &RunReport::of(vec![run]),
                github_issues::GithubCredentialRecord::of(&access),
            ),
        )
        .expect("write checkpoint");

        let destination = default_destination(&work);
        let same_access = GithubAccess::with_token("ghp_same_account_both_times");
        from_checkpoint(&work, &config(), &[], &destination, &same_access)
            .expect("a matching credential must not refuse packaging");
        assert!(destination.exists(), "the package must be written");
    }

    /// The back-compat case: a checkpoint written before credential
    /// verification existed carries no `github_credential` record at all.
    /// Packaging proceeds rather than refusing every pre-existing engagement
    /// outright, but the returned package states the uncertainty.
    #[test]
    fn packaging_an_unverified_checkpoint_proceeds_with_a_stated_gap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let stem = "00-acme-api";
        let run = audited(&work, stem, "acme-api");
        write_progress_without_github_credential(&work, &RunReport::of(vec![run]));

        let destination = default_destination(&work);
        let package = from_checkpoint(
            &work,
            &config(),
            &[],
            &destination,
            &GithubAccess::default(),
        )
        .expect("an unrecorded checkpoint must not refuse packaging");
        assert!(
            package
                .excluded
                .iter()
                .any(|line| line.contains("not recorded")),
            "the uncertainty must be stated: {:?}",
            package.excluded
        );
    }

    /// Writes a checkpoint the way a pre-#5980-follow-up sweep would have —
    /// no `github_credential` field at all, which `#[serde(default)]` reads
    /// back as `None` and `verify_unchanged` treats as unverifiable.
    ///
    /// Why hand-edit rather than hand-assemble: `RunProgress` is
    /// `#[non_exhaustive]`, so nothing outside `checkpoint.rs` can build one
    /// by struct literal, and hand-assembling the whole TOML document from
    /// scratch would silently drift from whatever `RunProgress`'s own
    /// `Serialize` impl actually produces. Serializing a real value and
    /// stripping just the one line this field owns keeps everything else
    /// exactly what the real serializer writes.
    fn write_progress_without_github_credential(work: &WorkDir, report: &RunReport) {
        let progress = crate::run::RunProgress::finished(
            report,
            github_issues::GithubCredentialRecord::NoToken,
        );
        let text = toml::to_string_pretty(&progress).expect("progress serializes");
        let stripped: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("github_credential"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_ne!(
            stripped, text,
            "the field must actually have been present to strip"
        );
        std::fs::write(crate::run::progress_path(work), stripped).expect("write raw checkpoint");
    }

    /// The Linear key, same reason. Two providers, one guard.
    #[test]
    fn a_member_carrying_the_linear_api_key_is_refused_and_leaves_no_zip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let destination = default_destination(&work);

        let err = packaging_a_member_carrying(
            &work,
            &config_with_boards(),
            "LINEAR_API_KEY=lin_api_do-not-package-me\n",
        )
        .expect_err("a package carrying the Linear key must not be produced");

        assert!(
            matches!(err, AuditError::CredentialInPackage { .. }),
            "{err:?}"
        );
        assert!(!destination.exists());
        assert!(!destination.with_extension("zip.part").exists());
    }

    /// Several needles in one member: neither board secret hides behind the
    /// other, and neither depends on the OpenRouter key being present to trip
    /// the guard.
    #[test]
    fn a_member_carrying_several_secrets_is_refused_on_the_first_pass() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());

        let err = packaging_a_member_carrying(
            &work,
            &config_with_boards(),
            "jira=jira-token-do-not-package-me\nlinear=lin_api_do-not-package-me\n",
        )
        .expect_err("a package carrying two board secrets must not be produced");

        assert!(
            matches!(err, AuditError::CredentialInPackage { .. }),
            "{err:?}"
        );
        assert!(!default_destination(&work).exists());
    }

    /// A member carrying none of them still packages — the widened scan must not
    /// refuse ordinary output.
    #[test]
    fn an_ordinary_member_still_packages_under_the_widened_scan() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());

        let package =
            packaging_a_member_carrying(&work, &config_with_boards(), "commits: 412\nauthors: 9\n")
                .expect("output carrying no secret packages");

        assert!(package.path.is_file(), "{package:?}");
    }

    /// Following a symlink would read a file outside the working directory into
    /// a package that leaves the recipient's network.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_member_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let outside = tmp.path().join("private.key");
        std::fs::write(&outside, "not ours to send").expect("write");
        let run = audited(&work, "00-acme-api", "acme-api");
        std::os::unix::fs::symlink(&outside, run.output.join("linked.key")).expect("symlink");
        let report = RunReport::of(vec![run]);
        let destination = default_destination(&work);

        let err = assemble(&work, &config(), &report, &[], &destination, None)
            .expect_err("a symlink must not be followed into the package");
        assert!(
            matches!(err, AuditError::UnsafePackageEntry { .. }),
            "{err:?}"
        );
        assert!(!destination.exists());
    }

    /// A hardlink is a second directory entry on the same inode, so
    /// `is_symlink()` is false for it and the symlink refusal does not see it.
    /// Its content is a file from outside the tree all the same.
    #[cfg(unix)]
    #[test]
    fn a_hardlinked_member_under_out_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let outside = tmp.path().join("private.key");
        std::fs::write(&outside, "not ours to send").expect("write");
        let run = audited(&work, "00-acme-api", "acme-api");
        std::fs::hard_link(&outside, run.output.join("linked.key")).expect("hard link");
        let report = RunReport::of(vec![run]);
        let destination = default_destination(&work);

        let err = assemble(&work, &config(), &report, &[], &destination, None)
            .expect_err("a hardlink must not carry outside content into the package");
        let AuditError::UnsafePackageEntry { kind, .. } = &err else {
            panic!("expected UnsafePackageEntry, got {err:?}");
        };
        assert_eq!(*kind, "hardlink");
        assert!(
            !destination.exists(),
            "a refused package must leave no file"
        );
        assert!(!destination.with_extension("zip.part").exists());
    }

    /// The same bypass through the other collector, which walks `extract/` with
    /// its own loop rather than through `collect_dir`.
    #[cfg(unix)]
    #[test]
    fn a_hardlinked_member_under_extract_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let outside = tmp.path().join("private.key");
        std::fs::write(&outside, "not ours to send").expect("write");
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api")]);
        // Named to match the `<stem>.db` prefix, so `collect_extract` picks it up.
        std::fs::hard_link(
            &outside,
            work.path(Area::Extract).join("00-acme-api.db-wal"),
        )
        .expect("hard link");
        let destination = default_destination(&work);

        let err = assemble(&work, &config(), &report, &[], &destination, None)
            .expect_err("a hardlink under extract/ must be refused too");
        let AuditError::UnsafePackageEntry { kind, .. } = &err else {
            panic!("expected UnsafePackageEntry, got {err:?}");
        };
        assert_eq!(*kind, "hardlink");
        assert!(!destination.exists());
    }

    /// The other side of the refusal: ordinary audit output has one link each,
    /// so widening the check to hardlinks must not start failing real sweeps.
    /// `a_completed_sweep_becomes_one_openable_zip` is the same guarantee end to
    /// end; this one states the link count the guarantee rests on.
    #[cfg(unix)]
    #[test]
    fn ordinary_audit_output_has_one_link_and_is_not_refused() {
        use std::os::unix::fs::MetadataExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let run = audited(&work, "00-acme-api", "acme-api");
        for path in [
            run.output.join("manifest.toml"),
            run.output.join("report.md"),
            work.path(Area::Extract).join("00-acme-api.db"),
        ] {
            let links = std::fs::symlink_metadata(&path).expect("stat").nlink();
            assert_eq!(links, 1, "{} has {links} links", path.display());
        }
        let report = RunReport::of(vec![run]);
        assemble(
            &work,
            &config(),
            &report,
            &[],
            &default_destination(&work),
            None,
        )
        .expect("ordinary output must still package");
    }

    /// A sweep in which nothing was audited must not produce a package that
    /// looks like a deliverable.
    #[test]
    fn a_sweep_that_audited_nothing_has_nothing_to_return() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let report = RunReport::of(vec![failed(&work, "00-acme-api", "acme-api")]);
        let destination = default_destination(&work);

        let err = assemble(&work, &config(), &report, &[], &destination, None)
            .expect_err("no audited repository means no package");
        assert!(
            matches!(err, AuditError::NothingToPackage { .. }),
            "{err:?}"
        );
        assert!(!destination.exists());
    }

    /// A partial sweep is packageable — DOC-67 §9 — but it must never read as a
    /// whole one, in the returned value or in the file the recipient opens.
    #[test]
    fn a_partial_sweep_names_what_the_package_omits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![
            audited(&work, "00-acme-api", "acme-api"),
            failed(&work, "01-acme-web", "acme-web"),
        ]);
        let destination = default_destination(&work);

        let package =
            assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");
        assert_eq!(package.excluded.len(), 1);
        assert!(
            package.excluded[0].contains("acme-web"),
            "{:?}",
            package.excluded
        );

        let readme = read_entry(&destination, README_ENTRY);
        assert!(readme.contains("Not in this package"), "{readme}");
        assert!(readme.contains("acme-web"), "{readme}");

        let metadata = read_entry(&destination, METADATA_ENTRY);
        assert!(metadata.contains("repositories_excluded = 1"), "{metadata}");

        // The failed repository's own REPORT must not ride along. Its log does,
        // under `failures/` — see `a_failed_repository_ships_its_log_and_a_record`.
        assert!(
            !entries(&destination)
                .iter()
                .any(|e| e.starts_with(&format!("{REPORTS_PREFIX}/01-acme-web"))),
            "the excluded repository's report files are in the package"
        );
    }

    /// 🔴 #6245: a repository that failed must leave a trace in the package —
    /// its own log, and a record naming what went wrong.
    ///
    /// Against `3771644d0` a failed target left NOTHING: no `failures/`
    /// directory, no log, only absence from `reports/`. Two of 59 repositories
    /// exited 1 on the 2026-08-25 run and shipped no trace at all, so "failed"
    /// and "never attempted" were the same observation from the outside.
    #[test]
    fn a_failed_repository_ships_its_log_and_a_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![
            audited(&work, "00-acme-api", "acme-api"),
            failed(&work, "01-acme-web", "acme-web"),
        ]);
        std::fs::write(
            &report.repos[1].log,
            "stage: collect\nfatal: could not read\n",
        )
        .expect("the child left a log");
        let destination = default_destination(&work);

        assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");

        let log = read_entry(&destination, "failures/01-acme-web.log");
        assert!(log.contains("fatal: could not read"), "{log}");

        let record = read_entry(&destination, FAILURES_ENTRY);
        for expected in ["acme-web", "exited with code 3", "failures/01-acme-web.log"] {
            assert!(record.contains(expected), "{record}");
        }
    }

    /// A repository whose child never wrote a log still gets a record, and the
    /// record says so — silence about a missing log is the defect one level
    /// down from the one #6245 is about.
    #[test]
    fn a_failure_with_no_log_still_gets_a_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![
            audited(&work, "00-acme-api", "acme-api"),
            failed(&work, "01-acme-web", "acme-web"),
        ]);
        let destination = default_destination(&work);

        assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");
        let record = read_entry(&destination, FAILURES_ENTRY);
        assert!(record.contains("no log survived"), "{record}");
        assert!(
            !entries(&destination).iter().any(|e| e.ends_with(".log")),
            "there was no log to package"
        );
    }

    /// A clean sweep gets no `failures/` directory: an index that says "none"
    /// reads worse than no directory at all.
    #[test]
    fn a_clean_sweep_has_no_failures_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api")]);
        let destination = default_destination(&work);

        assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");
        assert!(
            !entries(&destination)
                .iter()
                .any(|e| e.starts_with(FAILURES_PREFIX)),
            "{:?}",
            entries(&destination)
        );
    }

    /// The generated failure record quotes reason strings, and a reason can
    /// carry text a child produced — so it is scanned like every other member
    /// rather than trusted because this crate wrote it.
    #[test]
    fn a_failure_record_carrying_a_credential_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let mut leaking = failed(&work, "01-acme-web", "acme-web");
        leaking.result = RepoResult::Failed {
            reason: format!(
                "provider rejected the key {}",
                config().openrouter_key.expose()
            ),
        };
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api"), leaking]);
        let destination = default_destination(&work);

        let err = assemble(&work, &config(), &report, &[], &destination, None)
            .expect_err("a generated member carrying the key is refused");
        assert!(
            matches!(err, AuditError::CredentialInPackage { .. }),
            "{err:?}"
        );
        assert!(!destination.exists(), "a refused assembly leaves no zip");
    }

    /// The default lands inside the root that `rm -rf <work-dir>` removes, and
    /// it is not under an area the next assembly would sweep back in.
    #[test]
    fn the_default_destination_is_inside_the_root_but_outside_every_area() {
        let work = WorkDir::new("/engagement/work");
        let destination = default_destination(&work);
        assert!(destination.starts_with(work.root()));
        for (_, area) in work.layout() {
            assert!(!destination.starts_with(&area), "{}", destination.display());
        }
    }

    /// The recipient can put the file where they will attach it from. This is
    /// the one path on which this crate writes outside the working directory,
    /// and it happens only when asked in words.
    #[test]
    fn the_destination_can_be_chosen_outside_the_working_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api")]);
        let destination = tmp.path().join("Desktop/return.zip");

        let package =
            assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");
        assert_eq!(package.path, destination);
        assert!(destination.is_file());
        assert!(!destination.starts_with(work.root()));
    }

    /// WAL mode leaves sidecars, and a database shipped without them can be
    /// missing its last committed transactions.
    #[test]
    fn the_extract_databases_sidecars_travel_with_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api")]);
        std::fs::write(
            work.path(Area::Extract).join("00-acme-api.db-wal"),
            b"write-ahead log",
        )
        .expect("write wal");
        // A database belonging to a repository that is not in this package.
        std::fs::write(work.path(Area::Extract).join("09-other.db"), b"other").expect("write");
        let destination = default_destination(&work);

        assemble(&work, &config(), &report, &[], &destination, None).expect("assembles");
        let names = entries(&destination);
        assert!(
            names.contains(&"extract/00-acme-api.db".to_owned()),
            "{names:?}"
        );
        assert!(
            names.contains(&"extract/00-acme-api.db-wal".to_owned()),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("09-other")),
            "an unrelated database was packaged: {names:?}"
        );
    }

    /// A repository whose output directory exists but whose database was never
    /// written — build acceptance criterion 3 and #5862's closure condition 1:
    /// the report alone must never ship as if it were the whole deliverable.
    #[test]
    fn an_audited_repo_with_no_extract_database_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let run = audited(&work, "00-acme-api", "acme-api");
        // `audited` wrote the database already; remove it so the output
        // directory is present with no matching `extract/<stem>.db`.
        std::fs::remove_file(work.path(Area::Extract).join("00-acme-api.db")).expect("remove db");
        let report = RunReport::of(vec![run]);
        let destination = default_destination(&work);

        let err = assemble(&work, &config(), &report, &[], &destination, None)
            .expect_err("a report with no database must not be packaged");
        let AuditError::MissingExtractDatabase { repo, expected } = &err else {
            panic!("expected MissingExtractDatabase, got {err:?}");
        };
        assert_eq!(repo, "acme-api");
        assert!(
            expected.ends_with("00-acme-api.db"),
            "{}",
            expected.display()
        );
        assert!(
            !destination.exists(),
            "a refused package must leave no file"
        );
        assert!(!destination.with_extension("zip.part").exists());
    }

    /// The other route to the same absence: `extract/` was never created at
    /// all, rather than existing and simply lacking this repository's file.
    /// `collect_extract`'s `NotFound` branch used to treat this as `Ok(())`.
    #[test]
    fn an_audited_repo_with_no_extract_directory_at_all_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let run = audited(&work, "00-acme-api", "acme-api");
        std::fs::remove_dir_all(work.path(Area::Extract)).expect("remove extract dir");
        let report = RunReport::of(vec![run]);
        let destination = default_destination(&work);

        let err = assemble(&work, &config(), &report, &[], &destination, None)
            .expect_err("a missing extract/ directory must not be packaged");
        assert!(
            matches!(err, AuditError::MissingExtractDatabase { .. }),
            "{err:?}"
        );
        assert!(!destination.exists());
        assert!(!destination.with_extension("zip.part").exists());
    }
}
