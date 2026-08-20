//! The three members this crate GENERATES into the return package.
//!
//! Why: split out of `crate::package` when the packaged index (#6080) pushed
//! that file past the 500-SLOC production cap — the second split, after
//! `credential_scan`. It separates on a real line: `package.rs` decides what
//! goes into the archive and refuses what must not, and this file writes the
//! three files that describe the result. Nothing here reads the filesystem
//! except the tool record, and nothing here writes.
//!
//! What: [`render_readme`] (the page the recipient reads before sending),
//! [`render_metadata`] (`package.toml`), and [`render_index`]
//! (`reports/index.md`, #6080), plus [`exclusions`], the one line per
//! repository the package does not cover that the first two share.
//!
//! Test: `super::package_tests`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{EXTRACT_PREFIX, METADATA_ENTRY, REPORTS_PREFIX, output_stem};
use crate::config::EngagementConfig;
use crate::error::AuditError;
use crate::run::{RepoRun, RunReport};
use crate::tools;
use crate::workdir::WorkDir;

/// The three members this crate writes into the archive itself.
///
/// Why: one value rather than three `String` parameters threaded through
/// `super::write_archive` and `super::fill_archive` — the index (#6080) was the
/// third, and three adjacent strings of the same type at a call site is where an
/// argument goes into the wrong slot silently.
/// What: the file contents, in the order they are written. Each field's entry
/// name is the matching `super::*_ENTRY` constant.
pub(super) struct Generated {
    /// [`super::README_ENTRY`] — the page read before the package is sent.
    pub(super) readme: String,
    /// [`super::METADATA_ENTRY`] — coverage and versions, machine-readable.
    pub(super) metadata: String,
    /// [`super::INDEX_ENTRY`] — what the reports are, and a link to each.
    pub(super) index: String,
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

/// The `reports/index.md` this package carries (#6080).
///
/// Why: the recipient is the primary audience for an explain-the-contents page,
/// and they get the zip rather than the working directory the sweep wrote. The
/// sweep's own `out/index.md` is NOT copied in: it lists repositories whose
/// reports are excluded from the package and links logs the package does not
/// carry, so copied verbatim it would point the recipient at files that are not
/// there.
/// What: one entry per repository the sweep attempted, its files taken from the
/// zip entries `collected` names, and no log — see
/// [`crate::index_report::Producer::Package`]. Rendered against
/// [`REPORTS_PREFIX`], which is where the member lands, so `<stem>/report.md`
/// and `../extract/<stem>.db` both resolve inside the archive.
/// Test: `super::package_tests::the_package_carries_an_index_of_its_reports`,
/// `super::package_tests::the_package_index_links_resolve_inside_the_zip`.
pub(super) fn render_index(
    work: &WorkDir,
    report: &RunReport,
    collected: &[(String, PathBuf)],
) -> String {
    let entries = report
        .repos
        .iter()
        .map(|run| {
            let stem = output_stem(run);
            let under = format!("{REPORTS_PREFIX}/{stem}/");
            let database = format!("{EXTRACT_PREFIX}/{stem}.db");
            crate::index_report::IndexEntry {
                name: run.repo.name.clone(),
                files: collected
                    .iter()
                    .filter(|(entry, _)| entry.starts_with(&under) || *entry == database)
                    .map(|(entry, _)| PathBuf::from(entry))
                    .collect(),
                dir: PathBuf::from(format!("{REPORTS_PREFIX}/{stem}")),
                // The archive carries no `logs/`: a log is diagnostic material
                // for the operator who ran the sweep, not for the recipient.
                log: None,
                failure: match &run.result {
                    crate::run::RepoResult::Failed { reason } => Some(reason.clone()),
                    crate::run::RepoResult::Succeeded => None,
                },
                carried_over: run.resumed,
                duration: run.duration_ms.map(std::time::Duration::from_millis),
            }
        })
        .collect::<Vec<_>>();
    // The sum, not a wall clock — packaging did not time the sweep, and
    // `Producer::Package`'s timing note says so rather than letting the row
    // read as one.
    let measured: u64 = report.repos.iter().filter_map(|r| r.duration_ms).sum();
    let index = crate::index_report::IndexReport {
        producer: crate::index_report::Producer::Package,
        generated_at: crate::index_report::local_now(),
        tools: crate::index_report::recorded_tools(work),
        entries,
        total: (measured > 0).then(|| std::time::Duration::from_millis(measured)),
    };
    crate::index_report::render(&index, Path::new(REPORTS_PREFIX))
}

/// One line per repository the package does not cover.
pub(super) fn exclusions(report: &RunReport) -> Vec<String> {
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

pub(super) fn render_metadata(
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
pub(super) fn render_readme(
    config: &EngagementConfig,
    audited: &[&RepoRun],
    excluded: &[String],
) -> String {
    let mut out = String::from(
        "# Audit return package\n\n\
         This is the deliverable to send back. It is a plain zip with **no encryption \
         and no password** — open it and read exactly what you are about to send.\n\n\
         ## What is inside\n\n\
         | Path | Contents |\n|---|---|\n\
         | `README.md` | this file |\n\
         | `package.toml` | which repositories were audited, at which tool versions |\n\
         | `reports/index.md` | start here — what every file below is, and a link to each report |\n\
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
