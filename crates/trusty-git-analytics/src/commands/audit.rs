//! `tga audit` — the acquisition-diligence orchestrator (DOC-67, #5235/#5237).
//!
//! Why: an acquirer's technical reviewer points one command at an org and gets
//! a due-diligence report. DOC-67 §2 binds that to a single shot: once this
//! command starts, nothing here prompts, confirms, or waits for input, and no
//! code path in it may grow one.
//! What: [`AuditArgs`] and [`run`]. The command owns orchestration and
//! operator-facing reporting; stage sequencing belongs to
//! [`tga::audit::run_full_sweep`], which it calls rather than re-sequencing the
//! eight subcommands itself (#5237, DOC-67 §7 Q1).
//! Test: `crate::audit::tests::audit_args_parse_every_flag` and
//! `audit_takes_no_positional_arguments` cover the clap wiring; the
//! sweep's own behavior is covered alongside it.

use std::path::{Path, PathBuf};

use clap::Args;

use anyhow::Context as _;
use tga::audit::{
    ensure_analyze_daemon, ensure_repositories_indexed, ensure_search_daemon, index_gap_lines,
    require_inference_credential, require_rendered_report_carries_synthesis,
    require_review_supports_required_inference, resolve_review_binary, run_full_sweep,
    run_review_report, sweep_gap_lines, AuditSweepStats, SweepOptions, SweepStage,
    DATA_HANDLING_NOTE,
};
use tga::core::config::Config;
use tga::core::db::Database;
// #5823: the relay that carries the sweep's stage events to a parent process.
use tga::core::progress::{ProgressBus, ProgressEvent, Stage, StageRelay};
use tga::report::dd_manifest::{
    build_dd_manifest, configured_secrets, dd_repository_entries, repo_name, DdManifestOptions,
};
// #5405: the board-correlation figures the DD report renders.
use tga::report::build_ticketing_summary;
// #5453/#6004: per-repository ownership/bus-factor/trajectory figures, plus the
// name-match probe that keeps an unmatched repository name from rendering as a
// derived zero.
use tga::report::{build_authorship_summary, recorded_repository_names, repository_has_commits};
use trusty_common::credentials::scrub_secrets;

/// Arguments for `tga audit`.
///
/// Why: DOC-67 §6's manifest mapping needs a title, an analyst, and a client
/// for the report's metadata block, and §2 forbids obtaining any of them
/// interactively — so each is a flag whose absence is simply `None`, never a
/// prompt and never a hard error. The template renders an absent field as
/// `not stated in source report` on its own.
/// What: the target org/workspace, the three metadata fields, an output
/// directory, and the collection window.
/// Test: `crate::audit::tests::audit_args_parse_every_flag`.
#[derive(Args, Debug, Default)]
#[command(
    about = "One-shot acquisition-diligence sweep over an org or configured repo set.",
    long_about = "Run tga's full data-collection pipeline across every configured repository \
and prepare an acquisition due-diligence package.\n\n\
This command is strictly non-interactive: once started it never prompts, \
confirms, or waits for input. Configure sources first with `tga install` or by \
hand in config.yaml, then run this.\n\n\
A stage that fails does not abort the run. Every stage is attempted, and the \
failures are named in the summary so a missing dimension reads as \"not \
assessed\" rather than as a clean pass.",
    after_help = "EXAMPLES:\n\
  # Audit everything in config.yaml, writing into ./audit-output\n\
  tga audit\n\n\
  # Named engagement, last 26 weeks, custom output directory\n\
  tga audit --org acme --client \"Acme Holdings\" --analyst \"J. Reviewer\" \\\n\
    --weeks 26 --output ./acme-dd"
)]
pub struct AuditArgs {
    /// GitHub organisation or Bitbucket workspace under audit.
    ///
    /// Used for the report's title and metadata. Repository discovery itself
    /// is #5215/#5216's job — this command audits whatever repositories the
    /// resolved config already names.
    #[arg(long, value_name = "ORG")]
    pub org: Option<String>,

    /// Report title. Defaults to `"<org> — Technical Due Diligence"`.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,

    /// Name of the analyst producing the report.
    #[arg(long, value_name = "NAME")]
    pub analyst: Option<String>,

    /// Name of the client the report is produced for.
    #[arg(long, value_name = "NAME")]
    pub client: Option<String>,

    /// Directory for the audit's outputs. [default: ./audit-output]
    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Limit collection to the last N ISO weeks.
    #[arg(long, value_name = "N")]
    pub weeks: Option<u32>,

    /// Write the manifest and stop, leaving the report to a later render.
    ///
    /// Why (#6163): tga writes the manifest and renders from it inside one
    /// process, and trusty-audit grounds that same manifest only after this
    /// process exits — so the report it produced was always built from an
    /// ungrounded file, with `inspect_priority`, `attributed_only`, and the
    /// search-evidence ranking landing too late to reach it. This flag is what
    /// lets a caller ground first and render second.
    /// What: skips the `trusty-review report` invocation. Everything before it
    /// — the sweep, the artifacts, the manifest — is unchanged, including the
    /// preconditions this command checks up front, so a run that would have
    /// rendered a report still refuses early if it could not have.
    /// Test: `crate::audit::tests::{audit_args_parse_every_flag,
    /// audit_runs_with_no_flags_at_all}` — the second pins the default, which
    /// is the half that matters: a bare `tga audit` must still render.
    #[arg(long)]
    pub no_render: bool,
}

impl AuditArgs {
    /// The report title, derived from `--org` when `--title` is absent.
    fn resolved_title(&self) -> String {
        if let Some(t) = &self.title {
            return t.clone();
        }
        match &self.org {
            Some(org) => format!("{org} — Technical Due Diligence"),
            None => "Technical Due Diligence".to_string(),
        }
    }
}

/// Default output directory when `--output` is not supplied.
const DEFAULT_OUTPUT_DIR: &str = "audit-output";

/// Run one audit end to end.
///
/// Why: this is the operator-facing half of DOC-67 — it turns flags into a
/// sweep, and the sweep's per-stage record into something a human reads. It
/// deliberately does not sequence the stages itself (#5237): duplicating that
/// order here is exactly the second implementation DOC-67 §5 forbids, and it
/// would drift from the TUI's "Run Audit" path.
/// What: creates the output directory, prints the engagement header, calls
/// [`run_full_sweep`], prints one line per stage, indexes every repository the
/// report is about to be built from (#5670), and renders.
///
/// The indexing pass sits between the sweep and the manifest for two reasons:
/// clone-on-demand (#5215) is what guarantees a repository is on disk, so it
/// cannot run earlier; and a repository that would not index owes a Gaps &
/// Caveats line, which the manifest carries, so it cannot run later. It never
/// fails the run — see [`ensure_repositories_indexed`].
///
/// Exit status is `Ok` whenever the sweep completed, even with failed stages —
/// DOC-67 §9's one-shot rule makes a failed stage a *named gap*, not a reason
/// to report the whole run as a failure. The failures are on stderr and in the
/// returned stats.
/// Test: `crate::audit::tests::sweep_runs_every_stage_in_order_and_survives_failures`
/// is [`run_full_sweep`]'s own contract, not something `run` adds — it asserts
/// that an unconfigured JIRA stage fails without aborting the sweep or its
/// `Ok` return. The per-stage rendering `run` layers on top of that is
/// covered separately, by `audit_command_reports_each_stage` below.
///
/// # Errors
///
/// Propagates a missing inference credential, an unstartable `trusty-search` or
/// `trusty-analyze` daemon (#5670), and a failure to create the output directory
/// — all whole-run preconditions. A stage failure is reported, not propagated.
pub async fn run(config: Config, db: &mut Database, args: AuditArgs) -> anyhow::Result<()> {
    // #5454: the report's narrative now requires inference, so a missing
    // credential is checked before ANY work — ahead of the output directory and
    // stage 1. It is the one failure knowable up front, and DOC-67 §2 gives this
    // command a single shot with nobody watching to re-run it with a flag.
    require_inference_credential()?;

    // #5454 review: the other whole-run precondition knowable up front. tga and
    // trusty-review are installed separately, so a new tga beside a pre-0.15
    // renderer is ordinary — and that renderer produces exactly the report this
    // ticket abolished, while exiting 0.
    require_review_supports_required_inference()?;

    // #5670: link 1 of the prerequisite chain, and it must precede the analyze
    // preflight below — `trusty-analyze serve` exits at its own trusty-search
    // check, and an analyze daemon that is already up answers `503 degraded` for
    // as long as trusty-search is unreachable, which the analyze probe reads as
    // no daemon at all. Reordering these two makes the analyze preflight refuse
    // every run on a machine whose trusty-search is down.
    ensure_search_daemon().await?;

    // #5670: the fourth whole-run precondition. Nothing started `trusty-analyze`,
    // and DOC-67 §8 sources the findings table, the complexity distribution and
    // the health factors from it alone — so a machine without the daemon
    // produced a report with those three sections empty, and exited 0. This
    // starts it, and refuses the run when it cannot.
    ensure_analyze_daemon().await?;

    let output = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OUTPUT_DIR));
    std::fs::create_dir_all(&output)?;

    println!("Audit: {}", args.resolved_title());
    println!(
        "  analyst: {}\n  client:  {}\n  output:  {}",
        args.analyst.as_deref().unwrap_or("not stated"),
        args.client.as_deref().unwrap_or("not stated"),
        output.display()
    );

    let options = SweepOptions {
        output: Some(output.clone()),
        weeks: args.weeks,
    };
    // #5823: a parent that spawned this process gets the sweep's per-stage
    // events on stderr. Off unless it asked (`TRUSTY_PROGRESS_RELAY`), so a
    // hand-run `tga audit` behaves exactly as before.
    let relay = StageRelay::from_env();
    let stats = run_full_sweep(&config, db, &options, relay.bus()).await?;
    print_stage_report(&stats);

    // #5236: the manifest is the whole tga→trusty-review seam. It carries the
    // engagement metadata, the repository set, and — #5239/#5244 — the areas
    // this run could not assess, so a failed stage reaches the report as a
    // stated gap instead of an empty table.
    //
    // #5239: the gap lines excerpt a stage's `anyhow` cause chain, which can
    // quote a credential back at us, so they are redacted before they are cut —
    // against the same needles the manifest builder uses.
    let secrets = configured_secrets(&config);
    let mut gaps = sweep_gap_lines(&stats, &secrets);

    // #5236: the renderer resolves a relative repository path against the
    // MANIFEST's directory, not ours; anchoring here is what keeps it pointed at
    // the checkout tga actually collected from.
    let base_dir = std::env::current_dir().unwrap_or_default();

    // #5670: index each repository before the renderer looks for its index.
    // After the sweep, because clone-on-demand (#5215) is what puts a repository
    // on disk, and before the manifest is built, because a repository that could
    // not be indexed owes a gap line and the manifest is where gap lines go.
    // #5823: the two post-sweep phases are minutes each (indexing walks every
    // checkout; the render calls an LLM), so a parent watching only the sweep
    // would see the display stop and the process keep running.
    let phase = announce(relay.bus(), PHASE_INDEX);
    let indexed = ensure_repositories_indexed(&dd_repository_entries(&config, &base_dir)).await;
    finish_phase(relay.bus(), phase);
    gaps.extend(index_gap_lines(&indexed, &secrets));

    // #5405: the board-correlation figures the report renders. A write failure
    // is named in Gaps & Caveats rather than aborting — DOC-67 §9's rule that an
    // unassessed dimension is stated, never silently absent.
    let ticketing = match ticketing_artifact(&stats, db, &output) {
        Ok(path) => path,
        Err(e) => {
            gaps.push(scrub_secrets(
                &format!(
                    "Ticketing correlation: the sweep linked commits to board items, but the \
                     figures could not be written to {TICKETING_FILE} ({e:#}). The report states \
                     no board coverage for this run."
                ),
                &secrets,
            ));
            None
        }
    };

    gaps.push(DATA_HANDLING_NOTE.to_string());
    let mut manifest = build_dd_manifest(
        &config,
        &DdManifestOptions {
            title: args.resolved_title(),
            analyst: args.analyst.clone(),
            client: args.client.clone(),
            gaps,
            base_dir,
            ticketing,
        },
    )?;

    // #5453/#6004: one authorship artifact per repository, written beside the
    // manifest. A per-repository failure is a named gap on the MANIFEST
    // itself (DOC-67 §9's "named gap, never a silent one" rule) rather than
    // an aborted run — the report's Authorship section degrades to that gap
    // for exactly the repositories it could not compute.
    let mut authorship_gaps: Vec<String> = Vec::new();
    for (i, (entry, repo_cfg)) in manifest
        .repositories
        .iter_mut()
        .zip(&config.repositories)
        .enumerate()
    {
        let repository = repo_name(repo_cfg.name.as_deref(), &repo_cfg.path);
        match authorship_artifact(db, &output, &repository, i) {
            Ok(AuthorshipArtifact::Written(path)) => entry.authorship = Some(path),
            Ok(AuthorshipArtifact::NameMatchedNothing(recorded)) => {
                authorship_gaps.push(scrub_secrets(
                    &authorship_no_match_gap(&entry.name, &repository, &recorded),
                    &configured_secrets(&config),
                ));
            }
            Err(e) => authorship_gaps.push(scrub_secrets(
                &format!(
                    "Authorship ({}): could not write the authorship artifact ({e:#}). The \
                     report states no authorship/key-person signal for this application.",
                    entry.name
                ),
                &configured_secrets(&config),
            )),
        }
    }
    manifest.report.gaps.extend(authorship_gaps);

    let manifest_path = output.join(MANIFEST_FILE);
    // #6190: trusty-audit's grounding pass writes `inspect_priority`,
    // `crate_topology` and the `investigate_*` budget into this same file after
    // this process exits, so a second run into a live engagement must fold into
    // what is there rather than replace it.
    let existing = read_existing_manifest(&manifest_path)?;
    std::fs::write(
        &manifest_path,
        manifest.to_toml_merged(existing.as_deref())?,
    )?;
    println!("\nManifest: {}", manifest_path.display());

    // #6163: the caller asked for the manifest alone, because it grounds this
    // file and renders it itself once the ranking is in place.
    if args.no_render {
        println!("Skipping the report render (--no-render).");
        relay.finish().await;
        return Ok(());
    }

    let phase = announce(relay.bus(), PHASE_RENDER);
    let rendered = render_report(&manifest_path, &output).await;
    match &rendered {
        Ok(()) => finish_phase(relay.bus(), phase),
        Err(e) => fail_phase(relay.bus(), phase, format!("{e:#}")),
    }
    // #5823: the relay's task is joined before returning, so the last verdict
    // reaches the parent rather than racing this function's return.
    relay.finish().await;
    rendered
}

/// Target name for the post-sweep repository-indexing phase (#5823).
const PHASE_INDEX: &str = "index repositories";

/// Target name for the post-sweep report render (#5823).
const PHASE_RENDER: &str = "render report";

/// Tell a watching parent that `phase` began, and hand back its name.
///
/// Why: [`run_full_sweep`] announces its own nine stages, but the two phases
/// after it have no instrumentation at all — and the render is the slowest step
/// of the whole command. Returning the name keeps the start and the finish from
/// drifting apart at the call site.
/// What: emits a [`Stage::Audit`] start event when the relay is on; a no-op
/// otherwise.
/// Test: `crate::audit::tests::the_post_sweep_phases_are_announced`.
fn announce(progress: Option<&ProgressBus>, phase: &'static str) -> &'static str {
    if let Some(bus) = progress {
        bus.emit(ProgressEvent::started(Stage::Audit, phase, Some(1)));
    }
    phase
}

/// Report that `phase` finished.
fn finish_phase(progress: Option<&ProgressBus>, phase: &'static str) {
    if let Some(bus) = progress {
        bus.emit(ProgressEvent::completed(Stage::Audit, phase, 1));
    }
}

/// Report that `phase` failed, with the reason the caller is about to return.
fn fail_phase(progress: Option<&ProgressBus>, phase: &'static str, reason: String) {
    if let Some(bus) = progress {
        bus.emit(ProgressEvent::failed(Stage::Audit, phase, reason));
    }
}

/// Filename of the DD manifest written into the audit's output directory.
const MANIFEST_FILE: &str = "manifest.toml";

/// The manifest already sitting where this run is about to write one (#6190).
///
/// Why: `manifest.toml` has a second writer, and losing what it put there is
/// silent — the run exits 0 and the collapsed investigation only shows up in
/// the delivered report. So an unreadable existing file is an error rather than
/// a reason to fall through to a replacing write.
/// What: `None` when nothing is there yet, which is the ordinary first-run case
/// and every run inside a trusty-audit sweep (each repository gets its own
/// output directory).
/// Test: `super::super::report::dd_manifest_merge_tests` covers what happens to
/// the text this returns.
///
/// # Errors
///
/// Propagates a read that fails for any reason other than the file's absence.
fn read_existing_manifest(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!(
            "the existing manifest at {} could not be read; it may carry investigation scope this \
             run does not own, so it is not overwritten",
            path.display()
        ))),
    }
}

/// Filename of the ticketing artifact, beside the manifest (#5405).
const TICKETING_FILE: &str = "ticketing.json";

/// Write the run's board-correlation figures beside the manifest.
///
/// Why (#5405): the sweep synced `work_items` and joined them to `commits`, and
/// the DD report read none of it. This is the artifact that carries those
/// figures across the tga→trusty-review process boundary.
/// What: returns the manifest-relative path when the correlation stage
/// succeeded and the file was written, and `None` when that stage failed — in
/// which case its own gap line already names the omission, so writing figures
/// from a half-run join would be worse than stating there are none. The path is
/// relative because trusty-review resolves it against the MANIFEST's directory.
/// Test: `tests::a_failed_correlation_stage_writes_no_ticketing_artifact`,
/// `tests::a_succeeded_correlation_stage_writes_the_artifact`.
///
/// # Errors
///
/// Propagates the database read and the file write; the caller turns either
/// into a named gap rather than a failed run.
fn ticketing_artifact(
    stats: &AuditSweepStats,
    db: &Database,
    output: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let correlated = stats
        .outcomes
        .iter()
        .any(|o| o.stage == SweepStage::Correlate && !o.status.is_failure());
    if !correlated {
        return Ok(None);
    }

    let summary = build_ticketing_summary(db.connection())?;
    std::fs::write(output.join(TICKETING_FILE), summary.to_json()?)?;
    Ok(Some(PathBuf::from(TICKETING_FILE)))
}

/// Write one repository's authorship figures beside the manifest (#5453/#6004).
///
/// Why: mirrors [`ticketing_artifact`]'s shape, but per-repository — `commits.
/// repository` distinguishes repositories within tga's single-database-per-
/// engagement audit flow, so authorship is computed with a `WHERE repository =
/// ?` filter rather than a second database.
/// What: `index` names the file uniquely (`authorship-{index}.json`) since two
/// repositories can share a display name. The path returned is
/// manifest-relative, matching every other artifact this command writes.
///
/// A name matching NO commit row returns [`AuthorshipArtifact::NameMatchedNothing`]
/// rather than an artifact (#5453 review): the aggregation cannot tell that case
/// from a genuinely empty repository, and both would render as a confident
/// "0 authors, bus factor 0" — so the probe runs first and the caller states a
/// gap instead.
/// Test: `tests::{authorship_artifact_is_written_per_repository,
/// a_failed_authorship_write_is_a_named_gap,
/// a_repository_name_matching_no_commits_is_a_named_gap_not_zero_authors}`.
///
/// # Errors
///
/// Propagates the database read and the file write; the caller turns either
/// into a named gap rather than a failed run.
fn authorship_artifact(
    db: &Database,
    output: &Path,
    repository: &str,
    index: usize,
) -> anyhow::Result<AuthorshipArtifact> {
    if !repository_has_commits(db.connection(), repository)? {
        return Ok(AuthorshipArtifact::NameMatchedNothing(
            recorded_repository_names(db.connection())?,
        ));
    }
    let summary = build_authorship_summary(db.connection(), repository)?;
    let filename = format!("authorship-{index}.json");
    std::fs::write(output.join(&filename), summary.to_json()?)?;
    Ok(AuthorshipArtifact::Written(PathBuf::from(filename)))
}

/// The gap line for a repository whose name matched no collected commit.
///
/// Why: the two causes read identically from the aggregation but need different
/// action from the operator — an empty database means collection produced
/// nothing (its own stage gap already says so), while a populated one under
/// other names means the config name and `commits.repository` drifted, and the
/// remedy is naming the recorded value. So the line states which case it is.
/// Test: `tests::a_repository_name_matching_no_commits_is_a_named_gap_not_zero_authors`.
fn authorship_no_match_gap(display_name: &str, repository: &str, recorded: &[String]) -> String {
    if recorded.is_empty() {
        format!(
            "Authorship ({display_name}): the sweep recorded no commits at all, so there is no \
             authorship/key-person signal for this application."
        )
    } else {
        format!(
            "Authorship ({display_name}): no collected commit is recorded under the repository \
             name `{repository}` (the database holds commits under: {}). The report states no \
             authorship/key-person signal for this application rather than deriving zeroes from \
             an unmatched name.",
            recorded.join(", ")
        )
    }
}

/// What [`authorship_artifact`] produced for one repository.
///
/// Why: the two non-error outcomes are not interchangeable. Figures written
/// from real rows go on the manifest; a name that matched nothing must never
/// become an artifact of zeroes, because a reader cannot tell derived-zero from
/// no-data once it is rendered.
#[derive(Debug)]
enum AuthorshipArtifact {
    /// Figures were written; the manifest-relative path to them.
    Written(PathBuf),
    /// No `commits.repository` value equals this repository's name. Carries
    /// every name that IS recorded, so the gap line can point at the drift.
    NameMatchedNothing(Vec<String>),
}

/// Invoke `trusty-review report` and report what it produced.
///
/// Why: #5238 — the manifest is not the deliverable; the rendered report is.
/// The child's own streams are surfaced verbatim (DOC-67 §6 step 4) because
/// they carry the per-repository analyze warnings an operator needs, and its
/// artifact paths are printed last (step 5) so the run ends with the thing the
/// reader was promised.
/// What: spawns the renderer, echoes stderr, requires the report it wrote to
/// carry a written analysis, and prints the artifact paths — turning either
/// failure into an error, since the sweep's own results are already printed by
/// then and nothing is lost by exiting non-zero.
/// Test: `crate::audit::tests::missing_binary_is_a_named_actionable_error`
/// covers the not-installed path and
/// `exit_zero_over_a_narrative_free_report_is_a_failure` the exit-0-but-
/// deterministic one; the rendered output is covered by the end-to-end smoke
/// run.
async fn render_report(manifest_path: &Path, output: &Path) -> anyhow::Result<()> {
    println!("Rendering: {} report --manifest …", resolve_review_binary());
    let run = run_review_report(manifest_path, output).await?;

    if !run.stderr.trim().is_empty() {
        eprintln!("{}", run.stderr.trim_end());
    }
    if !run.success {
        // #5454: a failed inference pass lands here too, and the manifest that
        // makes the run resumable was written before `render_report` was called —
        // so the remedy is always the same one command, named in full.
        anyhow::bail!(
            "`{bin} report` exited with {code}; no due-diligence report was produced. Everything \
             collected is intact — the manifest at {manifest} survives this, so once the cause is \
             addressed re-run just the render:\n\n    {bin} report --manifest {manifest} \
             --analyze --synthesize --out {out}",
            bin = resolve_review_binary(),
            code = run
                .code
                .map_or_else(|| "a signal".to_string(), |c| format!("code {c}")),
            manifest = manifest_path.display(),
            out = output.display(),
        );
    }

    // #5454 review: exit 0 is not evidence a synthesis pass happened. A pre-0.15
    // renderer takes `--synthesize`, degrades to a narrative-free report when the
    // model call fails, and exits 0 — so the delivered artifact is what gets
    // checked, not the child's status.
    require_rendered_report_carries_synthesis(&run).with_context(|| {
        format!(
            "no due-diligence report was delivered. Everything collected is intact — the manifest \
             at {manifest} survives this, so once the renderer is upgraded re-run just the \
             render:\n\n    {bin} report --manifest {manifest} --analyze --out {out}",
            bin = resolve_review_binary(),
            manifest = manifest_path.display(),
            out = output.display(),
        )
    })?;

    println!("\nReport artifacts:");
    for path in &run.artifacts {
        println!("  {}", path.display());
    }
    Ok(())
}

/// Print one line per stage, then the roll-up.
///
/// Why: a silently-skipped stage is the failure mode DOC-67 §9 exists to
/// prevent, so every stage reports whether or not it succeeded.
/// What: `ok` / `FAILED` per stage on stdout, the failure detail on stderr.
/// Test: `audit_command_reports_each_stage` exercises the formatting through
/// [`write_stage_report`], the writer-parameterised body this delegates to —
/// `println!`/`eprintln!` write straight to the process's real stdout/stderr,
/// which a unit test cannot capture without process-level fd redirection.
fn print_stage_report(stats: &AuditSweepStats) {
    // #5303/#5308 follow-up: writing to an in-memory buffer can only fail on
    // an allocation failure, never on a real I/O error — `expect` is the
    // programmer-error case Code Contracts reserves it for, not a masked
    // fallback.
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    write_stage_report(stats, &mut out, &mut err).expect("writing to stdout/stderr");
}

/// Render the per-stage report into `out`/`err` instead of the process's
/// actual standard streams.
///
/// Why: [`print_stage_report`] is the one caller that matters at runtime, but
/// hard-coding `println!`/`eprintln!` inside it makes the formatting itself
/// unobservable from a test — this split is the whole fix.
/// What: identical output to `print_stage_report`, written through `out`/`err`
/// instead of `stdout()`/`stderr()`.
/// Test: `audit_command_reports_each_stage`, `collect_row_counts_stale_repositories`.
fn write_stage_report(
    stats: &AuditSweepStats,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> std::io::Result<()> {
    writeln!(out, "\nStages:")?;
    for outcome in &stats.outcomes {
        writeln!(
            out,
            "  {:<20} {:>6}  {:.1}s",
            outcome.stage.as_str(),
            stage_mark(stats, outcome),
            outcome.elapsed.as_secs_f64()
        )?;
    }
    writeln!(out, "\n{}", stats.summary())?;

    if stats.any_failed() {
        writeln!(
            err,
            "\nStages that did not complete (not assessed in this audit):"
        )?;
        for outcome in stats.failures() {
            if let tga::audit::StageStatus::Failed(msg) = &outcome.status {
                writeln!(err, "  {}: {msg}", outcome.stage)?;
            }
        }
    }
    Ok(())
}

/// One stage's status cell in the table.
///
/// Why: #5321 — `collect` succeeds when a repository falls back to stale local
/// refs, so the bare `ok` it earned is a status the operator cannot act on. The
/// report's Gaps & Caveats section states the same fact, but the person
/// watching the run has not got the report yet.
/// What: `FAILED` for a failed stage; `ok (N stale)` for the collect stage when
/// N repositories were collected from stale refs; `ok` otherwise — so a run
/// with no stale repository renders exactly as it did before.
/// Test: `collect_row_counts_stale_repositories`.
fn stage_mark(stats: &AuditSweepStats, outcome: &tga::audit::StageOutcome) -> String {
    if outcome.status.is_failure() {
        return "FAILED".to_string();
    }
    let stale = stats.stale_fetches.len();
    if outcome.stage == SweepStage::Collect && stale > 0 {
        return format!("ok ({stale} stale)");
    }
    "ok".to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tga::audit::{AuditSweepStats, StaleFetch, SweepStage};
    use tga::core::db::Database;

    use super::write_stage_report;
    use super::{announce, fail_phase, finish_phase, PHASE_INDEX, PHASE_RENDER};
    use tga::core::progress::{Outcome, ProgressBus};

    /// Why (#5823): [`run_full_sweep`] announces its nine stages, but the two
    /// phases after it — indexing every checkout, then an LLM-backed render —
    /// had no instrumentation at all, so a parent's display would stop while
    /// the process kept running for minutes.
    /// What: both phases emit a start, and their verdict reaches the bus with
    /// the reason attached when they fail.
    /// Test: this is the test.
    #[test]
    fn the_post_sweep_phases_are_announced() {
        let bus = ProgressBus::new();
        let phase = announce(Some(&bus), PHASE_INDEX);
        finish_phase(Some(&bus), phase);
        let phase = announce(Some(&bus), PHASE_RENDER);
        fail_phase(Some(&bus), phase, "the renderer exited 1".to_string());

        let events = bus.drain();
        let targets: Vec<&str> = events.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(
            targets,
            vec![PHASE_INDEX, PHASE_INDEX, PHASE_RENDER, PHASE_RENDER]
        );
        assert_eq!(events[1].outcome, Some(Outcome::Completed));
        assert_eq!(
            events[3].outcome.as_ref().and_then(Outcome::reason),
            Some("the renderer exited 1")
        );
        // A relay that is off costs nothing and says nothing.
        announce(None, PHASE_INDEX);
        finish_phase(None, PHASE_INDEX);
        fail_phase(None, PHASE_RENDER, "ignored".to_string());
    }

    /// Proves DOC-67 §9's "named gap, never a silent skip" obligation at the
    /// rendering layer: a failed stage prints `FAILED` (not silently `ok`) on
    /// stdout, and its cause on stderr. [`AuditSweepStats::summary`]'s own
    /// counting is already covered by
    /// `crate::audit::tests::summary_counts_successes_and_failures`; this
    /// test is about [`write_stage_report`]'s formatting, not the stats it
    /// formats.
    #[test]
    fn audit_command_reports_each_stage() {
        let mut stats = AuditSweepStats::default();
        stats.record(SweepStage::Collect, Instant::now(), Ok(()));
        stats.record(
            SweepStage::JiraSync,
            Instant::now(),
            Err(anyhow::anyhow!("no JIRA project configured")),
        );

        let mut out = Vec::new();
        let mut err = Vec::new();
        write_stage_report(&stats, &mut out, &mut err).expect("write to an in-memory buffer");
        let out = String::from_utf8(out).expect("stdout is UTF-8");
        let err = String::from_utf8(err).expect("stderr is UTF-8");

        // The succeeded stage is marked "ok" on stdout, and its failure text
        // never leaks into it.
        assert!(
            out.contains("collect") && out.contains("ok"),
            "missing the succeeded stage's ok mark: {out}"
        );
        assert!(
            !out.contains("no JIRA project configured"),
            "the failure detail must not appear on stdout: {out}"
        );

        // The failed stage is marked "FAILED" on stdout — never silently
        // "ok" — and its rollup line is present.
        assert!(
            out.contains("jira sync") && out.contains("FAILED"),
            "missing the failed stage's FAILED mark: {out}"
        );
        assert!(
            out.contains("1 of 2 stage(s) succeeded"),
            "missing the summary rollup line: {out}"
        );

        // The failure's cause is on stderr, named by stage.
        assert!(
            err.contains("jira sync") && err.contains("no JIRA project configured"),
            "missing the named failure detail on stderr: {err}"
        );
    }

    /// #5321 follow-up: the terminal table said a bare `ok` for a collect stage
    /// that fell back to stale refs on N repositories — success it had not fully
    /// earned, the same defect one surface over from the one this PR fixes.
    /// Pins both directions: the qualifier appears when repositories went stale,
    /// and a run with none renders the row exactly as it did before, right-
    /// aligned `ok` and no mention of staleness anywhere in the output.
    #[test]
    fn collect_row_counts_stale_repositories() {
        let render = |stats: &AuditSweepStats| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            write_stage_report(stats, &mut out, &mut err).expect("write to an in-memory buffer");
            String::from_utf8(out).expect("stdout is UTF-8")
        };
        let collect_row = |rendered: &str| {
            rendered
                .lines()
                .find(|l| l.contains("collect"))
                .expect("collect row present")
                .to_string()
        };

        let mut clean = AuditSweepStats::default();
        clean.record(SweepStage::Collect, Instant::now(), Ok(()));
        let clean_out = render(&clean);
        assert!(
            collect_row(&clean_out).contains("    ok  "),
            "a run with no stale repository must render the row unchanged: {clean_out}"
        );
        assert!(
            !clean_out.contains("stale"),
            "nothing about staleness belongs in a clean run: {clean_out}"
        );

        let mut stale = AuditSweepStats::default();
        stale.record(SweepStage::Collect, Instant::now(), Ok(()));
        for repo in ["acme-service", "acme-web"] {
            stale.record_stale_fetch(StaleFetch {
                repo: repo.to_string(),
                remote: "origin".to_string(),
                error: "unsupported URL protocol".to_string(),
            });
        }
        let stale_row = collect_row(&render(&stale));
        assert!(
            stale_row.contains("ok (2 stale)"),
            "the row must count the repositories that fell back: {stale_row}"
        );
    }

    /// #5405: the artifact is written only on the strength of a correlation
    /// stage that actually succeeded. A failed one leaves `None`, so the
    /// manifest declares nothing and the report states the absence instead of
    /// rendering figures from a half-completed join.
    #[test]
    fn a_failed_correlation_stage_writes_no_ticketing_artifact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("tga.db")).expect("open db");

        let mut stats = AuditSweepStats::default();
        stats.record(
            SweepStage::Correlate,
            Instant::now(),
            Err(anyhow::anyhow!("database is locked")),
        );

        let path = super::ticketing_artifact(&stats, &db, dir.path()).expect("no hard failure");
        assert_eq!(path, None, "a failed correlation stage declares nothing");
        assert!(
            !dir.path().join(super::TICKETING_FILE).exists(),
            "no artifact may be written for a failed correlation stage"
        );
    }

    /// #5405: the other direction — a succeeded stage writes the file and
    /// returns the MANIFEST-RELATIVE path, because trusty-review resolves it
    /// against the manifest's directory rather than tga's working directory.
    #[test]
    fn a_succeeded_correlation_stage_writes_the_artifact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("tga.db")).expect("open db");

        let mut stats = AuditSweepStats::default();
        stats.record(SweepStage::Correlate, Instant::now(), Ok(()));

        let path = super::ticketing_artifact(&stats, &db, dir.path()).expect("write");
        assert_eq!(path, Some(std::path::PathBuf::from(super::TICKETING_FILE)));

        let written = std::fs::read_to_string(dir.path().join(super::TICKETING_FILE))
            .expect("artifact written");
        assert!(
            written.contains("\"commits\"") && written.contains("\"work_items\""),
            "the artifact must carry the board counts: {written}"
        );
    }

    /// Seed one non-merge commit under `repository`, with one file touch.
    fn seed_commit(db: &Database, sha: &str, repository: &str) {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, is_merge) \
                 VALUES (?1, 'Alice', 'alice@x.com', '2026-01-15T00:00:00Z', 'msg', ?2, 0)",
                rusqlite::params![sha, repository],
            )
            .expect("insert commit");
        let commit_id = db.connection().last_insert_rowid();
        db.connection()
            .execute(
                "INSERT INTO files (commit_id, path, change_type) VALUES (?1, 'src/lib.rs', 'modified')",
                rusqlite::params![commit_id],
            )
            .expect("insert file");
    }

    /// #5453/#6004: the write-success half of the fail-open contract
    /// `authorship_artifact`'s doc comment states — figures for a repository
    /// that has commits become a file named by INDEX (two repositories may
    /// share a display name), and the returned path is MANIFEST-RELATIVE
    /// because trusty-review resolves it against the manifest's directory.
    #[test]
    fn authorship_artifact_is_written_per_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("tga.db")).expect("open db");
        seed_commit(&db, "a1", "acme-web");
        seed_commit(&db, "b1", "acme-api");

        let first = super::authorship_artifact(&db, dir.path(), "acme-web", 0).expect("write");
        let second = super::authorship_artifact(&db, dir.path(), "acme-api", 1).expect("write");

        let (super::AuthorshipArtifact::Written(first), super::AuthorshipArtifact::Written(second)) =
            (first, second)
        else {
            panic!("both repositories have commits, so both must produce figures");
        };
        assert_eq!(first, std::path::PathBuf::from("authorship-0.json"));
        assert_eq!(second, std::path::PathBuf::from("authorship-1.json"));

        let written = std::fs::read_to_string(dir.path().join("authorship-0.json"))
            .expect("artifact written");
        assert!(
            written.contains("\"repository\": \"acme-web\"") && written.contains("\"bus_factor\""),
            "the artifact must carry THIS repository's figures: {written}"
        );
        assert!(
            !written.contains("acme-api"),
            "the per-repository filter must not leak the sibling's commits: {written}"
        );
    }

    /// #5453/#6004: the error half — a write that cannot land is surfaced as an
    /// `Err` for the caller to turn into a named gap, never swallowed into a
    /// silent success that leaves the manifest pointing at a file which does not
    /// exist. Constructed by aiming the write at a path that is not a directory.
    #[test]
    fn a_failed_authorship_write_is_a_named_gap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("tga.db")).expect("open db");
        seed_commit(&db, "a1", "acme-web");

        // A regular file where the output directory should be: every write
        // beneath it fails at the OS layer.
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "").expect("create blocking file");

        let err = super::authorship_artifact(&db, &blocked, "acme-web", 0)
            .expect_err("an unwritable output must surface, not be swallowed");
        let rendered = format!("{err:#}");
        assert!(
            !rendered.is_empty(),
            "the error must carry a reason for the gap line to quote"
        );
        assert!(
            !blocked.join("authorship-0.json").exists(),
            "nothing may be left behind by a failed write"
        );
    }

    /// #5453 review: a repository name that matches no `commits.repository`
    /// value is a NAMED GAP, not an artifact of zeroes. Without the probe the
    /// aggregation returns "0 authors, bus factor 0" for a name that never
    /// joined a single row — indistinguishable, to a reader, from a measured
    /// finding that the codebase has no authors.
    #[test]
    fn a_repository_name_matching_no_commits_is_a_named_gap_not_zero_authors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("tga.db")).expect("open db");
        // Collection recorded `acme_web`; the manifest asks for `acme-web`.
        seed_commit(&db, "a1", "acme_web");

        let outcome = super::authorship_artifact(&db, dir.path(), "acme-web", 0).expect("no error");
        let super::AuthorshipArtifact::NameMatchedNothing(recorded) = outcome else {
            panic!("an unmatched name must never produce an artifact of zeroes");
        };
        assert_eq!(recorded, vec!["acme_web".to_string()]);
        assert!(
            !dir.path().join("authorship-0.json").exists(),
            "no artifact may be written for a name that matched nothing"
        );

        let gap = super::authorship_no_match_gap("Acme Web", "acme-web", &recorded);
        assert!(
            gap.contains("acme-web") && gap.contains("acme_web"),
            "the gap must name BOTH the name asked for and the name recorded: {gap}"
        );

        // The empty-database case reads differently — collection produced
        // nothing at all, which is not a naming drift.
        let empty = super::authorship_no_match_gap("Acme Web", "acme-web", &[]);
        assert!(
            empty.contains("no commits at all"),
            "an empty sweep must not be reported as a name mismatch: {empty}"
        );
    }
}
