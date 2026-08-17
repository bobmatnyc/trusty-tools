//! Assembling the deliverable that goes back, and saying where it landed.
//!
//! Why: #5499 is the client's last responsibility. Everything before it produces
//! artifacts scattered across the working directory — per-repository reports
//! under `out/`, extract databases under `extract/`, the verified tool triple
//! under `state/` — and none of that is something a recipient can attach to an
//! email. This module reduces them to one file and prints its path.
//!
//! What: [`assemble`] collects what a completed sweep produced, generates the
//! two files that explain it, and writes a single zip. [`ReturnPackage`] is what
//! a front end renders — the path, every member, and every repository left out.
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
//! Two refusals, both before a single byte reaches the destination:
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
//!   governs those. Every member's bytes are scanned as they are copied. See
//!   [`AuditError::CredentialInPackage`].
//!
//! Both are refusals rather than omissions, and the package is built in a
//! temporary file that is removed on either, so a refused assembly leaves no
//! partial zip a recipient could send by mistake.
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

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::EngagementConfig;
use crate::error::AuditError;
use crate::run::{RepoRun, RunReport};
use crate::tools;
use crate::workdir::{Area, WorkDir};

/// Filename of the return package, when the recipient does not choose one.
pub const PACKAGE_FILE_NAME: &str = "audit-return-package.zip";

/// The generated metadata member: engagement labels, tool versions, coverage.
pub const METADATA_ENTRY: &str = "package.toml";

/// The generated member that tells the recipient what they are about to send.
pub const README_ENTRY: &str = "README.md";

/// Directory inside the zip holding one subdirectory per audited repository.
pub const REPORTS_PREFIX: &str = "reports";

/// Directory inside the zip holding the tga extract databases (#5479).
pub const EXTRACT_PREFIX: &str = "extract";

/// How much of a member is read at a time while copying and scanning it.
const CHUNK_BYTES: usize = 64 * 1024;

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

/// The generated `package.toml`.
///
/// Why: #5478's config is what arrives; this is the metadata half of it that
/// goes back. It is a SEPARATE type from [`EngagementConfig`] rather than a
/// redacted copy, so there is no field for the credential to be carried in and
/// no `skip_serializing` a later edit could remove.
/// What: scalars first, then the two table arrays — TOML requires that order.
/// `not_attempted` is a plain string array, so it sits with the scalars.
#[derive(Debug, Serialize)]
struct PackageMetadata {
    generated_by: String,
    client: Option<String>,
    engagement: Option<String>,
    instructions: String,
    repositories_audited: usize,
    repositories_excluded: usize,
    /// Targets that never reached the sweep, so `repositories` cannot list them
    /// and `repositories_excluded` cannot count them (#5824).
    ///
    /// A repository that failed to clone and a registered board are both here.
    /// Kept separate from `repositories_excluded` rather than folded into it,
    /// because a board is not a repository and counting one as an excluded
    /// repository would overstate what the sweep was asked to do. Always
    /// emitted, so an empty array is a positive claim of full coverage rather
    /// than an absent key a reader has to interpret.
    not_attempted: Vec<String>,
    tools: Vec<ToolVersion>,
    repositories: Vec<PackagedRepo>,
}

/// One tool of the pinned triple that produced the reports (#5495).
///
/// The recorded binary PATH is deliberately absent: it names a directory on the
/// recipient's machine and says nothing about provenance the version does not.
#[derive(Debug, Serialize)]
struct ToolVersion {
    name: String,
    version: String,
}

/// One repository's coverage, as the package states it.
#[derive(Debug, Serialize)]
struct PackagedRepo {
    name: String,
    audited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    gaps: Vec<String>,
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
/// Test: `super::package_tests`.
///
/// # Errors
///
/// [`AuditError::NothingToPackage`] when no repository was audited,
/// [`AuditError::UnsafePackageEntry`] for a symlink or hardlink under `out/` or
/// `extract/`,
/// [`AuditError::CredentialInPackage`] when a member carries the engagement key,
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
/// # Errors
///
/// [`AuditError::NothingToPackage`] when no sweep has finished here, and
/// whatever [`assemble`] fails with.
pub fn from_checkpoint(
    work: &WorkDir,
    config: &EngagementConfig,
    unattempted: &[String],
    destination: &Path,
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
    assemble(work, config, &progress.report(), unattempted, destination)
}

pub fn assemble(
    work: &WorkDir,
    config: &EngagementConfig,
    report: &RunReport,
    unattempted: &[String],
    destination: &Path,
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
        collect_extract(work, &stem, &mut collected)?;
    }
    collected.sort_by(|a, b| a.0.cmp(&b.0));

    // #5824: the sweep's own failures, then the targets that never reached it.
    let mut excluded = exclusions(report);
    excluded.extend(unattempted.iter().cloned());
    let metadata = render_metadata(work, config, report, &audited, unattempted)?;
    let readme = render_readme(config, &audited, &excluded);

    write_archive(destination, config, readme, metadata, collected, excluded)
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

/// One repository's extract database, and any sidecar SQLite left beside it.
///
/// Matching by PREFIX rather than by exact name is what picks up the `-wal` and
/// `-shm` files WAL mode leaves: a database shipped without them can be missing
/// the last committed transactions.
fn collect_extract(
    work: &WorkDir,
    stem: &str,
    into: &mut Vec<(String, PathBuf)>,
) -> Result<(), AuditError> {
    let dir = work.path(Area::Extract);
    let wanted = format!("{stem}.db");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(AuditError::Package { path: dir, source }),
    };
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
            into.push((format!("{EXTRACT_PREFIX}/{name}"), entry.path()));
        }
    }
    Ok(())
}

/// One line per repository the package does not cover.
fn exclusions(report: &RunReport) -> Vec<String> {
    report
        .failures()
        .map(|run| match &run.result {
            crate::run::RepoResult::Failed { reason } => {
                format!("{} is not in this package — {reason}", run.repo.name)
            }
            crate::run::RepoResult::Succeeded => unreachable!("failures() yields only failures"),
        })
        .collect()
}

fn render_metadata(
    work: &WorkDir,
    config: &EngagementConfig,
    report: &RunReport,
    audited: &[&RepoRun],
    unattempted: &[String],
) -> Result<String, AuditError> {
    let tools = tools::read_record(work)?
        .into_iter()
        .map(|t| ToolVersion {
            name: t.crate_name,
            version: t.version,
        })
        .collect();
    let repositories = report
        .repos
        .iter()
        .map(|run| PackagedRepo {
            name: run.repo.name.clone(),
            audited: run.result.succeeded(),
            reason: match &run.result {
                crate::run::RepoResult::Failed { reason } => Some(reason.clone()),
                crate::run::RepoResult::Succeeded => None,
            },
            gaps: run.gaps.clone(),
        })
        .collect();
    let metadata = PackageMetadata {
        generated_by: format!("trusty-audit {}", env!("CARGO_PKG_VERSION")),
        client: config.client.clone(),
        engagement: config.engagement.clone(),
        instructions: config.instructions.clone(),
        repositories_audited: audited.len(),
        repositories_excluded: report.repos.len() - audited.len(),
        not_attempted: unattempted.to_vec(),
        tools,
        repositories,
    };
    toml::to_string_pretty(&metadata).map_err(|e| AuditError::Package {
        path: PathBuf::from(METADATA_ENTRY),
        source: std::io::Error::other(e),
    })
}

/// The page the recipient reads before sending the file.
///
/// The content claim is #5479's, worded as that issue requires: "no file
/// content, diffs, patches, hunks, or blobs" — never "no code", because
/// free-text columns carry whatever a human pasted into them.
fn render_readme(config: &EngagementConfig, audited: &[&RepoRun], excluded: &[String]) -> String {
    let mut out = String::from(
        "# Audit return package\n\n\
         This is the deliverable to send back. It is a plain zip with **no encryption \
         and no password** — open it and read exactly what you are about to send.\n\n\
         ## What is inside\n\n\
         | Path | Contents |\n|---|---|\n\
         | `README.md` | this file |\n\
         | `package.toml` | which repositories were audited, at which tool versions |\n\
         | `reports/<repo>/` | the rendered report and manifest for one repository |\n\
         | `extract/<repo>.db` | the tga extract database those reports were computed from |\n\n\
         ## What is not inside\n\n\
         - **No credential.** The OpenRouter key in your engagement config never \
         reaches this package; every file was scanned for it while the zip was written.\n\
         - **No source code as such.** The extract database holds no file content, \
         diffs, patches, hunks, or blobs. It does hold free-text fields — commit \
         messages, pull-request and work-item titles, classification notes — so a \
         snippet a person pasted into one of those is in it.\n\
         - **No signature yet.** Content signing is separate work (#5481); until it \
         lands nothing here proves the package was not altered after it was written.\n\n",
    );
    if let Some(client) = &config.client {
        out.push_str(&format!("Engagement: {client}\n\n"));
    }
    out.push_str(&format!(
        "## Coverage\n\n{} audited.\n",
        count_repos(audited.len())
    ));
    if excluded.is_empty() {
        out.push_str("\nEvery repository the sweep was asked to audit is in this package.\n");
    } else {
        out.push_str("\nNot in this package:\n\n");
        for line in excluded {
            out.push_str(&format!("- {line}\n"));
        }
    }
    out
}

fn count_repos(n: usize) -> String {
    format!("{n} {}", if n == 1 { "repository" } else { "repositories" })
}

/// Write the archive beside `destination`, then rename it into place.
///
/// The temporary file is what makes the two refusals meaningful: a credential
/// found in the last member removes a `.part` file rather than leaving a
/// finished-looking zip the recipient might send.
fn write_archive(
    destination: &Path,
    config: &EngagementConfig,
    readme: String,
    metadata: String,
    collected: Vec<(String, PathBuf)>,
    excluded: Vec<String>,
) -> Result<ReturnPackage, AuditError> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AuditError::Package {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temporary = destination.with_extension("zip.part");
    let result = fill_archive(&temporary, config, readme, metadata, collected);
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
    readme: String,
    metadata: String,
    collected: Vec<(String, PathBuf)>,
) -> Result<Vec<PackagedFile>, AuditError> {
    let file = std::fs::File::create(temporary).map_err(|source| AuditError::Package {
        path: temporary.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let mut files = Vec::with_capacity(collected.len() + 2);

    for (entry, text) in [(README_ENTRY, readme), (METADATA_ENTRY, metadata)] {
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
        let bytes = copy_member(&mut zip, &entry, &source, config, temporary)?;
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

/// Copy one file into the archive, refusing it if it carries the credential.
fn copy_member(
    zip: &mut Archive,
    entry: &str,
    source: &Path,
    config: &EngagementConfig,
    temporary: &Path,
) -> Result<u64, AuditError> {
    let mut input = std::fs::File::open(source).map_err(|e| AuditError::Package {
        path: source.to_path_buf(),
        source: e,
    })?;
    let bytes = input.metadata().map(|m| m.len()).unwrap_or(0);
    start(zip, entry, bytes, temporary)?;

    let needle = config.openrouter_key.expose().as_bytes().to_vec();
    let mut scan = CredentialScan::over(&needle);
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    let mut written = 0_u64;
    loop {
        let read = input.read(&mut buffer).map_err(|e| AuditError::Package {
            path: source.to_path_buf(),
            source: e,
        })?;
        if read == 0 {
            break;
        }
        if scan.feed(&buffer[..read]) {
            return Err(AuditError::CredentialInPackage {
                path: source.to_path_buf(),
            });
        }
        zip.write_all(&buffer[..read])
            .map_err(|source| AuditError::Package {
                path: temporary.to_path_buf(),
                source,
            })?;
        written += read as u64;
    }
    Ok(written)
}

/// A substring search across a stream, without holding the stream in memory.
///
/// Why: the credential check has to cover an extract database that can run to
/// hundreds of megabytes, and a match that straddles two reads is exactly the
/// case a naive per-chunk search misses. Keeping the last `needle.len() - 1`
/// bytes as the next window's prefix is what closes that gap.
/// Test: `super::package_tests::a_credential_split_across_two_reads_is_caught`.
struct CredentialScan<'a> {
    needle: &'a [u8],
    tail: Vec<u8>,
}

impl<'a> CredentialScan<'a> {
    fn over(needle: &'a [u8]) -> Self {
        Self {
            needle,
            tail: Vec::new(),
        }
    }

    /// Whether the needle appears in the stream up to and including `chunk`.
    fn feed(&mut self, chunk: &[u8]) -> bool {
        if self.needle.is_empty() {
            return false;
        }
        let mut window = std::mem::take(&mut self.tail);
        window.extend_from_slice(chunk);
        let found = window
            .windows(self.needle.len())
            .any(|candidate| candidate == self.needle);
        let keep = (self.needle.len() - 1).min(window.len());
        self.tail = window[window.len() - keep..].to_vec();
        found
    }
}

#[cfg(test)]
mod package_tests {
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
            },
            output,
            log: work.path(Area::Logs).join(format!("{stem}.log")),
            gaps: Vec::new(),
            resumed: false,
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
                },
                output: work.path(Area::Output).join(stem),
                log: work.path(Area::Logs).join(format!("{stem}.log")),
                gaps: Vec::new(),
                resumed: false,
                result: RepoResult::Succeeded,
            }
        }
    }

    fn install_record(work: &WorkDir) {
        std::fs::write(
            tools::record_path(work),
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

        let package = assemble(&work, &config(), &report, &[], &destination).expect("assembles");

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

    /// The transparency premise: a recipient can open it with no password, and
    /// the README says what they are sending.
    #[test]
    fn the_package_is_unencrypted_and_says_what_it_holds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        install_record(&work);
        let report = RunReport::of(vec![audited(&work, "00-acme-api", "acme-api")]);
        let destination = default_destination(&work);
        assemble(&work, &config(), &report, &[], &destination).expect("assembles");

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

        let err = assemble(&work, &config(), &report, &[], &destination)
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

    /// A needle straddling two reads is the case the chunked scan exists for.
    #[test]
    fn a_credential_split_across_two_reads_is_caught() {
        let needle = b"sk-or-v1-secret";
        let mut scan = CredentialScan::over(needle);
        assert!(!scan.feed(b"noise sk-or-v1-"));
        assert!(scan.feed(b"secret more noise"));

        // And an empty key never matches everything.
        let mut blank = CredentialScan::over(b"");
        assert!(!blank.feed(b"anything at all"));
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

        let err = assemble(&work, &config(), &report, &[], &destination)
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

        let err = assemble(&work, &config(), &report, &[], &destination)
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

        let err = assemble(&work, &config(), &report, &[], &destination)
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
        assemble(&work, &config(), &report, &[], &default_destination(&work))
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

        let err = assemble(&work, &config(), &report, &[], &destination)
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

        let package = assemble(&work, &config(), &report, &[], &destination).expect("assembles");
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

        // The failed repository's own output must not ride along.
        assert!(
            !entries(&destination)
                .iter()
                .any(|e| e.contains("01-acme-web")),
            "the excluded repository's files are in the package"
        );
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

        let package = assemble(&work, &config(), &report, &[], &destination).expect("assembles");
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

        assemble(&work, &config(), &report, &[], &destination).expect("assembles");
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
}
