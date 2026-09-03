//! Which files change most, read from the checkout's own git history (#6079).
//!
//! Why: the investigation budget is spent on the files the ranking names, and
//! before this the ranking knew two things — what trusty-analyze measured as
//! complex, and what trusty-search matched for a due-diligence dimension.
//! Neither knows what the team actually works on. A file rewritten forty times
//! in six months is where the defects, the review load and the key-person risk
//! are, and it is invisible to both signals when it happens to be short and
//! unremarkable to a text query. Git already holds that answer in every
//! checkout, for free.
//!
//! What: a `git log --numstat` shell-out per repository, reduced to per-file
//! commit counts, distinct-author counts and line totals; the hotspots band into
//! `[report].findings` under the `churn` category, and the top of the same list
//! becomes an optional lane in the evidence ranking
//! ([`super::evidence::blend_with`]). Absent churn — no git, no history, a
//! shallow clone — reproduces the ranking exactly as it was, which is #6079's
//! optional-input closure condition.
//!
//! **No tga dependency.** The owner ruled 2026-08-19 that trusty-review must run
//! independently of tga, and this collector is the trusty-audit half of that
//! ruling: the history is read here, by this crate, through
//! [`crate::git`]'s hardened child. `crates/trusty-git-analytics` computes its
//! own churn for its own reports and nothing here consumes it.
//!
//! ## The thresholds, in one place
//!
//! Everything numeric this collector decides is a `pub const` in the block
//! below — the window, the commit cap, the three bands, and the two output
//! sizes. They are stated once so the report's own scope line can quote them and
//! so tuning is one edit rather than a search.
//!
//! ## Fail-open, and never silently
//!
//! Five states produce no rows and each says which it was: `git` is not
//! installed, the path holds no git repository, the clone is shallow (its counts
//! would be truncated, and a truncated count understates churn — the false-clean
//! shape epic #6074 exists to remove), the window holds no commit, or the child
//! failed. A sixth produces rows AND a caveat: a window holding more commits
//! than [`MAX_COMMITS`] is read only as far as the cap, so every count it
//! produces understates and the gap line says so. The collector's own diagnosis
//! leads every one of them; the child's first stderr line is only ever the
//! parenthetical (#6720).
//!
//! There is deliberately no not-applicable arm. [`crate::local_repo`] gives
//! every checkout its history by `git clone`, so a path with no `.git` at this
//! leg is an anomaly rather than a leg that does not apply — and reporting it as
//! silence renders a Change Hotspots section that reads exactly like a quiet
//! repository's (#6079).
//!
//! Test: `churn_tests`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use toml_edit::{InlineTable, Value};

use super::cve::Severity;
use super::findings::first_line;
use super::priority::Priority;

/// How this collector names itself in a gap line.
pub const COLLECTOR: &str = "git-churn";
/// The `[report].findings` category these rows carry.
pub const CATEGORY: &str = "churn";
/// The single rule id every row shares; the path is what distinguishes them.
pub const RULE: &str = "churn-hotspot";

// ─── Thresholds ─────────────────────────────────────────────────────────────
// The one place this collector's numbers are stated (#6079).

/// How far back the window reaches, in days.
pub const WINDOW_DAYS: u32 = 180;
/// Most commits read from the window, newest first, whatever it holds.
///
/// A bound on the work rather than a tuning knob: a monorepo with a decade of
/// history would otherwise hand this process a `--numstat` stream measured in
/// hundreds of megabytes, and the ranking it feeds is capped far below that.
///
/// Hitting it is reported, never absorbed: a window with more commits than this
/// is read to the cap and the counts that come out understate every file, so
/// the leg returns [`Outcome::Capped`] and the caller states the cap in a gap
/// line (#6079).
pub const MAX_COMMITS: usize = 4_000;
/// Fewest commits in the window that make a file a hotspot at all.
pub const MIN_COMMITS: u32 = 5;
/// Commits in the window at or above which a file bands RED.
pub const RED_COMMITS: u32 = 20;
/// Most hotspot rows written into `[report].findings`.
pub const MAX_ROWS: usize = 20;
/// Most hotspots offered to the evidence ranking as a lane.
pub const MAX_RANKED: usize = 10;

/// Basenames whose churn measures a generator rather than the code.
///
/// A lockfile changes on every dependency bump and a changelog on every merge,
/// so both outrank every source file in any repository and would take the top of
/// both outputs. Excluded by BASENAME, so a lockfile in a subdirectory is
/// excluded too, and stated as a list rather than a heuristic so the exclusion
/// is inspectable.
pub const GENERATED: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.sum",
    "poetry.lock",
    "Gemfile.lock",
    "composer.lock",
    "CHANGELOG.md",
];

/// Extensions whose churn is configuration or prose rather than logic.
///
/// Why: this bounds the RANKING only — the findings still report them, because
/// a manifest rewritten 200 times in six months is a real dependency-movement
/// signal a due-diligence reader wants. What it must not do is spend an
/// analyst's inference budget: measured against this workspace, the unfiltered
/// top ten was seven `Cargo.toml`s, a `.tsv` allowlist and a `CLAUDE.md`.
/// A deny-list rather than a source-extension allow-list, so a language this
/// table has never heard of still reaches the ranking.
pub const NON_CODE: &[&str] = &[
    "toml", "md", "tsv", "csv", "json", "yaml", "yml", "lock", "txt", "cfg", "ini", "xml", "svg",
];

/// One file's change history over the window.
///
/// Why: the three numbers answer three different due-diligence questions —
/// commits say how unstable the file is, distinct authors say whether it is one
/// person's private territory, and the line totals separate a file rewritten
/// wholesale from one touched a line at a time.
/// What: repo-relative path plus the counts, as [`parse`] reduced them. A binary
/// file contributes commits and zero lines, because `--numstat` reports `-` for
/// both line columns and there is no honest number to put there.
/// Test: `churn_tests::{the_log_reduces_to_per_file_counts,
/// a_binary_file_counts_its_commits_and_no_lines}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ChurnFile {
    /// Repo-relative path, exactly as git spelled it.
    pub path: String,
    /// Commits in the window that touched it.
    pub commits: u32,
    /// Distinct author addresses among those commits.
    pub authors: usize,
    /// Lines added across those commits.
    pub added: u64,
    /// Lines deleted across those commits.
    pub deleted: u64,
}

impl ChurnFile {
    /// The band this file's commit count earns, or `None` below [`MIN_COMMITS`].
    ///
    /// `None` is what keeps the long tail out of both outputs: nearly every file
    /// in a repository is touched once or twice in six months, and reporting
    /// those as findings would bury the twenty that matter.
    ///
    /// Test: `churn_tests::the_bands_are_the_documented_thresholds`.
    #[must_use]
    pub fn severity(&self) -> Option<Severity> {
        if self.commits >= RED_COMMITS {
            Some(Severity::Red)
        } else if self.commits >= MIN_COMMITS {
            Some(Severity::Amber)
        } else {
            None
        }
    }

    /// The row's Summary cell: the counts, and the window they were taken over.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} commits by {} author(s) in the last {WINDOW_DAYS} days, +{}/-{} lines",
            self.commits, self.authors, self.added, self.deleted
        )
    }
}

/// What the leg produced for one repository.
///
/// Why: "no churn" has three meanings that must not share a variant. A measured
/// window whose files all fall below [`MIN_COMMITS`] is a real, quiet
/// repository. A window read only as far as [`MAX_COMMITS`] holds real hotspots
/// whose counts are all understated. A window that could not be read at all is
/// unassessed, and the report must not let the third read as the first.
///
/// Unlike [`super::cve::Outcome`] there is no not-applicable variant: a
/// dependency manifest is something a checkout may honestly not have, whereas
/// its own git history is something [`crate::local_repo`]'s clone always gives
/// it, so an absent one is an anomaly the report names (#6079).
/// What: the ranked hotspots (possibly empty, which IS a quiet repository), the
/// same ranked hotspots marked as read under the cap, or the one line the caller
/// turns into a gap.
/// Test: `churn_tests::{a_shallow_clone_is_a_named_gap,
/// a_repository_with_no_history_is_a_named_gap,
/// a_path_holding_no_repository_is_a_named_gap, a_capped_window_names_the_cap}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The history was read whole; these are its hotspots, worst first.
    Measured(Vec<ChurnFile>),
    /// The read stopped at [`MAX_COMMITS`]; these are the hotspots of the
    /// commits it did read, and every count in them understates.
    Capped(Vec<ChurnFile>),
    /// The history could not be read, or is not complete enough to count; why.
    Unavailable(String),
}

/// One completed `git log` invocation, as this module needs to see it.
///
/// Why a struct rather than `std::process::Output`: the shallow flag comes from
/// a SECOND git invocation, and folding both into one value is what lets every
/// failure arm be driven by a test without a fixture repository per arm.
/// What: the exit disposition, the `--numstat` stream, the child's stderr, and
/// whether the repository is a shallow clone.
/// Test: `churn_tests`, which constructs one per arm.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Run {
    /// Whether `git log` exited zero.
    pub success: bool,
    /// The `--numstat` stream on stdout.
    pub log: String,
    /// The child's stderr, for a gap's parenthetical only (#6720).
    pub stderr: String,
    /// Whether `git rev-parse --is-shallow-repository` said `true`.
    pub shallow: bool,
}

/// Measure `checkout`'s churn with a real `git`.
///
/// # Postconditions
/// Never panics. Reads only: `rev-parse` and `log` write nothing to the
/// repository, and this collector creates no file anywhere.
///
/// Test: `churn_tests::{a_fixture_repository_ranks_its_hotspots,
/// a_non_ascii_path_reaches_the_report_unescaped}`.
#[must_use]
pub fn measure(checkout: &Path) -> Outcome {
    measure_with(checkout, run_git)
}

/// [`measure`] with the git invocation supplied by the caller.
///
/// Why: every arm below is a state a real repository can be in and a test cannot
/// cheaply construct — a shallow clone, a window deeper than [`MAX_COMMITS`], a
/// git that fails mid-stream, a child whose stderr leads with an unrelated
/// notice. The seam is what makes each one a test rather than a comment.
///
/// Test: `churn_tests`, one test per arm.
pub fn measure_with<F>(checkout: &Path, run: F) -> Outcome
where
    F: FnOnce(&Path) -> Result<Run, String>,
{
    // #6079: an absent repository is unassessed, not inapplicable — every
    // checkout reaches this leg through `crate::local_repo`'s clone, so nothing
    // here can be measured and the reader must be told which. `.git` is a
    // directory in a checkout and a FILE in a linked worktree, so `exists` is
    // the test rather than `is_dir`. Both rungs run before any child is spawned.
    if !checkout.is_dir() {
        return Outcome::Unavailable(format!(
            "{COLLECTOR}: {} is not a directory, so there is no checkout to read a history from",
            checkout.display()
        ));
    }
    if !checkout.join(GIT_MARKER).exists() {
        return Outcome::Unavailable(format!(
            "{COLLECTOR}: {} holds no git repository, so it has no change history to read",
            checkout.display()
        ));
    }
    let output = match run(checkout) {
        Ok(output) => output,
        Err(cause) => return Outcome::Unavailable(cause),
    };
    if output.shallow {
        return Outcome::Unavailable(format!(
            "{COLLECTOR}: {} is a shallow clone, so its per-file commit counts would be truncated \
             at the graft point and would understate every hotspot",
            checkout.display()
        ));
    }
    if !output.success && output.log.trim().is_empty() {
        return Outcome::Unavailable(format!(
            "{COLLECTOR}: `{}` exited non-zero and printed no history to read ({})",
            crate::git::BINARY,
            first_line(&output.stderr)
        ));
    }
    let measured = parse(&output.log);
    if measured.is_empty() {
        return Outcome::Unavailable(format!(
            "{COLLECTOR}: no commit in the last {WINDOW_DAYS} days touched a file here, so there \
             is no change history to rank"
        ));
    }
    // #6079: `--max-count` truncates silently — a read that returned the cap is
    // a window this leg did not see the whole of, and every count it produced
    // is a floor rather than a measurement.
    if commits_read(&output.log) >= MAX_COMMITS {
        return Outcome::Capped(hotspots(measured));
    }
    Outcome::Measured(hotspots(measured))
}

/// How many commits the stream holds, as its [`RECORD`] markers count them.
///
/// Read from the stream rather than from the child, because `--max-count` is
/// the only thing that stopped it and git reports no truncation of its own.
fn commits_read(log: &str) -> usize {
    log.lines().filter(|line| line.starts_with(RECORD)).count()
}

/// Reduce a `git log --numstat` stream to per-file counts.
///
/// Why: the stream is per-commit and the report is per-file, and the reduction
/// is where the two decisions that are not git's live — that a generated file is
/// excluded, and that a binary file counts its commits and no lines.
/// What: a line beginning [`RECORD`] opens a commit and names its author; every
/// `added<TAB>deleted<TAB>path` line after it is folded into that path's totals.
/// Anything else is ignored, so git's blank separator lines and any future
/// header cost nothing.
///
/// # Postconditions
/// Never panics on arbitrary input. Output order is the map's — [`hotspots`]
/// imposes the report order.
///
/// Test: `churn_tests::{the_log_reduces_to_per_file_counts,
/// a_generated_file_never_becomes_a_hotspot, junk_is_ignored_rather_than_counted}`.
#[must_use]
pub fn parse(log: &str) -> Vec<ChurnFile> {
    let mut authors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut totals: BTreeMap<String, (u32, u64, u64)> = BTreeMap::new();
    let mut author = String::new();

    for line in log.lines() {
        if let Some(email) = line.strip_prefix(RECORD) {
            author = email.trim().to_owned();
            continue;
        }
        let mut columns = line.split('\t');
        let (Some(added), Some(deleted), Some(path)) =
            (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() || columns.next().is_some() || is_generated(path) {
            continue;
        }
        let (Some(added), Some(deleted)) = (count(added), count(deleted)) else {
            continue;
        };
        let entry = totals.entry(path.to_owned()).or_insert((0, 0, 0));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(added);
        entry.2 = entry.2.saturating_add(deleted);
        authors
            .entry(path.to_owned())
            .or_default()
            .insert(author.clone());
    }

    totals
        .into_iter()
        .map(|(path, (commits, added, deleted))| ChurnFile {
            authors: authors.get(&path).map_or(0, BTreeSet::len),
            path,
            commits,
            added,
            deleted,
        })
        .collect()
}

/// What a checkout holding a git repository has at its root.
const GIT_MARKER: &str = ".git";

/// The record separator `--format=%x01%ae` puts at the head of each commit.
///
/// `\x01` rather than a printable marker: it cannot occur in a path, so a file
/// literally named `commit …` can never be read as a commit header.
const RECORD: &str = "\u{1}";

/// One `--numstat` line column: a number, or `-` for a binary file.
fn count(column: &str) -> Option<u64> {
    let column = column.trim();
    if column == "-" {
        return Some(0);
    }
    column.parse().ok()
}

/// Whether a ranked path is worth an analyst's reading budget.
///
/// An extensionless file (a shell script, a `Makefile`) passes: the deny-list
/// names what is known NOT to be code, and everything else is presumed to be.
fn holds_code(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_none_or(|ext| !NON_CODE.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Whether this path's churn measures a generator rather than the code.
fn is_generated(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| GENERATED.contains(&name))
}

/// The hotspots among measured files, worst first, capped at [`MAX_ROWS`].
///
/// Ordering is total and deterministic — commits descending, then total lines
/// changed descending, then path ascending — so two runs over the same history
/// produce byte-identical rows and the manifest's dedup never sees a reordering
/// as a new finding.
fn hotspots(mut measured: Vec<ChurnFile>) -> Vec<ChurnFile> {
    measured.retain(|file| file.severity().is_some());
    measured.sort_by(|a, b| {
        b.commits
            .cmp(&a.commits)
            .then_with(|| (b.added + b.deleted).cmp(&(a.added + a.deleted)))
            .then_with(|| a.path.cmp(&b.path))
    });
    measured.truncate(MAX_ROWS);
    measured
}

/// Run `git rev-parse` and `git log` against `checkout`.
///
/// # Errors
/// One line naming which step failed, when `git` is absent, when the path is not
/// a repository, or when either child could not be spawned.
fn run_git(checkout: &Path) -> Result<Run, String> {
    let binary = crate::git::resolve()
        .map_err(|cause| format!("{COLLECTOR}: {cause}, so no change history was read"))?;

    // One invocation answers both questions: `rev-parse` fails outside a
    // repository, and prints `true` inside a shallow one. Asking for a second
    // flag alongside it would make the answer positional, and reading the wrong
    // line reports every repository as shallow.
    let probe = crate::git::at(&binary, checkout, &["rev-parse", "--is-shallow-repository"])
        .output()
        .map_err(|e| format!("{COLLECTOR}: `git rev-parse` could not be run ({e})"))?;
    if !probe.status.success() {
        return Err(format!(
            "{COLLECTOR}: {} is not a git repository, so it has no change history",
            checkout.display()
        ));
    }
    let shallow = String::from_utf8_lossy(&probe.stdout).trim() == "true";

    let since = format!("--since={WINDOW_DAYS} days ago");
    let max = format!("--max-count={MAX_COMMITS}");
    // `--no-renames` so a rename is an add and a delete under two real paths
    // rather than git's `{old => new}` composite, which is not a path a reader
    // can open. `--no-merges` so a merge commit does not re-count its own side.
    //
    // #6079: `core.quotepath=false` because git's default C-quotes every
    // non-ASCII byte, so `src/café.rs` reaches the findings and the ranking as
    // `"src/caf\303\251.rs"` — a path no reader can open and no editor can find.
    // The setting is passed per invocation rather than read from the repository,
    // so the report does not depend on the audited checkout's own config.
    let output = crate::git::at(
        &binary,
        checkout,
        &[
            "-c",
            "core.quotepath=false",
            "log",
            "--no-merges",
            "--no-renames",
            "--numstat",
            "--format=%x01%ae",
            &since,
            &max,
        ],
    )
    .output()
    .map_err(|e| format!("{COLLECTOR}: `git log` could not be run ({e})"))?;

    Ok(Run {
        success: output.status.success(),
        log: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        shallow,
    })
}

/// Measure `checkout`, and say in one line what could not be measured.
///
/// Why: the two outputs go to different readers. The hotspots feed the evidence
/// ranking and the manifest's findings; the gap lines feed `[report].gaps`, and
/// they are the whole reason this leg is fail-open rather than fatal — a sweep
/// over an org cannot spend its one shot on a repository whose history is
/// unreadable.
/// What: the hotspots, plus zero or one gap line naming `display`, the
/// collector, and what the report will therefore not carry.
///
/// # Postconditions
/// Never panics and never errors. An empty hotspot list always comes with either
/// a gap line or the quiet-repository scope line, never with silence.
///
/// Test: `churn_tests::{a_quiet_repository_states_its_scope,
/// a_shallow_clone_is_a_named_gap, a_path_holding_no_repository_is_a_named_gap,
/// a_capped_window_names_the_cap}`.
#[must_use]
pub fn ground(checkout: &Path, display: &str) -> (Vec<ChurnFile>, Vec<String>) {
    ground_with(checkout, display, run_git)
}

/// [`ground`] with the git invocation supplied by the caller.
///
/// Test: `churn_tests`, one test per arm.
pub fn ground_with<F>(checkout: &Path, display: &str, run: F) -> (Vec<ChurnFile>, Vec<String>)
where
    F: FnOnce(&Path) -> Result<Run, String>,
{
    match measure_with(checkout, run) {
        Outcome::Unavailable(cause) => (
            Vec::new(),
            vec![format!(
                "{display}: {cause} — the report names no change hotspot for it, which must be \
                 read as unmeasured rather than as a codebase nobody is rewriting, and its \
                 investigation pass ranks files without a churn signal"
            )],
        ),
        Outcome::Measured(files) if files.is_empty() => (
            Vec::new(),
            vec![format!(
                "{display}: {COLLECTOR}: no file was touched by {MIN_COMMITS} or more commits in \
                 the last {WINDOW_DAYS} days, so it has no change hotspot to report. Work older \
                 than that window, and history rewritten by a squash or a filter, are not covered \
                 by that count"
            )],
        ),
        // #6079: rows AND a caveat — the ranking is still worth having, and a
        // reader who is not told the cap reads a floor as a measurement.
        Outcome::Capped(files) => (
            files,
            vec![format!(
                "{display}: {COLLECTOR}: the window holds more commits than the {MAX_COMMITS} this \
                 leg reads, so every count below was taken from the newest {MAX_COMMITS} alone and \
                 understates its file; a file whose work is entirely older than that read is \
                 missing from the hotspots altogether"
            )],
        ),
        Outcome::Measured(files) => (files, Vec::new()),
    }
}

/// Write the hotspot rows into the manifest, or say why they are not there.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19). Rows this
/// process holds and does not write reach no renderer — not the sweep's, and not
/// the recipient's own re-render of the delivered package.
/// What: one row per hotspot through [`super::findings::append`], the shared
/// writer, under [`CATEGORY`]. Identity is `id` plus `package`, so a resumed
/// sweep restates nothing; the counts in `title` are deliberately NOT part of
/// identity, because a re-measured file with one more commit is the same finding
/// rather than a second one.
///
/// # Postconditions
/// Never panics. An empty `files` writes nothing and returns no gap — the caller
/// has already stated that case's scope line through [`ground`].
///
/// Test: `churn_tests::{the_hotspots_land_in_the_manifest,
/// a_resumed_sweep_does_not_restate_a_hotspot,
/// a_manifest_that_cannot_be_written_is_a_named_gap}`.
#[must_use]
pub fn write_into(manifest: &Path, files: &[ChurnFile], display: &str) -> Vec<String> {
    let rows: Vec<InlineTable> = files.iter().map(row).collect();
    match super::findings::append(manifest, &rows, IDENTITY) {
        Ok(()) => Vec::new(),
        Err(cause) => vec![format!(
            "{display}: {COLLECTOR}: {cause} — the report states none of the {} change hotspot(s) \
             its history holds",
            files.len()
        )],
    }
}

/// What makes two `[report].findings` rows the same churn hotspot.
const IDENTITY: &[&str] = &["id", "package"];

/// One hotspot as a `[report].findings` row.
fn row(file: &ChurnFile) -> InlineTable {
    let mut table = InlineTable::new();
    table.insert("category", Value::from(CATEGORY));
    table.insert("id", Value::from(RULE));
    table.insert("package", Value::from(file.path.as_str()));
    table.insert("version", Value::from(""));
    table.insert(
        "severity",
        Value::from(file.severity().map_or("AMBER", Severity::as_str)),
    );
    table.insert("title", Value::from(file.summary()));
    table
}

/// The hotspots as an evidence-ranking lane (#6079's optional `select_files` input).
///
/// Why: churn is a selection signal, not only a reported number. A file the team
/// rewrites weekly is worth an analyst's attention whether or not it is complex
/// and whether or not a dimension query matched it.
/// What: the top [`MAX_RANKED`] hotspots that hold code ([`NON_CODE`]), as bare
/// [`Priority`] rows carrying no dimension — churn is evidence for no single
/// due-diligence dimension, and claiming one would let the report count a
/// dimension as investigated on the strength of a commit count. The reason line
/// names the measurement.
///
/// # Postconditions
/// Empty in, empty out — which is what makes the ranking with no churn identical
/// to the ranking before this leg existed.
///
/// Test: `churn_tests::{the_lane_is_the_top_hotspots_with_no_dimension,
/// the_lane_declines_a_manifest_the_findings_still_report}`,
/// `super::evidence::evidence_tests::an_absent_churn_lane_reproduces_the_previous_ranking`.
#[must_use]
pub fn lane(files: &[ChurnFile]) -> Vec<Priority> {
    files
        .iter()
        .filter(|file| holds_code(&file.path))
        .take(MAX_RANKED)
        .map(|file| Priority {
            path: file.path.clone(),
            dimension: None,
            reason: Some(format!("{COLLECTOR}: {}", file.summary())),
            hotspot: None,
        })
        .collect()
}

#[cfg(test)]
#[path = "churn_tests.rs"]
mod churn_tests;
