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
/// What: all three default. [`RerenderOptions::default`] re-renders the working
/// directory in place with the resolved `trusty-review`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RerenderOptions {
    /// The unzipped package, or a working directory. Defaults to the work dir.
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
    /// How it ended.
    pub result: RenderResult,
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
/// What: discovers the manifests, resolves the renderer, then one child per
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
pub async fn rerender(
    work: &WorkDir,
    key: &SecretKey,
    inference: &[(&'static str, String)],
    options: &RerenderOptions,
    progress: &Progress,
) -> Result<RerenderReport, AuditError> {
    let source = options
        .from
        .clone()
        .unwrap_or_else(|| work.root().to_path_buf());
    let manifests = discover(&source)?;
    let output = options
        .out
        .clone()
        .unwrap_or_else(|| source.join(DEFAULT_OUTPUT_DIR));
    let review = resolve_review(work, options.review.as_deref());
    // #6081: this is the tga-free render path, and nothing on it started a
    // daemon or indexed anything — so `--analyze` fetched nothing and every
    // analyze-derived section rendered empty over a clean exit. Resolved once,
    // because the two addresses come from discovery files and the environment
    // and re-reading them per manifest could disagree with itself mid-run.
    let tools = grounding::Tools::resolved(work);
    mkdir(&output)?;

    let total = manifests.len();
    progress.operation_started(Operation::Rerender, total);
    let mut reports = Vec::with_capacity(total);
    let mut taken = HashSet::new();
    for (index, manifest) in manifests.into_iter().enumerate() {
        let name = unique_name(&manifest, index, &mut taken);
        progress.unit_started(Operation::Rerender, name.as_str(), index + 1, total);
        let rendered =
            render_one(&review, &tools, &manifest, &output, name, key, inference).await?;
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

    Ok(RerenderReport {
        source,
        output,
        review,
        reports,
    })
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
        return Err(AuditError::NothingToRerender {
            searched: source.to_path_buf(),
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

/// The `trusty-review` this run drives.
///
/// Why: three sources, cheapest first, and no version check on any of them
/// (#6080 owner ruling — the verb runs at whatever version the binary is). The
/// working directory's copy comes before `PATH` so a recipient who ran the
/// engagement re-renders with the binary that produced the reports, without
/// having to say so.
/// What: the explicit path, else the copy under `tools/` when it is there, else
/// the bare name for the OS to resolve on `PATH`.
/// Test: `super::rerender_tests::the_installed_copy_is_preferred_over_the_path`.
fn resolve_review(work: &WorkDir, explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    let installed = RequiredTool::TrustyReview.path_in(work);
    if installed.is_file() {
        return installed;
    }
    PathBuf::from(RequiredTool::TrustyReview.binary_name())
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
        gaps.extend(grounding::ground(tools, &path, &repo.name).await.gaps);
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
        super::rerender(
            work,
            &key(),
            &[],
            &RerenderOptions {
                from: Some(from.to_path_buf()),
                out: None,
                review: Some(review.to_path_buf()),
            },
            &Progress::none(),
        )
        .await
        .expect("the re-render runs")
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

    /// A source with nothing to render is a refusal naming the directory, not an
    /// empty success — "0 reports re-rendered" reads as a finished run.
    #[tokio::test]
    async fn a_source_with_no_manifest_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).expect("mkdir");
        let review = stub_review(tmp.path(), WRITES_A_REPORT);

        let refusal = super::rerender(
            &work_in(tmp.path()),
            &key(),
            &[],
            &RerenderOptions {
                from: Some(empty.clone()),
                out: None,
                review: Some(review),
            },
            &Progress::none(),
        )
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

        let refusal = super::rerender(
            &work_in(tmp.path()),
            &key(),
            &[],
            &RerenderOptions {
                from: Some(package),
                out: None,
                review: Some(absent.clone()),
            },
            &Progress::none(),
        )
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
        assert_eq!(
            resolve_review(&work, None),
            PathBuf::from(RequiredTool::TrustyReview.binary_name()),
            "with nothing installed it falls back to PATH"
        );

        let installed = RequiredTool::TrustyReview.path_in(&work);
        std::fs::write(&installed, "#!/bin/sh\nexit 0\n").expect("stub binary");
        assert_eq!(resolve_review(&work, None), installed);

        let explicit = tmp.path().join("elsewhere/trusty-review");
        assert_eq!(resolve_review(&work, Some(&explicit)), explicit);
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

    /// Addresses with nothing listening, and binaries that do not exist, so no
    /// test reaches a real daemon or a real install.
    fn unreachable_tools() -> grounding::Tools {
        grounding::Tools {
            search: PathBuf::from("/nonexistent/trusty-search"),
            // Port 1 is privileged: a connect there is refused immediately.
            search_url: "http://127.0.0.1:1".to_owned(),
            analyze: PathBuf::from("/nonexistent/trusty-analyze"),
            analyze_url: "http://127.0.0.1:1".to_owned(),
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
