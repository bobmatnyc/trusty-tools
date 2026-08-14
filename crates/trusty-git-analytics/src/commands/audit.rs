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
    ensure_analyze_daemon, require_inference_credential, require_rendered_report_carries_synthesis,
    require_review_supports_required_inference, resolve_review_binary, run_full_sweep,
    run_review_report, sweep_gap_lines, AuditSweepStats, SweepOptions, SweepStage,
    DATA_HANDLING_NOTE,
};
use tga::core::config::Config;
use tga::core::db::Database;
use tga::report::dd_manifest::{build_dd_manifest, configured_secrets, DdManifestOptions};

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
/// [`run_full_sweep`], then prints one line per stage.
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
/// Propagates a missing inference credential, an unstartable `trusty-analyze`
/// daemon (#5670), and a failure to create the output directory — all whole-run
/// preconditions. A stage failure is reported, not propagated.
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

    // #5670: the third whole-run precondition. Nothing started `trusty-analyze`,
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
    let stats = run_full_sweep(&config, db, &options, None).await?;
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
    gaps.push(DATA_HANDLING_NOTE.to_string());
    let manifest = build_dd_manifest(
        &config,
        &DdManifestOptions {
            title: args.resolved_title(),
            analyst: args.analyst.clone(),
            client: args.client.clone(),
            gaps,
            // #5236: the renderer resolves a relative repository path against
            // the MANIFEST's directory, not ours; anchoring here is what keeps
            // it pointed at the checkout tga actually collected from.
            base_dir: std::env::current_dir().unwrap_or_default(),
        },
    )?;

    let manifest_path = output.join(MANIFEST_FILE);
    std::fs::write(&manifest_path, manifest.to_toml()?)?;
    println!("\nManifest: {}", manifest_path.display());

    render_report(&manifest_path, &output).await
}

/// Filename of the DD manifest written into the audit's output directory.
const MANIFEST_FILE: &str = "manifest.toml";

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

    use super::write_stage_report;

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
}
