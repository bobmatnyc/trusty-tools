//! The `index.md` a run leaves beside the reports it produced (#6080).
//!
//! Why: an output directory holding `00-acme-api/`, `01-acme-web/`, `extract/`
//! and a pile of `.md` files says nothing about itself. The recipient opening it
//! cannot tell which file is the report, which repository has none, which
//! versions of the four tools produced any of it, when the run happened, or
//! where the hours went. Every one of those facts is known to the process that
//! wrote the directory and none of it was written down, so answering "what am I
//! looking at" meant asking the person who ran it.
//!
//! What: one Markdown file, written UNCONDITIONALLY — a single-repository run
//! gets one too, because "there is only one report" is a fact about coverage
//! that the index is the right place to state. It carries the tool versions, the
//! local time the run finished, the wall clock per unit and overall, a table of
//! what each file in the directory is, and a relative link to every report file
//! that exists. A unit with no report is listed with the reason and a link to
//! its log, so a partial run's index says what is missing rather than quietly
//! being shorter.
//!
//! Both producers write one through this module: [`crate::run`]'s sweep into
//! `out/`, and [`crate::rerender`]'s re-render into its `--out` directory. One
//! implementation, so the two indexes cannot describe the same fact differently.
//!
//! ## What the timings are, and are not
//!
//! Wall clock measured by THIS process around each unit of work it drives — one
//! duration per repository for the sweep, one per report for the re-render, plus
//! the run's own total. That is the granularity the process has: `tga audit`
//! runs nine stages per repository and relays each one as a display event
//! (`crate::progress`), but no stage event carries a duration and nothing
//! records when one began, so a per-stage table would be invented rather than
//! measured. The index says what was measured and no more.
//!
//! ## The clock
//!
//! [`local_now`] reads the wall clock; every other function in this module takes
//! the already-formatted string. That keeps the rendering provable against a
//! fixed timestamp — the same shape [`crate::workdir::WorkDir::resolve`] uses
//! for the environment — and leaves the one impure call at the call site, where
//! a reader can see it.
//!
//! Test: `super::index_tests`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::error::AuditError;

/// The index file's name, in whichever directory a run wrote its reports into.
///
/// `index.md` is unclaimed in both layouts: the sweep names each repository's
/// directory `<NN>-<repo>` and the re-render names each after the directory its
/// manifest came from, so neither can produce a sibling file of this name.
pub const INDEX_FILE: &str = "index.md";

/// Which run wrote the index, and so how its contents section reads.
///
/// The two directories hold different things — a sweep's `out/` has siblings
/// under `extract/` and `logs/`, a re-render's `--out` has the child logs beside
/// the report directories — and a contents table that described the other one
/// would send the reader to files that are not there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Producer {
    /// `trusty-audit run` / `audit`, writing `out/` (`crate::run`).
    Sweep,
    /// `trusty-audit render`, writing its `--out` directory (`crate::rerender`).
    Render,
}

/// One tool that produced these reports, and how its version was learned.
///
/// Why: `source` exists so an absent version is a STATEMENT rather than a blank
/// cell. "not recorded — a re-render runs no `tga`" and "not recorded — the
/// binary would not answer `--version`" are different facts, and a reader who
/// gets an empty cell has to guess which one they are looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolVersion {
    /// The tool, as the engagement names it — `tga`, `trusty-review`.
    pub name: String,
    /// Its version, or `None` when this run could not learn it.
    pub version: Option<String>,
    /// Where the version came from, or why there is none.
    pub source: String,
}

impl ToolVersion {
    /// A version this run learned, and the channel it learned it from.
    pub fn known(name: impl Into<String>, version: String, source: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: Some(version),
            source: source.into(),
        }
    }

    /// A version this run does not have, and why not.
    pub fn unknown(name: impl Into<String>, why: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            source: why.into(),
        }
    }
}

/// One unit of work the run drove, and what it left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndexEntry {
    /// What the operator calls it — a repository's `owner/name`, a report's stem.
    pub name: String,
    /// The directory this unit's files were written into.
    ///
    /// It need not exist: a repository whose child never started has none, and
    /// the entry is then listed with its failure instead of with links.
    pub dir: PathBuf,
    /// The child's combined output, when this run kept one.
    pub log: Option<PathBuf>,
    /// Why this unit produced no report, or `None` when it produced one.
    pub failure: Option<String>,
    /// Whether an earlier run did this work and this one carried it over.
    pub carried_over: bool,
    /// Wall clock around this unit, or `None` when this run did not measure it.
    pub duration: Option<Duration>,
}

/// Everything the index states, before it is rendered against a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndexReport {
    /// Which run wrote it.
    pub producer: Producer,
    /// Local time with its UTC offset, from [`local_now`].
    pub generated_at: String,
    /// The tools responsible, in the order they are worth reading.
    pub tools: Vec<ToolVersion>,
    /// One entry per unit the run drove, in the run's own order.
    pub entries: Vec<IndexEntry>,
    /// Wall clock around the whole run.
    pub total: Option<Duration>,
}

/// The current local time with its UTC offset, e.g. `2026-08-19 22:40:11 -04:00`.
///
/// Local rather than UTC because the reader of an index is the operator who ran
/// it or the auditor they sent it to, and "when did this run" is a question both
/// ask in their own time. The offset is carried so the value stays unambiguous
/// once it has travelled.
pub fn local_now() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string()
}

/// The version `binary --version` reports, or `None` when it will not say.
///
/// Why: the version a run RECORDS at install time and the version the binary it
/// actually spawned answers with are two different claims, and the re-render has
/// only the second — it resolves a `trusty-review` from `--review-bin`, the
/// working directory or `PATH` and drives whatever that is (`crate::rerender`).
/// What: spawns the binary once, takes the first line of its stdout, and drops a
/// leading token equal to the binary's own file name so the version column does
/// not repeat the tool column. Anything that fails — the binary is absent, exits
/// non-zero, says nothing — is `None`, which the index renders as a stated gap
/// rather than a blank.
///
/// Synchronous inside async callers, for the reason `crate::run::approve`
/// records: it is one short-lived child per run, not per repository.
/// Test: `super::index_tests::a_version_line_drops_the_repeated_binary_name`,
/// `super::index_tests::a_binary_that_will_not_answer_has_no_version`.
pub fn tool_version(binary: &Path) -> Option<String> {
    let finished = std::process::Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !finished.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&finished.stdout);
    Some(strip_binary_name(binary, text.lines().next()?.trim())).filter(|v| !v.is_empty())
}

/// `trusty-review 0.16.0` → `0.16.0`; anything else is left alone.
fn strip_binary_name(binary: &Path, line: &str) -> String {
    let name = binary
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    line.strip_prefix(&name)
        .map_or(line, str::trim_start)
        .to_owned()
}

/// Write the index into `dir`, replacing any earlier one.
///
/// # Postconditions
/// On `Ok`, `dir/index.md` describes this run: the versions, the timestamp, the
/// timings, and one link per report file that EXISTS at the moment of writing.
/// Nothing outside `dir` is written — which is what keeps
/// `crate::rerender`'s "the source package is only read" postcondition true when
/// the re-render writes an index of its own.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when the file cannot be written. It is deliberately
/// propagated rather than swallowed: the index is a member of the deliverable,
/// and a directory that has just absorbed every report but will not take one
/// more small file is a broken disk, not a cosmetic problem.
pub fn write(report: &IndexReport, dir: &Path) -> Result<(), AuditError> {
    let path = dir.join(INDEX_FILE);
    std::fs::write(&path, render(report, dir))
        .map_err(|source| AuditError::WorkDir { path, source })
}

/// The index's Markdown, with every link relative to `dir`.
///
/// What: reads `dir` to list each entry's files, so a link is only written for a
/// file that is there — a report the child never wrote is named as missing
/// rather than linked into a 404. The listing is sorted, so two runs over the
/// same directory produce the same order.
/// Test: `super::index_tests`.
pub fn render(report: &IndexReport, dir: &Path) -> String {
    let mut out = String::new();
    out.push_str("# Audit report index\n\n");
    out.push_str(
        "This file was written by `trusty-audit` beside the reports it produced. \
         It states which tool versions are responsible, when the run finished, how \
         long each piece took, what every file here is, and where each report is.\n\n",
    );
    summary(report, &mut out);
    versions(report, &mut out);
    timings(report, &mut out);
    reports(report, dir, &mut out);
    contents(report.producer, &mut out);
    out
}

/// The four facts worth having before any table.
fn summary(report: &IndexReport, out: &mut String) {
    let produced = report
        .entries
        .iter()
        .filter(|e| e.failure.is_none())
        .count();
    let unit = report.producer.unit(report.entries.len());
    out.push_str(&format!("- Generated: {}\n", report.generated_at));
    out.push_str(&format!(
        "- Produced by: trusty-audit {}\n",
        env!("CARGO_PKG_VERSION")
    ));
    out.push_str(&format!(
        "- Reports: {} of {} {}\n",
        produced,
        report.entries.len(),
        unit
    ));
    match report.total {
        Some(total) => out.push_str(&format!("- Total wall clock: {}\n\n", human(total))),
        None => out.push_str("- Total wall clock: not recorded\n\n"),
    }
}

/// The versions responsible for everything in this directory.
fn versions(report: &IndexReport, out: &mut String) {
    out.push_str("## Versions\n\n| tool | version | source |\n| --- | --- | --- |\n");
    out.push_str(&format!(
        "| `trusty-audit` | {} | this binary |\n",
        env!("CARGO_PKG_VERSION")
    ));
    for tool in &report.tools {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            tool.name,
            tool.version.as_deref().unwrap_or("not recorded"),
            tool.source
        ));
    }
    out.push_str(
        "\n`trusty-audit` bakes no build-time git metadata, so its git revision is \
         **not recorded** — the version above is what it was compiled at.\n\n",
    );
}

/// How long each piece took, and what that measurement covers.
fn timings(report: &IndexReport, out: &mut String) {
    out.push_str("## Timing\n\n");
    out.push_str(&format!("{}\n\n", report.producer.timing_note()));
    out.push_str("| unit | wall clock | outcome |\n| --- | --- | --- |\n");
    for entry in &report.entries {
        let time = match entry.duration {
            Some(d) => human(d),
            None => "not recorded".to_owned(),
        };
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            entry.name,
            time,
            entry.outcome()
        ));
    }
    let total = match report.total {
        Some(total) => human(total),
        None => "not recorded".to_owned(),
    };
    out.push_str(&format!("| **total** | **{total}** | |\n\n"));
}

/// One section per unit: its files, or why it has none.
fn reports(report: &IndexReport, dir: &Path, out: &mut String) {
    out.push_str("## Reports\n\n");
    for entry in &report.entries {
        out.push_str(&format!("### {}\n\n", entry.name));
        if let Some(reason) = &entry.failure {
            out.push_str(&format!("No report — {reason}\n\n"));
        }
        let files = files_in(&entry.dir);
        if files.is_empty() && entry.failure.is_none() {
            out.push_str(&format!(
                "No files at `{}` — the run recorded this one as finished, so the \
                 directory has been emptied or moved since.\n\n",
                relative(dir, &entry.dir)
            ));
        }
        for file in &files {
            out.push_str(&format!("- {}\n", link(dir, file)));
        }
        // Only a log that is actually there — a link into a file the run never
        // wrote is worse than no link.
        if let Some(log) = entry.log.as_ref().filter(|log| log.is_file()) {
            out.push_str(&format!("- log: {}\n", link(dir, log)));
        }
        out.push('\n');
    }
    if report.entries.is_empty() {
        out.push_str("This run produced no reports.\n\n");
    }
}

/// What each file and directory here is.
fn contents(producer: Producer, out: &mut String) {
    out.push_str("## What is in this directory\n\n| path | what it is |\n| --- | --- |\n");
    for (path, what) in producer.contents() {
        out.push_str(&format!("| `{path}` | {what} |\n"));
    }
    out.push('\n');
    out.push_str(producer.closing_note());
    out.push('\n');
}

impl Producer {
    /// The noun for one unit of this producer's work, agreeing with `count`.
    fn unit(self, count: usize) -> &'static str {
        match (self, count) {
            (Producer::Sweep, 1) => "repository",
            (Producer::Sweep, _) => "repositories",
            (Producer::Render, 1) => "report",
            (Producer::Render, _) => "reports",
        }
    }

    /// What the timing table measured, said before it is read.
    fn timing_note(self) -> &'static str {
        match self {
            Producer::Sweep => {
                "Wall clock measured around each repository's `tga audit` child, plus this \
                 sweep's own total. The child reports nine stages per repository but no \
                 stage carries a duration, so there is no per-stage breakdown to give — \
                 these are the durations this run actually measured."
            }
            Producer::Render => {
                "Wall clock measured around each `trusty-review report` child, plus this \
                 re-render's own total. Each per-report figure includes the indexing and \
                 measurement of any checkout present on this machine, which runs before \
                 the child."
            }
        }
    }

    /// The contents table's rows, in the order a reader meets them.
    fn contents(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Producer::Sweep => vec![
                ("index.md", "this file"),
                (
                    "<NN>-<repo>/",
                    "one directory per selected repository, numbered by its place in the selection",
                ),
                (
                    "<NN>-<repo>/manifest.toml",
                    "what `tga audit` collected — the interface `trusty-review` renders the report from, and the file `trusty-audit render` re-reads",
                ),
                (
                    "<NN>-<repo>/*.md",
                    "the rendered report and its companion documents, such as the authorship analysis",
                ),
                (
                    "../extract/<NN>-<repo>.db",
                    "the `tga` extract database built from that repository's git history — metrics, never file contents",
                ),
                (
                    "../logs/<NN>-<repo>.log",
                    "the combined stdout and stderr of that repository's `tga audit` child, with known credentials removed",
                ),
            ],
            Producer::Render => vec![
                ("index.md", "this file"),
                (
                    "<name>/",
                    "one directory per manifest re-rendered, named after the directory the manifest came from",
                ),
                (
                    "<name>/*",
                    "the files `trusty-review report` wrote for that manifest",
                ),
                (
                    "<name>.log",
                    "the combined stdout and stderr of that report's `trusty-review report` child, with the credential removed",
                ),
            ],
        }
    }

    /// The one caveat each producer owes its reader.
    fn closing_note(self) -> &'static str {
        match self {
            Producer::Sweep => {
                "A repository listed above with no report was attempted and did not finish; \
                 its log is the record of why. `trusty-audit package` assembles only the \
                 repositories that produced one, and names the rest.\n"
            }
            Producer::Render => {
                "Everything here was produced by this re-render. The package it read is \
                 unchanged — nothing was written into it. The executive summary and top \
                 risks are model-authored, so this render words them differently from the \
                 original over the same figures.\n"
            }
        }
    }
}

impl IndexEntry {
    /// One phrase for the timing table's third column.
    fn outcome(&self) -> String {
        match (&self.failure, self.carried_over) {
            (Some(_), _) => "no report".to_owned(),
            (None, true) => "carried over from an earlier run".to_owned(),
            (None, false) => "report written".to_owned(),
        }
    }
}

/// The files directly in `dir`, sorted, or an empty list when it cannot be read.
///
/// Not recursive and never through a symlink: this is a reading aid, and a link
/// out of the directory would point the reader at a file the run did not write.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

/// A Markdown link to `path`, written relative to `base`.
fn link(base: &Path, path: &Path) -> String {
    let target = relative(base, path);
    let text = target.clone();
    // CommonMark needs the angle-bracket form once a destination carries a
    // space or a parenthesis; a repository name reaches the filesystem through
    // `crate::run::stem`'s sanitizer, but a re-render's directory names come
    // from a package this crate did not write.
    if target.contains([' ', '(', ')']) {
        return format!("[{text}](<{target}>)");
    }
    format!("[{text}]({target})")
}

/// `path` expressed relative to `base`, walking up where it has to.
///
/// Both paths are built from the same root by their callers — a work-dir area or
/// a `--out` directory — so the common prefix is real rather than coincidental.
/// A path with no shared prefix at all falls back to its own display form, which
/// is still a correct link and an honest one.
fn relative(base: &Path, path: &Path) -> String {
    let mut base_parts = base.components().peekable();
    let mut path_parts = path.components().peekable();
    while base_parts.peek().is_some() && base_parts.peek() == path_parts.peek() {
        base_parts.next();
        path_parts.next();
    }
    let ups = base_parts.count();
    let rest: PathBuf = path_parts.collect();
    if ups == 0 && rest.as_os_str().is_empty() {
        return path.display().to_string();
    }
    let mut relative = "../".repeat(ups);
    relative.push_str(&rest.to_string_lossy());
    if relative.is_empty() {
        return path.display().to_string();
    }
    relative
}

/// A duration a human reads at a glance rather than converting.
fn human(d: Duration) -> String {
    let millis = d.as_millis();
    if millis < 1_000 {
        return format!("{millis} ms");
    }
    let seconds = d.as_secs();
    if seconds < 60 {
        return format!("{}.{:01}s", seconds, (millis % 1_000) / 100);
    }
    let (hours, minutes, secs) = (seconds / 3_600, (seconds % 3_600) / 60, seconds % 60);
    if hours > 0 {
        return format!("{hours}h {minutes:02}m {secs:02}s");
    }
    format!("{minutes}m {secs:02}s")
}

/// Write the sweep's index into `out/`, from what the sweep recorded.
///
/// Why: the two producers assemble their entries HERE rather than each building
/// an [`IndexReport`] at its own call site, so `crate::run` and
/// `crate::rerender` cannot describe the same fact differently — and so neither
/// file grows the version-lookup and link-building code a second time.
/// What: the pinned tool versions this engagement installed, one entry per
/// repository in selection order, and the sweep's own wall clock. A repository
/// that failed carries its reason and its log; one carried over from an earlier
/// run is marked as such and keeps that run's measured duration.
/// Test: `crate::run::run_tests::a_sweep_writes_an_index_beside_its_reports`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when `out/index.md` cannot be written. The tool
/// record is read fail-open: a missing or unreadable one leaves every version
/// stated as not recorded rather than failing a finished sweep.
pub fn write_sweep(
    work: &crate::workdir::WorkDir,
    report: &crate::run::RunReport,
    total: Duration,
) -> Result<(), AuditError> {
    let recorded = crate::tools::read_record(work).unwrap_or_default();
    let tools = crate::tools::RequiredTool::ALL
        .iter()
        .map(|tool| {
            let name = tool.crate_name();
            match recorded.iter().find(|t| t.crate_name == name) {
                Some(installed) => {
                    ToolVersion::known(name, installed.version.clone(), "recorded at install")
                }
                None => ToolVersion::unknown(
                    name,
                    "not recorded — this working directory has no install record for it",
                ),
            }
        })
        .collect();
    let entries = report
        .repos
        .iter()
        .map(|run| IndexEntry {
            name: run.repo.name.clone(),
            dir: run.output.clone(),
            log: Some(run.log.clone()),
            failure: match &run.result {
                crate::run::RepoResult::Failed { reason } => Some(reason.clone()),
                crate::run::RepoResult::Succeeded => None,
            },
            carried_over: run.resumed,
            duration: run.duration_ms.map(Duration::from_millis),
        })
        .collect();
    let index = IndexReport {
        producer: Producer::Sweep,
        generated_at: local_now(),
        tools,
        entries,
        total: Some(total),
    };
    write(&index, &work.path(crate::workdir::Area::Output))
}

/// Write the re-render's index into its `--out` directory.
///
/// Why: see [`write_sweep`] — one module decides what an index says.
/// What: the `trusty-review` this run actually drove, asked for its own
/// `--version` because a re-render has no install record to read (#6080), plus
/// one entry per manifest it found. `tga` is stated as not recorded rather than
/// omitted: a reader comparing this index against a sweep's should see why one
/// row is empty rather than that a row is missing.
/// Test: `crate::rerender::rerender_tests::a_re_render_writes_an_index_into_its_output`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when `index.md` cannot be written.
pub fn write_render(
    out_dir: &Path,
    review: &Path,
    reports: &[crate::rerender::RenderedReport],
    total: Duration,
) -> Result<(), AuditError> {
    let review_version = match tool_version(review) {
        Some(version) => ToolVersion::known(
            "trusty-review",
            version,
            format!("`{} --version`", review.display()),
        ),
        None => ToolVersion::unknown(
            "trusty-review",
            format!("not recorded — `{}` did not answer", review.display()),
        ),
    };
    let entries = reports
        .iter()
        .map(|rendered| IndexEntry {
            name: rendered.name.clone(),
            dir: rendered.output.clone(),
            log: Some(rendered.log.clone()),
            failure: match &rendered.result {
                crate::rerender::RenderResult::Failed { reason } => Some(reason.clone()),
                crate::rerender::RenderResult::Succeeded => None,
            },
            carried_over: false,
            duration: rendered.duration_ms.map(Duration::from_millis),
        })
        .collect();
    let index = IndexReport {
        producer: Producer::Render,
        generated_at: local_now(),
        tools: vec![
            review_version,
            ToolVersion::unknown("tga", "not recorded — a re-render runs no `tga`"),
        ],
        entries,
        total: Some(total),
    };
    write(&index, out_dir)
}

#[cfg(test)]
mod index_tests {
    use super::*;

    fn entry(name: &str, dir: PathBuf) -> IndexEntry {
        IndexEntry {
            name: name.to_owned(),
            dir,
            log: None,
            failure: None,
            carried_over: false,
            duration: Some(Duration::from_millis(1_500)),
        }
    }

    fn report(producer: Producer, entries: Vec<IndexEntry>) -> IndexReport {
        IndexReport {
            producer,
            generated_at: "2026-08-19 22:40:11 -04:00".to_owned(),
            tools: vec![ToolVersion::known(
                "tga",
                "2.9.4".to_owned(),
                "recorded at install",
            )],
            entries,
            total: Some(Duration::from_secs(3_723)),
        }
    }

    /// 🔴 #6080's requirement in one assertion: a run over ONE repository still
    /// writes an index, and that index states the versions, the timestamp and
    /// the coverage rather than being skipped as redundant.
    #[test]
    fn a_single_unit_run_still_writes_an_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("out");
        let unit = dir.join("00-acme-api");
        std::fs::create_dir_all(&unit).expect("mkdir");
        std::fs::write(unit.join("00-acme-api.md"), "# report\n").expect("write report");

        write(
            &report(Producer::Sweep, vec![entry("acme/api", unit)]),
            &dir,
        )
        .expect("the index is written");

        let text = std::fs::read_to_string(dir.join(INDEX_FILE)).expect("the index is there");
        assert!(text.contains("## Versions"), "{text}");
        assert!(text.contains("2026-08-19 22:40:11 -04:00"), "{text}");
        assert!(text.contains("Reports: 1 of 1 repository"), "{text}");
        assert!(
            text.contains("[00-acme-api/00-acme-api.md](00-acme-api/00-acme-api.md)"),
            "the one report must be linked relatively: {text}"
        );
    }

    /// Every version the run knows is stated, and every one it does not is
    /// stated too — a blank cell would read as "no version" rather than as
    /// "this run could not learn it".
    #[test]
    fn an_unknown_version_is_stated_rather_than_blank() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut index = report(Producer::Render, Vec::new());
        index.tools.push(ToolVersion::unknown(
            "tga",
            "not recorded — a re-render runs no `tga`",
        ));

        let text = render(&index, tmp.path());

        assert!(
            text.contains("| `tga` | 2.9.4 | recorded at install |"),
            "{text}"
        );
        assert!(
            text.contains("| `tga` | not recorded | not recorded — a re-render runs no `tga` |"),
            "{text}"
        );
        assert!(
            text.contains(concat!("| `trusty-audit` | ", env!("CARGO_PKG_VERSION"))),
            "the crate's own version must be stated: {text}"
        );
    }

    /// 🔴 A unit that produced no report is NAMED with its reason and its log,
    /// so a partial run's index says what is missing rather than being shorter
    /// by one section and silent about it.
    #[test]
    fn a_unit_with_no_report_is_named_with_its_reason_and_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("out");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let log = dir.join("01-acme-web.log");
        std::fs::write(&log, "render failed\n").expect("write log");
        let mut failed = entry("acme/web", dir.join("01-acme-web"));
        failed.failure = Some("`trusty-review report` exited with code 3".to_owned());
        failed.log = Some(log);

        let text = render(&report(Producer::Render, vec![failed]), &dir);

        assert!(text.contains("### acme/web"), "{text}");
        assert!(
            text.contains("No report — `trusty-review report` exited with code 3"),
            "{text}"
        );
        assert!(
            text.contains("- log: [01-acme-web.log](01-acme-web.log)"),
            "a failed unit must link its log: {text}"
        );
        assert!(text.contains("| acme/web | 1.5s | no report |"), "{text}");
        assert!(text.contains("Reports: 0 of 1 report"), "{text}");
    }

    /// A repository carried over from an earlier sweep says so, rather than
    /// reading as work this run did in no time at all.
    #[test]
    fn a_carried_over_unit_says_so() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut resumed = entry("acme/api", tmp.path().join("00-acme-api"));
        resumed.carried_over = true;
        resumed.duration = None;

        let text = render(&report(Producer::Sweep, vec![resumed]), tmp.path());

        assert!(
            text.contains("| acme/api | not recorded | carried over from an earlier run |"),
            "{text}"
        );
    }

    /// A log that lives outside the index's own directory — the sweep keeps them
    /// under `logs/`, a sibling of `out/` — is linked by walking up, not by an
    /// absolute path naming the recipient's home directory.
    #[test]
    fn a_sibling_directory_is_linked_by_walking_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("work/out");
        let logs = tmp.path().join("work/logs");
        std::fs::create_dir_all(&out).expect("mkdir out");
        std::fs::create_dir_all(&logs).expect("mkdir logs");
        let log = logs.join("00-acme-api.log");
        std::fs::write(&log, "").expect("write log");
        let mut unit = entry("acme/api", out.join("00-acme-api"));
        unit.log = Some(log);
        unit.failure = Some("`tga audit` exited with code 1".to_owned());

        let text = render(&report(Producer::Sweep, vec![unit]), &out);

        assert!(
            text.contains("- log: [../logs/00-acme-api.log](../logs/00-acme-api.log)"),
            "{text}"
        );
    }

    /// The contents table describes THIS directory. A sweep's index naming a
    /// re-render's layout would send the reader to files that are not there.
    #[test]
    fn the_contents_table_matches_the_producer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sweep = render(&report(Producer::Sweep, Vec::new()), tmp.path());
        let rendered = render(&report(Producer::Render, Vec::new()), tmp.path());

        assert!(sweep.contains("../extract/<NN>-<repo>.db"), "{sweep}");
        assert!(sweep.contains("`tga audit` child"), "{sweep}");
        assert!(!rendered.contains("../extract/"), "{rendered}");
        assert!(
            rendered.contains("The package it read is unchanged"),
            "{rendered}"
        );
    }

    /// A destination carrying a space still parses as one link — a re-render
    /// takes its directory names from a package this crate did not write.
    #[test]
    fn a_link_with_a_space_uses_the_angle_bracket_form() {
        let base = Path::new("/w/out");
        assert_eq!(
            link(base, Path::new("/w/out/acme api/report.md")),
            "[acme api/report.md](<acme api/report.md>)"
        );
        assert_eq!(
            link(base, Path::new("/w/out/acme/report.md")),
            "[acme/report.md](acme/report.md)"
        );
    }

    /// Durations read as durations at every scale, because "7263000 ms" is not
    /// an answer to "how long did this take".
    #[test]
    fn durations_render_at_every_scale() {
        assert_eq!(human(Duration::from_millis(412)), "412 ms");
        assert_eq!(human(Duration::from_millis(1_500)), "1.5s");
        assert_eq!(human(Duration::from_secs(125)), "2m 05s");
        assert_eq!(human(Duration::from_secs(3_723)), "1h 02m 03s");
    }

    #[cfg(unix)]
    fn stub(at: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = at.join(name);
        std::fs::write(&path, script).expect("stub binary");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// The version column carries the version, not `trusty-review 0.16.0` —
    /// the tool column already said which tool it is.
    #[cfg(unix)]
    #[test]
    fn a_version_line_drops_the_repeated_binary_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let binary = stub(
            tmp.path(),
            "trusty-review",
            "#!/bin/sh\necho 'trusty-review 0.16.0'\n",
        );
        assert_eq!(tool_version(&binary).as_deref(), Some("0.16.0"));
    }

    /// A binary that is absent, or that refuses `--version`, yields no version
    /// rather than an error — the index states the gap and the run continues.
    #[cfg(unix)]
    #[test]
    fn a_binary_that_will_not_answer_has_no_version() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(tool_version(&tmp.path().join("no-such-binary")), None);
        let refuses = stub(tmp.path(), "grumpy", "#!/bin/sh\nexit 2\n");
        assert_eq!(tool_version(&refuses), None);
    }
}
