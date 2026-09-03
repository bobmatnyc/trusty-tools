//! Re-rendering a delivered audit from the artifacts that shipped with it (#6080).
//!
//! Why: the deliverable leaves this crate as one zip — a directory per audited
//! repository under `reports/`, each holding the `manifest.toml` `tga audit`
//! wrote and the artifacts it collected beside it, plus the extract database
//! under `extract/`. Everything downstream of that manifest is a render, and
//! whoever holds the zip could not run it: `tga audit` collects from git
//! checkouts the package deliberately does not carry (DOC-68 §13), so the only
//! way to regenerate the report was to re-run the whole sweep on the machine
//! that produced it. This runs the render step alone, against the manifests the
//! package already contains.
//!
//! What: [`rerender`] finds every `manifest.toml` under a delivered package (or
//! under a working directory) and runs one `trusty-review report` child per
//! manifest into a fresh output directory. It reads the package and never
//! writes into it, and it clones and collects nothing. It DOES index and
//! measure any checkout the manifest names that is present on this machine
//! (#6081), because the child it spawns asks for `--analyze`.
//!
//! ## What a re-render reproduces, and what it does not
//!
//! DOC-68 §12 draws the line and this module does not move it. The manifest is
//! the interface: the metrics `tga audit` derived are already in it and in the
//! artifacts beside it, so the scorecards, findings and appendices rebuild from
//! what shipped. Two things do not come back byte for byte:
//!
//! - The executive summary and top risks are LLM-authored, so a second render
//!   words them differently over the same facts.
//! - The dimensions that need the repository itself — the code scan and the
//!   `trusty-analyze` pass — need a checkout at the path the manifest names and
//!   a local analyze daemon. A package carries neither. [`checkout_gaps`] states
//!   the missing checkout and [`ground_present_checkouts`] states a daemon that
//!   will not start, rather than letting a thinner report read as the same one.
//!
//! ## Versions
//!
//! Whatever `trusty-review` this resolves is the one that renders — there is no
//! pin check here and no version machinery (owner ruling on #6080: current
//! versions). That is the deliberate difference from `crate::run`, which
//! refuses anything off the engagement's pin because a sweep's collected data
//! has to match the tool that reads it.
//!
//! Test: `super::rerender_tests`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::config::SecretKey;
use crate::error::AuditError;
use crate::grounding;
use crate::manifest::AuditManifest;
use crate::progress::{Operation, Progress, UnitOutcome};
use crate::run::ENV_INFERENCE_CREDENTIAL;
use crate::tools::RequiredTool;
use crate::workdir::WorkDir;

/// Directory the regenerated reports land in when `--out` is not given.
///
/// Relative to the source, so a recipient who unzipped the package into one
/// directory gets everything this run produced in one named subdirectory of it.
pub const DEFAULT_OUTPUT_DIR: &str = "rerendered";

/// The directories a package or a working directory keeps its reports under.
///
/// `reports/` is the delivered package's layout (`crate::package`), `out/` is
/// the working directory's ([`crate::workdir::Area::Output`]), and the source
/// itself covers an operator who pointed at either of those directly.
const REPORT_PARENTS: [&str; 2] = ["reports", "out"];

/// Where the re-render reads from, where it writes, and which renderer it runs.
///
/// Why: three operator choices with defaults, in one struct rather than three
/// positional arguments — the same shape [`crate::run::RunOptions`] takes, and
/// for the same reason.
/// What: all three default. [`RerenderOptions::default`] re-renders the
/// directory the front end was run in, else the working directory, with the
/// resolved `trusty-review` — see [`resolve_source`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RerenderOptions {
    /// The unzipped package, or a working directory. See [`resolve_source`].
    pub from: Option<PathBuf>,
    /// Where the regenerated reports go. Defaults to [`DEFAULT_OUTPUT_DIR`]
    /// inside the source, which is the one place this writes.
    pub out: Option<PathBuf>,
    /// The `trusty-review` binary to run, when it is not the resolved one.
    pub review: Option<PathBuf>,
}

/// What happened to one manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderResult {
    /// `trusty-review report` exited 0.
    Succeeded,
    /// It did not, or the manifest could not be read at all.
    Failed {
        /// One line naming what went wrong, with any credential already removed.
        reason: String,
    },
}

impl RenderResult {
    /// Whether this report was regenerated.
    pub fn succeeded(&self) -> bool {
        matches!(self, RenderResult::Succeeded)
    }
}

/// One manifest's re-render.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderedReport {
    /// The manifest that was rendered.
    pub manifest: PathBuf,
    /// The name this report is filed under, taken from its directory.
    pub name: String,
    /// Directory holding what this render wrote.
    pub output: PathBuf,
    /// File holding the child's combined stdout and stderr.
    pub log: PathBuf,
    /// The files the render left in [`RenderedReport::output`], sorted.
    pub artifacts: Vec<PathBuf>,
    /// Dimensions this render could not reproduce — see [`checkout_gaps`].
    pub gaps: Vec<String>,
    /// Wall clock around this report, in milliseconds (#6080).
    ///
    /// Covers the whole unit — reading the manifest, indexing and measuring any
    /// checkout present on this machine, and the `trusty-review report` child —
    /// because that is what the operator waited for. `None` only where nothing
    /// measured it, which the index states rather than rendering as zero.
    pub duration_ms: Option<u64>,
    /// How it ended.
    pub result: RenderResult,
}

/// Where the `trusty-review` a re-render drove came from (#6080).
///
/// Why: two of these three are a choice somebody made, and the third is
/// whatever the machine happened to have. A recipient who ran the engagement
/// has its pinned copy under `tools/`; when the work dir does not resolve, the
/// fall-through silently picked an unrelated `trusty-review` off `PATH` — two
/// versions behind the engagement's pin in the live case — and nothing said so
/// until the index's versions table, after the render had finished. The
/// distinction exists so [`crate::cli::render`] can state it at the top of the
/// run instead.
/// What: the three branches of [`resolve_review`], in its resolution order.
/// Test: `super::rerender_tests::the_installed_copy_is_preferred_over_the_path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReviewSource {
    /// `--review-bin` named it.
    Explicit,
    /// The engagement installed it, under a working directory's `tools/` — the
    /// one this run discovered, else the session's own.
    Engagement,
    /// Nothing named it — whatever `PATH` resolves is what ran.
    Path,
}

impl ReviewSource {
    /// Whether this renderer was chosen by the machine rather than by anyone.
    pub fn is_unchosen(self) -> bool {
        matches!(self, ReviewSource::Path)
    }
}

/// The result of one re-render.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RerenderReport {
    /// The package or working directory the manifests were read from.
    pub source: PathBuf,
    /// The directory every regenerated report was written under.
    pub output: PathBuf,
    /// The `trusty-review` this run drove.
    pub review: PathBuf,
    /// How that renderer was chosen (#6080).
    pub review_source: ReviewSource,
    /// What `trusty-review --version` answered, or `None` when it would not.
    pub review_version: Option<String>,
    /// One entry per manifest found, in discovery order.
    pub reports: Vec<RenderedReport>,
}

impl RerenderReport {
    /// The reports that did not regenerate.
    pub fn failures(&self) -> impl Iterator<Item = &RenderedReport> {
        self.reports.iter().filter(|r| !r.result.succeeded())
    }
}

/// Re-render every report a package or working directory carries.
///
/// Why: #6080 — the one capability the recipient of a finished audit has and
/// could not run. It takes the credential and the model selection as arguments
/// rather than reading them, for the reason `crate::inference` documents: the
/// library never reaches for the environment, so this path is provable without
/// mutating it.
///
/// # Preconditions
/// `key` is non-blank ([`crate::session::Session`] refuses a blank one before
/// calling), and `inference` is either empty or names all four
/// `TRUSTY_REVIEW_*` variables — never a subset.
///
/// # Postconditions
/// On `Ok`, every manifest found has one entry in [`RerenderReport::reports`],
/// each with its child's combined output at its `log` path. Nothing under the
/// source is written except the output directory, which is created here.
///
/// What: resolves the source ([`resolve_source`] — `cwd` is the directory the
/// front end was run in), discovers the manifests, resolves the renderer, then
/// one child per
/// manifest. A child that fails is RECORDED and the run continues — the same
/// failed-but-continuing model the sweep uses (DOC-67 §9), because one report
/// that will not render is not a reason to withhold the four that did.
/// Test: `super::rerender_tests`.
///
/// # Errors
///
/// [`AuditError::NothingToRerender`] when the source carries no manifest,
/// [`AuditError::ReviewBinaryMissing`] when the renderer cannot be started at
/// all (it would fail identically for every manifest), and
/// [`AuditError::WorkDir`] when the output directory or a log cannot be written.
#[allow(clippy::too_many_arguments)]
pub async fn rerender(
    work: &WorkDir,
    cwd: &Path,
    config: &Path,
    key: &SecretKey,
    inference: &crate::inference::Inference,
    report_settings: &crate::config::ReportSettings,
    options: &RerenderOptions,
    pinned_review: Option<&str>,
    progress: &Progress,
) -> Result<RerenderReport, AuditError> {
    let choice = resolve_source(options.from.clone(), cwd, config, work.root());
    // The refusal names every location this run considered, not just the last
    // one it settled on — see [`AuditError::NothingToRerender`].
    let Ok(manifests) = discover(&choice.source) else {
        return Err(AuditError::NothingToRerender {
            searched: choice.searched,
            file: AuditManifest::FILE_NAME,
        });
    };
    let source = choice.source;
    let output = options
        .out
        .clone()
        .unwrap_or_else(|| source.join(DEFAULT_OUTPUT_DIR));
    // #6080: the renderer is resolved against the work dir this run DISCOVERED,
    // not the one the session defaulted to before discovery ran.
    let (review, review_source) = resolve_review(work, &source, options.review.as_deref());
    // #6080: asked once, up front, so the version this run will render at is
    // known before the first child rather than only after the last one. The
    // index reads the same answer rather than spawning a second `--version`.
    let review_version = crate::index_report::tool_version(&review);
    // #6173: reconcile the engagement's own copy against the engagement's own
    // pin, which nothing on this path did.
    reconcile_review_pin(review_source, review_version.as_deref(), pinned_review)?;
    // #6081: this is the tga-free render path, and nothing on it started a
    // daemon or indexed anything — so `--analyze` fetched nothing and every
    // analyze-derived section rendered empty over a clean exit. Resolved once,
    // because the two addresses come from discovery files and the environment
    // and re-reading them per manifest could disagree with itself mid-run.
    let tools = grounding::Tools::resolved(work);
    mkdir(&output)?;

    // #6669: one env set per run, resolved once for the same reason the
    // inference selection is — the engagement's report selection is a property
    // of the engagement, not of a manifest, so every child gets the same pairs.
    // The report keys sit AFTER the inference pairs and name different
    // variables, so neither can shadow the other.
    let mut child_env = inference.env.clone();
    child_env.extend(report_settings.child_env());

    // #6080: this run's own wall clock, for the index written at the end.
    let started = std::time::Instant::now();
    let total = manifests.len();
    progress.operation_started(Operation::Rerender, total);
    let mut reports = Vec::with_capacity(total);
    let mut taken = HashSet::new();
    // #6135: what the delivered manifests declare is what the children will
    // resolve from — it outranks the pairs this run injects — so the index
    // states that, falling back to this run's own selection when no manifest
    // declares one.
    let mut declared: Option<crate::index_report::InferenceRecord> = None;
    for (index, manifest) in manifests.into_iter().enumerate() {
        if declared.is_none()
            && let Ok(Some(read)) = AuditManifest::load_if_present(&manifest)
        {
            declared = read
                .inference
                .as_ref()
                .map(crate::index_report::InferenceRecord::declared);
        }
        let name = unique_name(&manifest, index, &mut taken);
        progress.unit_started(Operation::Rerender, name.as_str(), index + 1, total);
        let unit_started = std::time::Instant::now();
        let mut rendered =
            render_one(&review, &tools, &manifest, &output, name, key, &child_env).await?;
        // Timed here rather than inside `render_one`, which returns from two
        // places — a manifest that will not parse is still a unit that took time.
        rendered.duration_ms = Some(millis(unit_started.elapsed()));
        progress.unit_finished(
            Operation::Rerender,
            rendered.name.as_str(),
            match &rendered.result {
                RenderResult::Succeeded => UnitOutcome::Succeeded,
                RenderResult::Failed { reason } => UnitOutcome::Failed(reason.clone()),
            },
        );
        reports.push(rendered);
    }
    progress.operation_finished(
        Operation::Rerender,
        reports.iter().filter(|r| r.result.succeeded()).count(),
        total,
    );
    // #6080: the index goes in the OUTPUT directory and nowhere else, which is
    // what keeps this module's postcondition — the source package is read, never
    // written — true of the index as much as of every report.
    crate::index_report::write_render(
        &output,
        &review,
        review_version.as_deref(),
        declared.or_else(|| {
            inference
                .selection
                .as_ref()
                .map(crate::index_report::InferenceRecord::of)
        }),
        &reports,
        started.elapsed(),
    )?;

    Ok(RerenderReport {
        source,
        output,
        review,
        review_source,
        review_version,
        reports,
    })
}

/// The directory a re-render reads, and every directory it considered.
///
/// Why: the choice and the refusal need the same list. Settling on one location
/// and reporting only that one is what made the #6080 failure unreadable —
/// `no manifest.toml under /Users/masa/dogfood-audit` named a directory the
/// operator was standing in and could see, and said nothing about the two other
/// places the run had already looked.
/// What: the directory that will be read, plus the ordered list that produced
/// it. With `--from` the list is that one path, because nothing else was tried.
/// Test: `super::rerender_tests::a_refusal_names_every_location_it_tried`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceChoice {
    /// The directory this run will read.
    pub source: PathBuf,
    /// Every directory considered, in the order they were tried.
    pub searched: Vec<PathBuf>,
}

/// The directory a re-render reads when `--from` was not given.
///
/// Why: #6080 — the recipient unzips the delivered package, changes into it,
/// and types `trusty-audit render`. Defaulting to the working directory sent
/// that invocation to `~/.trusty-tools/trusty-audit/work`, which on a
/// recipient's machine holds nothing, so the one command they were given
/// refused while they were standing in the package it should have rendered.
///
/// The engagement-implied rung is the second half of that, and it is the one
/// the live dogfood run hit: an engagement whose sweeps all ran with
/// `--work-dir <config-dir>/work` has its reports neither in the directory the
/// config sits in nor in the TOOL's default work root, so a zero-argument
/// `render` from beside `engagement.toml` refused with the reports one
/// directory down. Nothing in the config schema declares a work dir, and
/// nothing needs to: `<config-dir>/work` is the layout the flow produces, so
/// the convention is read rather than a field added — one that every existing
/// `engagement.toml` in the field would be missing anyway. An engagement whose
/// work dir is somewhere else still names it with `--from` or `--work-dir`.
///
/// What: `explicit` wins outright; otherwise the first of these that a
/// re-render would find something in — `cwd`, then
/// `<config's directory>/work` when `config` is a file that is really there,
/// then `work_root`. None of them carrying a report leaves `cwd`, so a refusal
/// leads with the directory the operator ran in rather than one they never
/// chose. The test is [`discover`] itself, so the default and the search this
/// run then performs cannot disagree.
/// Test: `super::rerender_tests::an_unzipped_package_is_rendered_from_the_cwd`,
/// `super::rerender_tests::a_config_directorys_own_work_dir_is_rendered`,
/// `super::rerender_tests::a_cwd_with_no_reports_falls_back_to_the_work_dir`,
/// `super::rerender_tests::an_explicit_source_beats_every_default`.
pub fn resolve_source(
    explicit: Option<PathBuf>,
    cwd: &Path,
    config: &Path,
    work_root: &Path,
) -> SourceChoice {
    if let Some(path) = explicit {
        return SourceChoice {
            searched: vec![path.clone()],
            source: path,
        };
    }
    let mut searched = vec![cwd.to_path_buf()];
    searched.extend(engagement_work_dir(config));
    searched.push(work_root.to_path_buf());
    searched.dedup();
    let source = searched
        .iter()
        .find(|dir| carries_reports(dir))
        .cloned()
        .unwrap_or_else(|| cwd.to_path_buf());
    SourceChoice { source, searched }
}

/// The work root the engagement config at `config` implies, when it is there.
///
/// `None` when no config is at that path: an absent config implies nothing, and
/// adopting a bare `work/` beside an unrelated directory would render whatever
/// happened to be sitting there. See [`resolve_source`] for why the convention
/// is read rather than declared.
fn engagement_work_dir(config: &Path) -> Option<PathBuf> {
    config
        .is_file()
        .then(|| config.parent().unwrap_or(Path::new(".")))
        .map(|dir| dir.join(crate::workdir::WORK_SUBDIR))
}

/// Whether a re-render pointed at `dir` would find a manifest to render.
fn carries_reports(dir: &Path) -> bool {
    discover(dir).is_ok()
}

/// Every `manifest.toml` under `source`, in a stable order.
///
/// Why: the recipient should not have to know which layout they have. A
/// delivered package keeps its reports under `reports/`, a working directory
/// under `out/`, and an operator who unzipped one report on its own has the
/// manifest right there — so all three are looked for, one level deep in each.
/// It deliberately does not recurse: a deep walk would find the manifests a
/// previous re-render wrote into its own output directory and render them
/// again.
/// What: the source itself and its `reports/` and `out/` children, each checked
/// for a manifest directly and then for one in each subdirectory. Symlinked
/// entries are skipped, the same posture `crate::package` takes about reading
/// outside the tree it was pointed at.
/// Test: `super::rerender_tests::a_delivered_package_layout_is_rendered`,
/// `super::rerender_tests::a_working_directory_layout_is_rendered`,
/// `super::rerender_tests::a_source_with_no_manifest_is_refused`.
///
/// # Errors
///
/// [`AuditError::NothingToRerender`] when no manifest is found anywhere.
fn discover(source: &Path) -> Result<Vec<PathBuf>, AuditError> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let parents = std::iter::once(source.to_path_buf())
        .chain(REPORT_PARENTS.iter().map(|dir| source.join(dir)));
    for parent in parents {
        push_manifest(&parent, &mut found, &mut seen);
        let Ok(entries) = std::fs::read_dir(&parent) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        children.sort();
        for child in children {
            push_manifest(&child, &mut found, &mut seen);
        }
    }
    if found.is_empty() {
        // This one directory is all THIS function looked in. A `--from`-less
        // run tried more, and [`rerender`] replaces the list with what it tried.
        return Err(AuditError::NothingToRerender {
            searched: vec![source.to_path_buf()],
            file: AuditManifest::FILE_NAME,
        });
    }
    Ok(found)
}

/// Record `dir`'s manifest when it has one and has not been recorded already.
fn push_manifest(dir: &Path, found: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let manifest = dir.join(AuditManifest::FILE_NAME);
    if manifest.is_file() && seen.insert(manifest.clone()) {
        found.push(manifest);
    }
}

/// The name one manifest's output is filed under, unique within this run.
///
/// The manifest's directory name is what an operator recognises — `01-acme-api`
/// is the same stem the sweep and the package used. Two layouts can carry the
/// same stem (a package unzipped inside a working directory), so a repeat is
/// prefixed with its index rather than allowed to overwrite the first.
fn unique_name(manifest: &Path, index: usize, taken: &mut HashSet<String>) -> String {
    let stem = manifest
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && n != "." && n != "..")
        .unwrap_or_else(|| "report".to_owned());
    if taken.insert(stem.clone()) {
        return stem;
    }
    let unique = format!("{index:02}-{stem}");
    taken.insert(unique.clone());
    unique
}

/// The `trusty-review` this run drives, and which of the three sources gave it.
///
/// Why: three sources, cheapest first, and no version check on any of them
/// (#6080 owner ruling — the verb runs at whatever version the binary is). The
/// working directory's copy comes before `PATH` so a recipient who ran the
/// engagement re-renders with the binary that produced the reports, without
/// having to say so.
///
/// `discovered` is the directory [`resolve_source`] settled on, and passing it
/// is what keeps the two resolutions from disagreeing. They did: this rung used
/// to ask only `session`, the root the front end resolved BEFORE any discovery
/// ran, so a zero-argument `render` beside an `engagement.toml` found its
/// reports under `<config dir>/work` and then looked for the renderer under the
/// tool's default work root — which on that machine held nothing. The
/// engagement's own `tools/trusty-review` sat unused while a `PATH` copy two
/// minor versions behind rendered the reports (#6080).
///
/// #6080: the `PATH` branch also reports itself, because no version check means
/// a stale binary is not an error and the operator is the only gate left. The
/// path is RESOLVED there rather than left as a bare name, through
/// `trusty_common::bin_resolve` — the workspace's one entry point for this —
/// so the disclosure names the file that will actually run instead of the word
/// the OS was asked to look up. An unresolvable name stays bare, which keeps
/// [`AuditError::ReviewBinaryMissing`] naming what was looked for.
/// What: the explicit path, else `tools/trusty-review` under the discovered
/// directory and then under the session's own root, else whatever `PATH`
/// resolves. The session root stays as a second candidate because a recipient
/// standing in an unzipped package still has the engagement they installed, and
/// its pinned copy beats an unrelated one off `PATH`. The two candidates are the
/// same directory whenever discovery fell through to the session root, and
/// deduplicating is not worth a stat that already answers `false`.
/// Test: `super::rerender_tests::the_installed_copy_is_preferred_over_the_path`,
/// `super::rerender_tests::the_discovered_work_dirs_tools_copy_renders`,
/// `super::rerender_tests::a_path_resolved_renderer_reports_itself_as_unchosen`.
fn resolve_review(
    session: &WorkDir,
    discovered: &Path,
    explicit: Option<&Path>,
) -> (PathBuf, ReviewSource) {
    if let Some(path) = explicit {
        return (path.to_path_buf(), ReviewSource::Explicit);
    }
    let discovered = WorkDir::new(discovered);
    for candidate in [&discovered, session] {
        let installed = RequiredTool::TrustyReview.path_in(candidate);
        if installed.is_file() {
            return (installed, ReviewSource::Engagement);
        }
    }
    let name = RequiredTool::TrustyReview.binary_name();
    let resolved =
        trusty_common::bin_resolve::resolve_binary(name).unwrap_or_else(|| PathBuf::from(name));
    (resolved, ReviewSource::Path)
}

/// Refuse an engagement-local renderer that is not at the engagement's pin.
///
/// Why: #6173. `install`, `run`, and `guided` all reconcile pins through
/// [`crate::run::pins::pinned_binaries`], which refuses on a mismatch. `render`
/// never called it, and its default resolves the engagement's own
/// `tools/trusty-review`. So bumping the pin 0.23.0 -> 0.23.1 and re-rendering
/// executed the 0.23.0 copy and exited 0; the only trace was the version in the
/// report metadata, read after the report had been believed. `describe_renderer`
/// (#6080) discloses a version only for a `PATH`-resolved renderer, which is a
/// different branch and stays silent here.
///
/// `pinned_binaries` itself is the wrong instrument for this path: it demands
/// all four tools be installed and verified, and a recipient re-rendering a
/// delivered package has only `trusty-review`. This checks the one binary
/// `render` actually drives.
///
/// What: refuses only on a KNOWN difference against a KNOWN pin, and only for
/// [`ReviewSource::Engagement`]. `Explicit` is the operator naming a copy on
/// purpose — the documented escape hatch, and refusing it would leave no way
/// out. `Path` is nobody's choice and already discloses itself. No pin (a
/// package re-rendered where no `engagement.toml` loads) and no version (a
/// binary that will not answer `--version`) are both absences, not differences.
/// Test: `super::rerender_tests::a_stale_engagement_copy_is_refused_against_the_pin`,
/// `super::rerender_tests::an_engagement_copy_at_the_pin_renders`,
/// `super::rerender_tests::an_explicit_renderer_is_never_reconciled`.
fn reconcile_review_pin(
    source: ReviewSource,
    version: Option<&str>,
    pinned: Option<&str>,
) -> Result<(), AuditError> {
    if source != ReviewSource::Engagement {
        return Ok(());
    }
    let (Some(version), Some(pinned)) = (version, pinned) else {
        return Ok(());
    };
    if version == pinned {
        return Ok(());
    }
    Err(AuditError::VersionMismatch {
        tool: RequiredTool::TrustyReview.crate_name(),
        pinned: pinned.to_owned(),
        installed: version.to_owned(),
    })
}

/// Render one manifest, recording rather than propagating its failure.
#[allow(clippy::too_many_arguments)]
async fn render_one(
    review: &Path,
    tools: &grounding::Tools,
    manifest: &Path,
    output_root: &Path,
    name: String,
    key: &SecretKey,
    inference: &[(&'static str, String)],
) -> Result<RenderedReport, AuditError> {
    let output = output_root.join(&name);
    let log = output_root.join(format!("{name}.log"));
    let mut report = RenderedReport {
        manifest: manifest.to_path_buf(),
        name,
        output,
        log,
        artifacts: Vec::new(),
        gaps: Vec::new(),
        duration_ms: None,
        result: RenderResult::Succeeded,
    };

    // A manifest that will not parse fails THIS report and no other: the file
    // is the interface, and one unreadable copy says nothing about the rest.
    match AuditManifest::load(manifest) {
        Ok(read) => {
            report.gaps = checkout_gaps(manifest, &read);
            report
                .gaps
                .extend(ground_present_checkouts(tools, manifest, &read).await);
        }
        Err(refusal) => {
            report.result = RenderResult::Failed {
                reason: refusal.to_string(),
            };
            return Ok(report);
        }
    }

    mkdir(&report.output)?;
    report.result = spawn_review(
        review,
        manifest,
        &report.output,
        &report.log,
        key,
        inference,
    )?;
    if report.result.succeeded() {
        report.artifacts = written_files(&report.output);
    }
    Ok(report)
}

/// Index and measure every repository the manifest names that is on this
/// machine (#6081).
///
/// Why: `spawn_review` asks for `--analyze`, and until this existed nothing on
/// this path started trusty-search or trusty-analyze or indexed anything — so
/// the fetch missed, fell open, and the findings table, the complexity
/// distribution and the health factors all rendered empty over exit 0. Empty
/// reads as a clean bill of health.
/// What: the repositories whose checkout IS present, one grounding each. The
/// absent ones are skipped in silence because [`checkout_gaps`] has already
/// stated them, and starting a daemon to measure a directory that is not there
/// would buy nothing. The ranking each grounding produces is DISCARDED here: the
/// manifest is the interface, the sweep already wrote it (`crate::run`), and
/// this module's postcondition is that nothing under the source is written.
/// Test: `super::rerender_tests::a_render_whose_daemons_are_down_says_so`,
/// `super::rerender_tests::grounding_writes_nothing_into_the_package`.
async fn ground_present_checkouts(
    tools: &grounding::Tools,
    manifest: &Path,
    read: &AuditManifest,
) -> Vec<String> {
    let base = manifest.parent().unwrap_or(Path::new("."));
    // #6082: the budget this package was produced under, which the sweep wrote
    // into `[report]`. It sizes the evidence caps even though the ranking is
    // discarded, so the queries this pass runs match the ones that produced it.
    let budget = grounding::priority::Budget::for_manifest(manifest);
    let mut gaps = Vec::new();
    for repo in &read.repositories {
        let path = if repo.path.is_absolute() {
            repo.path.clone()
        } else {
            base.join(&repo.path)
        };
        if !path.is_dir() {
            continue;
        }
        // #6082: no brief here on purpose — the ranking this produces is
        // discarded (the manifest already carries the sweep's), so deriving
        // extra queries from the brief would spend a request set for nothing.
        gaps.extend(
            grounding::ground(tools, &path, &repo.name, None, budget)
                .await
                .gaps,
        );
    }
    gaps
}

/// The dimensions this render cannot reproduce, one line each.
///
/// Why: DOC-68 §12's recomputability boundary, stated where it is actionable
/// rather than only in a spec. A package carries reports and databases, never
/// `repos/`, so the checkouts the manifest names are absent on any machine but
/// the one that ran the sweep — and `trusty-review`'s scan of a missing
/// directory produces a THINNER report, not a failing one. Unstated, that reads
/// as the same report with fewer findings.
/// What: one line per repository whose checkout is not on this machine. Paths
/// resolve against the manifest's own directory, which is `trusty-review`'s
/// rule for a relative entry.
/// Test: `super::rerender_tests::a_render_without_the_checkouts_says_so`.
fn checkout_gaps(manifest: &Path, read: &AuditManifest) -> Vec<String> {
    let base = manifest.parent().unwrap_or(Path::new("."));
    read.repositories
        .iter()
        .filter_map(|repo| {
            let path = if repo.path.is_absolute() {
                repo.path.clone()
            } else {
                base.join(&repo.path)
            };
            (!path.is_dir()).then(|| {
                format!(
                    "no checkout for {} at {} — this render rebuilds from the collected \
                     artifacts, without the code scan and analysis that need the repository",
                    repo.name,
                    path.display()
                )
            })
        })
        .collect()
}

/// Run `trusty-review report` over one manifest, capturing everything it says.
///
/// Why: the argument vector is DOC-67 §6's tga→trusty-review contract, spelled
/// the way `tga::audit::review::report_args` spells it — a re-render that asked
/// for something different would not be re-rendering the same report.
/// `--synthesize` is a deprecated no-op on a current binary and the only switch
/// that turns inference on at all on an older one, so it is passed for the same
/// reason tga passes it.
/// What: spawns the child, writes its combined output to `log` with every known
/// credential removed, and maps a non-zero exit onto a recorded failure. Only a
/// failure to START is an error, and it is the whole run's: a renderer that
/// cannot be spawned cannot be spawned for the next manifest either.
/// Test: `super::rerender_tests::a_failing_render_is_recorded_and_the_run_continues`,
/// `super::rerender_tests::a_renderer_that_cannot_start_names_the_binary`.
fn spawn_review(
    review: &Path,
    manifest: &Path,
    output: &Path,
    log: &Path,
    key: &SecretKey,
    inference: &[(&'static str, String)],
) -> Result<RenderResult, AuditError> {
    let mut command = std::process::Command::new(review);
    command
        .arg("report")
        .arg("--manifest")
        .arg(manifest)
        .arg("--analyze")
        .arg("--synthesize")
        .arg("--out")
        .arg(output)
        // #5671: the key alone never reaches OpenRouter — trusty-review
        // defaults to Bedrock unless the provider and all three role models are
        // named too. Resolved by the caller, all four or none.
        .env(ENV_INFERENCE_CREDENTIAL, key.expose())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in inference {
        command.env(name, value);
    }

    let finished = match command.output() {
        Ok(finished) => finished,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(AuditError::ReviewBinaryMissing {
                binary: review.display().to_string(),
            });
        }
        Err(source) => {
            return Err(AuditError::ReviewBinaryMissing {
                binary: format!("{} ({source})", review.display()),
            });
        }
    };

    // #5869's rule, one call rather than a stream: the child is handed the key,
    // so anything it echoes can carry it into a file a human opens.
    let needles = [key.expose().to_owned()];
    let transcript = trusty_common::credentials::scrub_secrets(
        &format!(
            "{}{}",
            String::from_utf8_lossy(&finished.stdout),
            String::from_utf8_lossy(&finished.stderr)
        ),
        &needles,
    );
    std::fs::write(log, transcript).map_err(|source| AuditError::WorkDir {
        path: log.to_path_buf(),
        source,
    })?;

    if finished.status.success() {
        return Ok(RenderResult::Succeeded);
    }
    Ok(RenderResult::Failed {
        reason: format!(
            "`{} report` {} — see {}",
            review.display(),
            match finished.status.code() {
                Some(code) => format!("exited with code {code}"),
                None => "was killed by a signal".to_owned(),
            },
            log.display()
        ),
    })
}

/// The files a render left behind, sorted, so the operator is told what to open.
///
/// An unreadable output directory yields an empty list rather than a failure:
/// the child already reported success, and this is a convenience for the
/// operator, not part of the verdict.
fn written_files(output: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(output) else {
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

/// A measured span as whole milliseconds, saturating rather than wrapping.
fn millis(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn mkdir(path: &Path) -> Result<(), AuditError> {
    std::fs::create_dir_all(path).map_err(|source| AuditError::WorkDir {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod rerender_tests {
    use super::*;

    const MANIFEST: &str = "[report]\ntitle = \"Acme\"\n\n[[repositories]]\n\
                            name = \"acme/api\"\npath = \"/nowhere/acme-api\"\n";

    fn work_in(root: &Path) -> WorkDir {
        let work = WorkDir::new(root.join("work"));
        work.create().expect("the working directory is created");
        work
    }

    /// A report directory holding a manifest, as a package or a sweep leaves it.
    fn report_dir(parent: &Path, stem: &str) -> PathBuf {
        let dir = parent.join(stem);
        std::fs::create_dir_all(&dir).expect("mkdir report");
        std::fs::write(dir.join("manifest.toml"), MANIFEST).expect("write manifest");
        dir
    }

    /// A stub `trusty-review` that behaves like the real one's report verb.
    fn stub_review(at: &Path, script: &str) -> PathBuf {
        let path = at.join("trusty-review");
        std::fs::write(&path, script).expect("stub binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }

    /// Writes one artifact into `--out`, which is what a real render leaves.
    const WRITES_A_REPORT: &str = "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  \
        case \"$1\" in --out) out=\"$2\"; shift;; esac\n  shift\ndone\n\
        mkdir -p \"$out\"\nprintf '# report\\n' > \"$out/report.md\"\nexit 0\n";

    fn key() -> SecretKey {
        SecretKey::new("sk-or-v1-not-a-real-key-0123456789")
    }

    async fn run(from: &Path, review: &Path, work: &WorkDir) -> RerenderReport {
        run_in(Some(from), from, review, work)
            .await
            .expect("the re-render runs")
    }

    /// One re-render, with the source and the cwd named separately (#6080).
    ///
    /// The config path names a file that is not there, so the engagement rung
    /// of [`resolve_source`] is inert unless a test plants one — which
    /// `run_from_config_dir` does.
    async fn run_in(
        from: Option<&Path>,
        cwd: &Path,
        review: &Path,
        work: &WorkDir,
    ) -> Result<RerenderReport, AuditError> {
        let config = cwd.join(crate::config::EngagementConfig::FILE_NAME);
        run_with_config(from, cwd, &config, review, work).await
    }

    /// [`run_in`] with the engagement config named separately (#6080).
    async fn run_with_config(
        from: Option<&Path>,
        cwd: &Path,
        config: &Path,
        review: &Path,
        work: &WorkDir,
    ) -> Result<RerenderReport, AuditError> {
        super::rerender(
            work,
            cwd,
            config,
            &key(),
            &crate::inference::Inference::default(),
            &crate::config::ReportSettings::default(),
            &RerenderOptions {
                from: from.map(Path::to_path_buf),
                out: None,
                review: Some(review.to_path_buf()),
            },
            // These drive an EXPLICIT renderer, which #6173 never reconciles.
            None,
            &Progress::none(),
        )
        .await
    }

    /// 🔴 The delivered package's own layout — `reports/<stem>/manifest.toml` —
    /// is what the recipient unzips, so it is the layout that must work with no
    /// flags beyond `--from`.
    #[tokio::test]
    async fn a_delivered_package_layout_is_rendered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        report_dir(&package.join("reports"), "01-acme-api");
        report_dir(&package.join("reports"), "02-acme-web");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run(&package, &review, &work_in(tmp.path())).await;

        assert_eq!(report.reports.len(), 2, "{:?}", report.reports);
        assert!(report.failures().next().is_none(), "{:?}", report.reports);
        assert_eq!(
            report
                .reports
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["01-acme-api", "02-acme-web"]
        );
        for rendered in &report.reports {
            assert_eq!(
                rendered.artifacts,
                vec![rendered.output.join("report.md")],
                "the render must say which file it wrote"
            );
        }
        assert_eq!(report.output, package.join(DEFAULT_OUTPUT_DIR));
    }

    /// The working directory's layout — `out/<stem>/` — is the same capability
    /// for the operator who still has the engagement on disk.
    #[tokio::test]
    async fn a_working_directory_layout_is_rendered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        report_dir(&work.root().join("out"), "01-acme-api");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run(work.root(), &review, &work).await;

        assert_eq!(report.reports.len(), 1, "{:?}", report.reports);
        assert!(report.reports[0].result.succeeded());
    }

    /// 🔴 #6080's whole point: the recipient unzips the package, changes into
    /// it, and types `trusty-audit render`. Nothing names the source, and the
    /// package it was run in is what renders — into `rerendered/` beside it.
    #[tokio::test]
    async fn an_unzipped_package_is_rendered_from_the_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("acme-audit");
        report_dir(&package.join("reports"), "01-acme-api");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);
        // The work dir carries a report of its own, so rendering the package
        // proves the cwd WINS rather than that only one manifest existed.
        let work = work_in(tmp.path());
        report_dir(&work.root().join("out"), "99-stale");

        let report = run_in(None, &package, &review, &work)
            .await
            .expect("the re-render runs");

        assert_eq!(report.source, package);
        assert_eq!(report.output, package.join(DEFAULT_OUTPUT_DIR));
        assert_eq!(
            report
                .reports
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["01-acme-api"]
        );
    }

    /// The operator who still has the engagement on disk keeps what they had:
    /// a cwd with nothing to render falls back to the working directory.
    #[tokio::test]
    async fn a_cwd_with_no_reports_falls_back_to_the_work_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("mkdir");
        let work = work_in(tmp.path());
        report_dir(&work.root().join("out"), "01-acme-api");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run_in(None, &elsewhere, &review, &work)
            .await
            .expect("the re-render runs");

        assert_eq!(report.source, work.root());
        assert_eq!(report.reports.len(), 1, "{:?}", report.reports);
    }

    /// Neither directory carries a report: the refusal names the one the
    /// operator was standing in, not one they never chose.
    #[tokio::test]
    async fn nothing_anywhere_refuses_by_naming_the_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("mkdir");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let refusal = run_in(None, &elsewhere, &review, &work_in(tmp.path()))
            .await
            .expect_err("nothing to render must refuse");

        assert!(
            refusal
                .to_string()
                .contains(&elsewhere.display().to_string()),
            "{refusal}"
        );
    }

    /// 🔴 An explicit `--from` still decides, whatever any default holds.
    #[test]
    fn an_explicit_source_beats_every_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let named = tmp.path().join("named");
        let cwd = tmp.path().join("cwd");
        report_dir(&cwd.join("reports"), "01-acme-api");
        let work = work_in(tmp.path());
        report_dir(&work.root().join("out"), "01-acme-api");
        let config = engagement_config_in(&cwd);
        report_dir(&cwd.join("work").join("out"), "01-acme-api");

        let choice = resolve_source(Some(named.clone()), &cwd, &config, work.root());
        assert_eq!(
            choice.source, named,
            "the flag must win over a cwd that would have rendered"
        );
        assert_eq!(
            choice.searched,
            vec![named],
            "with --from nothing else was tried, so nothing else may be listed"
        );
    }

    /// An `engagement.toml` at `dir`, as the operator's own engagement leaves it.
    fn engagement_config_in(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).expect("mkdir");
        let path = dir.join(crate::config::EngagementConfig::FILE_NAME);
        // Contents are irrelevant: `resolve_source` asks only whether a config
        // is there, because the engagement's work root is a layout convention
        // and not a field. See `engagement_work_dir`.
        std::fs::write(&path, "openrouter_key = \"sk-or-v1-x\"\n").expect("write config");
        path
    }

    /// 🔴 #6080's second failure, from the live dogfood run: the engagement's
    /// config is in the cwd, every sweep ran with `--work-dir <cwd>/work`, and
    /// `trusty-audit render` with no arguments refused — the cwd itself carries
    /// no report and the TOOL's default work root is empty on this machine, so
    /// the reports one directory down were never looked for.
    #[tokio::test]
    async fn a_config_directorys_own_work_dir_is_rendered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engagement = tmp.path().join("dogfood-audit");
        let config = engagement_config_in(&engagement);
        report_dir(&engagement.join("work").join("out"), "01-acme-api");
        // The tool's default work root holds a different report, so rendering
        // the engagement's own proves the rung ORDER rather than that only one
        // candidate existed.
        let work = work_in(tmp.path());
        report_dir(&work.root().join("out"), "99-stale");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run_with_config(None, &engagement, &config, &review, &work)
            .await
            .expect("the re-render runs");

        assert_eq!(report.source, engagement.join("work"));
        assert_eq!(
            report
                .reports
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["01-acme-api"]
        );
    }

    /// The cwd still outranks the engagement's work dir: a recipient standing in
    /// an unzipped package renders the package, config beside it or not.
    #[test]
    fn the_cwd_outranks_the_engagements_work_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().join("package");
        report_dir(&cwd.join("reports"), "01-in-the-package");
        let config = engagement_config_in(&cwd);
        report_dir(&cwd.join("work").join("out"), "02-in-the-work-dir");
        let work = work_in(tmp.path());

        assert_eq!(resolve_source(None, &cwd, &config, work.root()).source, cwd);
    }

    /// A `work/` beside a directory holding NO config is not an engagement's
    /// work dir — adopting it would render whatever happened to be sitting
    /// there, which is why the rung asks for the config first.
    #[test]
    fn a_work_dir_with_no_config_beside_it_is_not_adopted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().join("unrelated");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        report_dir(&cwd.join("work").join("out"), "01-not-ours");
        let work = work_in(tmp.path());
        let absent = cwd.join(crate::config::EngagementConfig::FILE_NAME);

        let choice = resolve_source(None, &cwd, &absent, work.root());
        assert_eq!(choice.source, cwd, "nothing renders, so the cwd is kept");
        assert_eq!(
            choice.searched,
            vec![cwd, work.root().to_path_buf()],
            "an absent config implies no work dir, so none may be listed"
        );
    }

    /// 🔴 A refusal names every directory the run tried, not only the one it
    /// settled on. Naming just the cwd is what made the live failure unreadable:
    /// the operator could see that directory, and the message said nothing about
    /// the two others already ruled out.
    #[tokio::test]
    async fn a_refusal_names_every_location_it_tried() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engagement = tmp.path().join("dogfood-audit");
        let config = engagement_config_in(&engagement);
        let work = work_in(tmp.path());
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let refusal = run_with_config(None, &engagement, &config, &review, &work)
            .await
            .expect_err("nothing to render must refuse");

        let message = refusal.to_string();
        for expected in [
            engagement.clone(),
            engagement.join("work"),
            work.root().to_path_buf(),
        ] {
            assert!(
                message.contains(&expected.display().to_string()),
                "{} is missing from the refusal: {message}",
                expected.display()
            );
        }
    }

    /// 🔴 The source is READ, never written into: everything this produces goes
    /// under the output directory, so the delivered package still hashes the way
    /// it was signed.
    #[tokio::test]
    async fn the_source_reports_are_left_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        let source = report_dir(&package.join("reports"), "01-acme-api");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run(&package, &review, &work_in(tmp.path())).await;

        let left: Vec<String> = std::fs::read_dir(&source)
            .expect("read source")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            ["manifest.toml"],
            "the package must not be written to"
        );
        assert!(report.reports[0].output.starts_with(&report.output));
    }

    /// The index this re-render wrote into its output directory (#6080).
    fn index_of(report: &RerenderReport) -> String {
        std::fs::read_to_string(report.output.join(crate::index_report::INDEX_FILE))
            .expect("the re-render writes an index")
    }

    /// 🔴 #6080: the re-render writes an index too, with the versions, the
    /// timestamp, the per-report timings and a link to what it produced.
    ///
    /// The version of `trusty-review` comes from asking the binary this run
    /// actually drove — a re-render has no install record to read, and the whole
    /// point of the verb is that it runs at whatever version is resolved.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_re_render_writes_an_index_into_its_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        report_dir(&package.join("reports"), "01-acme-api");
        let review = stub_review(
            tmp.path(),
            &WRITES_A_REPORT.replace(
                "#!/bin/sh\n",
                "#!/bin/sh\ncase \"$1\" in --version) echo 'trusty-review 0.16.0'; exit 0;; esac\n",
            ),
        );

        let report = run(&package, &review, &work_in(tmp.path())).await;

        let index = index_of(&report);
        assert!(index.contains("Reports: 1 of 1 report"), "{index}");
        assert!(index.contains("| `trusty-review` | 0.16.0 |"), "{index}");
        assert!(
            index.contains("| `tga` | not recorded | not recorded — a re-render runs no `tga`"),
            "an absent version must be stated, not left blank: {index}"
        );
        assert!(
            index.contains("[01-acme-api/report.md](01-acme-api/report.md)"),
            "{index}"
        );
        assert!(index.contains("## Timing"), "{index}");
        assert!(
            report.reports[0].duration_ms.is_some(),
            "{:?}",
            report.reports[0]
        );
    }

    /// 🔴 A report that would not render still gets an entry, with the failure
    /// named and its log linked. An index that simply omits the failure is how a
    /// partial re-render reads as a whole one.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_render_is_still_indexed_with_its_reason() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        report_dir(&package.join("reports"), "01-acme-api");
        let review = stub_review(tmp.path(), "#!/bin/sh\necho 'render failed' >&2\nexit 3\n");

        let report = run(&package, &review, &work_in(tmp.path())).await;

        let index = index_of(&report);
        assert!(index.contains("Reports: 0 of 1 report"), "{index}");
        assert!(index.contains("### 01-acme-api"), "{index}");
        assert!(index.contains("No report — "), "{index}");
        assert!(index.contains("exited with code 3"), "{index}");
        assert!(
            index.contains("[01-acme-api.log](01-acme-api.log)"),
            "the failed report's log must be linked: {index}"
        );
    }

    /// 🔴 The index goes in the OUTPUT directory and nowhere else — this
    /// module's postcondition covers the index as much as every report, and the
    /// delivered package still hashes the way it was signed.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_index_is_not_written_into_the_source_package() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        let source = report_dir(&package.join("reports"), "01-acme-api");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run(&package, &review, &work_in(tmp.path())).await;

        assert!(
            report
                .output
                .join(crate::index_report::INDEX_FILE)
                .is_file(),
            "the index belongs in the output directory"
        );
        for reserved in [&source, &package.join("reports")] {
            assert!(
                !reserved.join(crate::index_report::INDEX_FILE).exists(),
                "an index was written into the source package at {}",
                reserved.display()
            );
        }
        let left: Vec<String> = std::fs::read_dir(&source)
            .expect("read source")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            ["manifest.toml"],
            "the package must not be written to"
        );
    }

    /// A source with nothing to render is a refusal naming the directory, not an
    /// empty success — "0 reports re-rendered" reads as a finished run.
    #[tokio::test]
    async fn a_source_with_no_manifest_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).expect("mkdir");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let refusal = run_in(Some(&empty), &empty, &review, &work_in(tmp.path()))
            .await
            .expect_err("a source with no manifest must refuse");

        assert!(
            refusal.to_string().contains(&empty.display().to_string()),
            "{refusal}"
        );
    }

    /// 🔴 DOC-67 §9's failed-but-continuing model: one report that will not
    /// render must not withhold the ones that did, and the failure must carry
    /// its log.
    #[tokio::test]
    async fn a_failing_render_is_recorded_and_the_run_continues() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        report_dir(&package.join("reports"), "01-acme-api");
        report_dir(&package.join("reports"), "02-acme-web");
        // Fails only for the second report, by name.
        let review = stub_review(
            tmp.path(),
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  \
             case \"$1\" in --out) out=\"$2\"; shift;; esac\n  shift\ndone\n\
             case \"$out\" in *02-acme-web) echo 'render failed' >&2; exit 3;; esac\n\
             mkdir -p \"$out\"\nprintf '# report\\n' > \"$out/report.md\"\nexit 0\n",
        );

        let report = run(&package, &review, &work_in(tmp.path())).await;

        assert!(report.reports[0].result.succeeded());
        let failed = report.failures().collect::<Vec<_>>();
        assert_eq!(failed.len(), 1, "{:?}", report.reports);
        let RenderResult::Failed { reason } = &failed[0].result else {
            panic!("expected a recorded failure: {:?}", failed[0]);
        };
        assert!(reason.contains("code 3"), "{reason}");
        assert!(
            std::fs::read_to_string(&failed[0].log)
                .expect("the log is kept")
                .contains("render failed"),
            "the child's reason must survive in the log"
        );
    }

    /// 🔴 The credential is handed to the child and must not come back out of it
    /// into a file a human opens (#5869).
    #[tokio::test]
    async fn a_renderer_that_echoes_the_key_does_not_leave_it_in_the_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        report_dir(&package.join("reports"), "01-acme-api");
        let review = stub_review(
            tmp.path(),
            "#!/bin/sh\necho \"key=$OPENROUTER_API_KEY\" >&2\nexit 1\n",
        );

        let report = run(&package, &review, &work_in(tmp.path())).await;

        let log = std::fs::read_to_string(&report.reports[0].log).expect("the log is kept");
        assert!(
            !log.contains(key().expose()),
            "the key must not reach the log"
        );
    }

    /// A renderer that cannot be started fails the whole run rather than being
    /// recorded once per manifest — it will not start for the next one either.
    #[tokio::test]
    async fn a_renderer_that_cannot_start_names_the_binary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        report_dir(&package.join("reports"), "01-acme-api");
        let absent = tmp.path().join("no-such-trusty-review");

        let refusal = run_in(Some(&package), &package, &absent, &work_in(tmp.path()))
            .await
            .expect_err("an absent renderer must refuse");

        assert!(
            refusal.to_string().contains(&absent.display().to_string()),
            "{refusal}"
        );
    }

    /// 🔴 DOC-68 §12: a package carries no checkouts, so the dimensions that
    /// need one are STATED rather than left to read as a thinner report.
    #[tokio::test]
    async fn a_render_without_the_checkouts_says_so() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        report_dir(&package.join("reports"), "01-acme-api");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run(&package, &review, &work_in(tmp.path())).await;

        let gaps = &report.reports[0].gaps;
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(gaps[0].contains("acme/api"), "{gaps:?}");
        assert!(gaps[0].contains("/nowhere/acme-api"), "{gaps:?}");
    }

    /// A checkout that IS present states no gap — the line means something only
    /// if its absence does.
    #[tokio::test]
    async fn a_present_checkout_states_no_gap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        let dir = report_dir(&package.join("reports"), "01-acme-api");
        let checkout = tmp.path().join("acme-api");
        std::fs::create_dir_all(&checkout).expect("mkdir checkout");
        std::fs::write(
            dir.join("manifest.toml"),
            format!(
                "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"acme/api\"\n\
                 path = \"{}\"\n",
                checkout.display()
            ),
        )
        .expect("write manifest");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run(&package, &review, &work_in(tmp.path())).await;

        // #6081: a present checkout may still owe a GROUNDING gap — this
        // machine need not be running the two daemons. What it must never owe
        // is the missing-checkout line, which is what this test is about.
        assert!(
            !report.reports[0]
                .gaps
                .iter()
                .any(|g| g.contains("no checkout for")),
            "{:?}",
            report.reports[0]
        );
    }

    /// A manifest that will not parse fails ITS report and no other.
    #[tokio::test]
    async fn an_unreadable_manifest_fails_only_its_own_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        let broken = report_dir(&package.join("reports"), "01-broken");
        std::fs::write(broken.join("manifest.toml"), "this is not toml =").expect("write");
        report_dir(&package.join("reports"), "02-acme-web");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let report = run(&package, &review, &work_in(tmp.path())).await;

        assert_eq!(report.failures().count(), 1, "{:?}", report.reports);
        assert!(report.reports[1].result.succeeded());
    }

    /// The copy this client installed wins over whatever is on `PATH`, so a
    /// recipient re-renders with the binary that produced the reports.
    #[test]
    fn the_installed_copy_is_preferred_over_the_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());

        let installed = RequiredTool::TrustyReview.path_in(&work);
        std::fs::write(&installed, "#!/bin/sh\nexit 0\n").expect("stub binary");
        let nowhere = tmp.path().join("no-reports-here");
        assert_eq!(
            resolve_review(&work, &nowhere, None),
            (installed, ReviewSource::Engagement)
        );

        let explicit = tmp.path().join("elsewhere/trusty-review");
        assert_eq!(
            resolve_review(&work, &nowhere, Some(&explicit)),
            (explicit, ReviewSource::Explicit)
        );
    }

    /// 🔴 #6080's third failure, from the live dogfood run: `trusty-audit
    /// render` with no arguments found the engagement's reports under
    /// `<config dir>/work` and then rendered them with a `trusty-review` off
    /// `PATH`, two minor versions behind, while the engagement's own
    /// `work/tools/trusty-review` sat unused and the run printed the unchosen-
    /// renderer NOTE.
    ///
    /// The cause was two resolutions that could disagree: `resolve_source`
    /// discovered the work dir, and `resolve_review` asked the session's root,
    /// which the front end had resolved before discovery ran. This drives the
    /// live layout — config in the cwd, `work/` under it holding both the
    /// reports and the pinned renderer, and a session root that is somewhere
    /// else entirely — and asserts the discovered copy renders.
    ///
    /// It asserts the SOURCE and the PATH of the renderer rather than mutating
    /// `PATH`, which is process-global and would race every other test in this
    /// binary. Against `2efbfedbb` this resolved `ReviewSource::Path`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_discovered_work_dirs_tools_copy_renders() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engagement = tmp.path().join("dogfood-audit");
        let config = engagement_config_in(&engagement);
        report_dir(&engagement.join("work").join("out"), "01-acme-api");
        // The engagement's own pinned renderer, where `trusty-audit run` left it.
        let engagement_work = WorkDir::new(engagement.join("work"));
        engagement_work.create().expect("the engagement's work dir");
        let pinned = stub_review(
            &engagement_work.path(crate::workdir::Area::Tools),
            &WRITES_A_REPORT.replace(
                "#!/bin/sh\n",
                "#!/bin/sh\ncase \"$1\" in --version) echo 'trusty-review 0.21.0'; exit 0;; esac\n",
            ),
        );
        // The session's root is the tool default, which on the live machine
        // held nothing — so nothing here may answer for the renderer.
        let session = work_in(tmp.path());

        let report = super::rerender(
            &session,
            &engagement,
            &config,
            &key(),
            &crate::inference::Inference::default(),
            &crate::config::ReportSettings::default(),
            &RerenderOptions::default(),
            // The pin this stub answers with, so reconciliation is satisfied
            // rather than skipped — see #6173.
            Some("0.21.0"),
            &Progress::none(),
        )
        .await
        .expect("the re-render runs");

        assert_eq!(report.source, engagement.join("work"));
        assert_eq!(report.review, pinned, "the engagement's copy must render");
        assert_eq!(report.review_source, ReviewSource::Engagement);
        assert_eq!(report.review_version.as_deref(), Some("0.21.0"));
        assert!(
            !crate::cli::render(&crate::session::Outcome::Rerendered(report)).contains("NOTE:"),
            "a chosen renderer owes no unchosen-renderer disclosure"
        );
    }

    /// 🔴 THE #6173 REGRESSION TEST — the engagement bumped its pin, the
    /// engagement's own `tools/` copy is still the old one, and `render` drove
    /// it and exited 0.
    ///
    /// This is the live 2026-08-22 sequence: pin bumped 0.23.0 -> 0.23.1, the
    /// `tools/trusty-review` left by the last `install` still at 0.23.0, and
    /// the only trace a version in the report metadata read afterwards.
    /// `describe_renderer` (#6080) says nothing here, because the renderer WAS
    /// chosen — it is the engagement's, just not the engagement's current one.
    /// Against `fa8afbccc` this returns `Ok` and renders at 0.23.0.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_engagement_copy_is_refused_against_the_pin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engagement = tmp.path().join("dogfood-audit");
        let config = engagement_config_in(&engagement);
        report_dir(&engagement.join("work").join("out"), "01-acme-api");
        let engagement_work = WorkDir::new(engagement.join("work"));
        engagement_work.create().expect("the engagement's work dir");
        stub_review(
            &engagement_work.path(crate::workdir::Area::Tools),
            &WRITES_A_REPORT.replace(
                "#!/bin/sh\n",
                "#!/bin/sh\ncase \"$1\" in --version) echo 'trusty-review 0.23.0'; exit 0;; esac\n",
            ),
        );
        let session = work_in(tmp.path());

        let refusal = super::rerender(
            &session,
            &engagement,
            &config,
            &key(),
            &crate::inference::Inference::default(),
            &crate::config::ReportSettings::default(),
            &RerenderOptions::default(),
            Some("0.23.1"),
            &Progress::none(),
        )
        .await
        .expect_err("a copy off the pin must not render");

        assert!(
            matches!(
                &refusal,
                AuditError::VersionMismatch { tool, pinned, installed }
                    if *tool == "trusty-review" && pinned == "0.23.1" && installed == "0.23.0"
            ),
            "the refusal must name both versions, got {refusal:?}"
        );
    }

    /// An engagement copy AT the pin renders — the refusal above is about drift,
    /// not about the engagement branch existing.
    #[test]
    fn an_engagement_copy_at_the_pin_renders() {
        reconcile_review_pin(ReviewSource::Engagement, Some("0.23.1"), Some("0.23.1"))
            .expect("a copy at the pin is what the engagement asked for");
    }

    /// `--review-bin` is the documented escape hatch (#6173's own workaround),
    /// so reconciling it would leave no way past a bad pin. `PATH` is nobody's
    /// choice and discloses itself instead (#6080).
    #[test]
    fn an_explicit_renderer_is_never_reconciled() {
        for source in [ReviewSource::Explicit, ReviewSource::Path] {
            reconcile_review_pin(source, Some("0.23.0"), Some("0.23.1"))
                .unwrap_or_else(|e| panic!("{source:?} must not be reconciled: {e}"));
        }
    }

    /// An absence is not a difference: a package re-rendered where no
    /// `engagement.toml` loads pins nothing, and a binary that will not answer
    /// `--version` states nothing to compare.
    #[test]
    fn an_absent_pin_or_version_is_not_a_mismatch() {
        reconcile_review_pin(ReviewSource::Engagement, Some("0.23.0"), None)
            .expect("no pin, nothing to reconcile against");
        reconcile_review_pin(ReviewSource::Engagement, None, Some("0.23.1"))
            .expect("no version, nothing to reconcile");
    }

    /// 🔴 #6080: with nothing installed and no flag, the renderer is whatever
    /// this machine happens to have — in the live run a `trusty-review` two
    /// minor versions behind the engagement's own pin, chosen in silence. The
    /// source is recorded so [`crate::cli::render`] can say so.
    ///
    /// This asserts the SOURCE, not the path: whether a `trusty-review` is on
    /// this machine's `PATH` is a property of the machine running the test.
    #[test]
    fn a_path_resolved_renderer_reports_itself_as_unchosen() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());

        let (resolved, source) = resolve_review(&work, &tmp.path().join("no-reports-here"), None);
        assert_eq!(source, ReviewSource::Path);
        assert!(source.is_unchosen(), "a PATH lookup is nobody's choice");
        assert!(
            !resolved.starts_with(work.root()),
            "nothing is installed, so the engagement's tools/ cannot have answered: {}",
            resolved.display()
        );
        assert_eq!(
            resolved.file_name().expect("a file name"),
            RequiredTool::TrustyReview.binary_name(),
            "whatever it resolved must still be the renderer: {}",
            resolved.display()
        );
    }

    /// The version the report carries is the one the binary answered, and the
    /// index reads that same answer rather than spawning `--version` again.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_version_is_asked_for_once_and_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let package = tmp.path().join("package");
        report_dir(&package.join("reports"), "01-acme-api");
        let review = stub_review(
            tmp.path(),
            &WRITES_A_REPORT.replace(
                "#!/bin/sh\n",
                "#!/bin/sh\ncase \"$1\" in --version) echo 'trusty-review 0.18.0'; exit 0;; esac\n",
            ),
        );

        let report = run(&package, &review, &work_in(tmp.path())).await;

        assert_eq!(report.review_version.as_deref(), Some("0.18.0"));
        assert_eq!(
            report.review_source,
            ReviewSource::Explicit,
            "the test harness names the renderer, so nothing was left to PATH"
        );
        assert!(index_of(&report).contains("| `trusty-review` | 0.18.0 |"));
    }

    /// Two layouts carrying the same stem must not overwrite each other's
    /// output, which is what a shared directory name would do.
    #[test]
    fn a_repeated_stem_gets_its_own_output_directory() {
        let mut taken = HashSet::new();
        let first = unique_name(Path::new("/p/reports/01-acme/manifest.toml"), 0, &mut taken);
        let second = unique_name(Path::new("/p/out/01-acme/manifest.toml"), 1, &mut taken);
        assert_eq!(first, "01-acme");
        assert_ne!(second, first);
    }

    // ─── #6081: grounding on the tga-free render path ────────────────────────

    /// A manifest naming a checkout that IS here, and daemons that are not.
    fn present_checkout(tmp: &Path) -> (PathBuf, PathBuf) {
        let checkout = tmp.join("repos").join("acme-api");
        std::fs::create_dir_all(&checkout).expect("mkdir checkout");
        let manifest = tmp.join("manifest.toml");
        std::fs::write(
            &manifest,
            format!(
                "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"acme/api\"\npath = \"{}\"\n",
                checkout.display()
            ),
        )
        .expect("write manifest");
        (manifest, checkout)
    }

    /// Sockets nothing has ever bound, and binaries that do not exist, so no
    /// test reaches a real daemon or a real install.
    fn unreachable_tools() -> grounding::Tools {
        grounding::Tools {
            search: PathBuf::from("/nonexistent/trusty-search"),
            // #6285: a path nothing has ever bound — refused immediately.
            search_socket: PathBuf::from("/nonexistent/trusty-search.sock"),
            analyze: PathBuf::from("/nonexistent/trusty-analyze"),
            // #6287: a path nothing has ever bound — the socket equivalent of
            // port 1, refused immediately.
            analyze_socket: PathBuf::from("/nonexistent/trusty-analyze.sock"),
            bind_timeout: std::time::Duration::from_millis(50),
            startup_timeout: std::time::Duration::from_millis(100),
            poll_interval: std::time::Duration::from_millis(20),
        }
    }

    /// #6081: before this, a re-render with `--analyze` and no daemon produced a
    /// report whose code-analysis sections were empty, and said nothing about
    /// it. The gap is now stated per repository.
    #[tokio::test]
    async fn a_render_whose_daemons_are_down_says_so() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (manifest, _checkout) = present_checkout(tmp.path());
        let read = AuditManifest::load(&manifest).expect("parses");

        let gaps = ground_present_checkouts(&unreachable_tools(), &manifest, &read).await;

        // #6079: the churn leg speaks for every checkout — this fixture holds no
        // repository, and that is a line of its own. This test is about the
        // daemon legs; `churn::churn_tests` owns the churn wording.
        let gaps: Vec<String> = gaps
            .into_iter()
            .filter(|gap| !gap.contains(crate::grounding::churn::COLLECTOR))
            .collect();
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(gaps[0].contains("acme/api"), "{gaps:?}");
        assert!(gaps[0].contains("trusty-search"), "{gaps:?}");
    }

    /// This module's postcondition: nothing under the source is written except
    /// the output directory. Grounding here reads and measures; the ranking it
    /// produces belongs to the sweep that wrote the manifest, not to a render.
    #[tokio::test]
    async fn grounding_writes_nothing_into_the_package() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (manifest, _checkout) = present_checkout(tmp.path());
        let before = std::fs::read_to_string(&manifest).expect("read");
        let read = AuditManifest::load(&manifest).expect("parses");

        let _ = ground_present_checkouts(&unreachable_tools(), &manifest, &read).await;
        assert_eq!(
            std::fs::read_to_string(&manifest).expect("read back"),
            before
        );
    }

    /// A checkout that is not on this machine costs no daemon: `checkout_gaps`
    /// already stated it, and one line per repository is the right number.
    #[tokio::test]
    async fn an_absent_checkout_is_not_grounded_twice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest = tmp.path().join("manifest.toml");
        std::fs::write(&manifest, MANIFEST).expect("write manifest");
        let read = AuditManifest::load(&manifest).expect("parses");

        assert_eq!(checkout_gaps(&manifest, &read).len(), 1);
        assert!(
            ground_present_checkouts(&unreachable_tools(), &manifest, &read)
                .await
                .is_empty()
        );
    }
}
