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

use tga::audit::{
    resolve_review_binary, run_full_sweep, run_review_report, sweep_gap_lines, AuditSweepStats,
    SweepOptions, DATA_HANDLING_NOTE,
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
/// Propagates a failure to create the output directory. A stage failure is
/// reported, not propagated.
pub async fn run(config: Config, db: &mut Database, args: AuditArgs) -> anyhow::Result<()> {
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
/// What: spawns the renderer, echoes stderr, prints the artifact paths, and
/// turns a failed render into an error — the sweep's own results are already
/// printed by then, so nothing is lost by exiting non-zero.
/// Test: `crate::audit::tests::missing_binary_is_a_named_actionable_error`
/// covers the not-installed path; the rendered output is covered by the
/// end-to-end smoke run.
async fn render_report(manifest_path: &Path, output: &Path) -> anyhow::Result<()> {
    println!("Rendering: {} report --manifest …", resolve_review_binary());
    let run = run_review_report(manifest_path, output).await?;

    if !run.stderr.trim().is_empty() {
        eprintln!("{}", run.stderr.trim_end());
    }
    if !run.success {
        anyhow::bail!(
            "`{} report` exited with {}; no due-diligence report was produced. The manifest at \
             {} is intact and can be rendered again once the cause is fixed.",
            resolve_review_binary(),
            run.code
                .map_or_else(|| "a signal".to_string(), |c| format!("code {c}")),
            manifest_path.display()
        );
    }

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
/// Test: `audit_command_reports_each_stage`.
fn write_stage_report(
    stats: &AuditSweepStats,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> std::io::Result<()> {
    writeln!(out, "\nStages:")?;
    for outcome in &stats.outcomes {
        let mark = if outcome.status.is_failure() {
            "FAILED"
        } else {
            "ok"
        };
        writeln!(
            out,
            "  {:<20} {:>6}  {:.1}s",
            outcome.stage.as_str(),
            mark,
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tga::audit::{AuditSweepStats, SweepStage};

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
}
