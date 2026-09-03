//! `trusty-review report` CLI subcommand (M1, #2313).
//!
//! Why: the report subcommand is the single entry point for manifest-driven
//! technical-DD report generation from THIS binary. Since #6669 the pipeline it
//! drives lives in the library (`trusty_review::report::run`), because
//! `trusty-analyze` grew a `report` verb over the same pipeline and two
//! implementations of manifest loading, template precedence and the credential
//! preflight would drift. This module is now argument parsing and output
//! printing, nothing else.
//! What: defines [`ReportArgs`] (clap-derive), maps it onto
//! [`ReportRequest`](trusty_review::report::ReportRequest), and prints the
//! written paths. Progress goes to STDERR; the written paths go to STDOUT for
//! scripting.
//! Test: `tests::report_args_parse_defaults` verifies clap parsing and the
//! request mapping; the pipeline's own decisions are covered by
//! `report::run::tests` and the render path by `tests/report_e2e.rs`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;

use trusty_review::report::{ReportRequest, run_report};

// ─── Report args ──────────────────────────────────────────────────────────────

/// Arguments for the `report` subcommand.
///
/// Why: groups the manifest-driven report flags in one testable struct.
/// What: the manifest path (required), an optional template override, the
/// code-only switch, and the output directory (default `./reports`).
/// Test: `tests::report_args_parse_defaults`.
#[derive(Debug, Parser)]
pub struct ReportArgs {
    /// Path to the report manifest TOML file (required).
    #[arg(long, value_name = "FILE")]
    pub manifest: PathBuf,

    /// Template name or alias (e.g. `cast`, or `report-technical-dd-cast`).
    /// Precedence: this flag > manifest `[report].template` >
    /// `TRUSTY_AUDIT_REPORT_TEMPLATE` > default.
    #[arg(long, value_name = "NAME")]
    pub template: Option<String>,

    /// Render the template's non-code sections as stated out-of-scope
    /// boundaries (#6669).
    ///
    /// A code-only audit reads a repository and nothing else. The sections that
    /// need an interview, an operations dashboard, or a vendor benchmark corpus
    /// keep their headings and say so; the sections that ARE code-derived but
    /// are never cross-checked carry a line saying that. Nothing is dropped.
    /// The switch can only turn the mode ON — the manifest `[report].code_only`
    /// key and `TRUSTY_AUDIT_REPORT_CODE_ONLY` set it too, and omitting the flag
    /// never overrides either.
    #[arg(long)]
    pub code_only: bool,

    /// Path to a free-form analyst instructions markdown file (#2340).  The brief
    /// is recorded verbatim in the report and injected as focus directives.
    /// Precedence: this flag > manifest `[report].instructions`.
    /// A missing file is an error; an empty file is a warning (proceeds as absent).
    #[arg(long, value_name = "FILE")]
    pub instructions: Option<PathBuf>,

    /// Output directory for the generated report pair (`{slug}.md`/`.json`).
    #[arg(long, value_name = "DIR", default_value = "./reports")]
    pub out: PathBuf,

    /// Deprecated and ignored (#5454): synthesis is now unconditional.
    ///
    /// It is still accepted so that scripts, the `tga audit` invocation, and the
    /// recovery command printed on a failed render keep parsing against both the
    /// old and the new binary — `tga` resolves `trusty-review` from PATH, so the
    /// two versions are not released together and either may be installed.
    /// Passing it prints one deprecation line to stderr and changes nothing.
    #[arg(long)]
    pub synthesize: bool,

    /// Wave-3 investigation budget: max files sent per repository (#2357).
    /// Precedence: this flag > manifest `[report].investigate_max_files` > the
    /// environment > default.
    #[arg(long, value_name = "N")]
    pub investigate_max_files: Option<usize>,

    /// Wave-3 investigation budget: max total content bytes sent per repository
    /// (#2357).  Same precedence as `--investigate-max-files`.
    #[arg(long, value_name = "BYTES")]
    pub investigate_max_bytes: Option<usize>,

    /// Benchmark corpus directory (M3).  Overrides the manifest `[report].corpus`
    /// key and the per-user XDG default (`~/.local/share/trusty-review/benchmark/`).
    #[arg(long, value_name = "DIR")]
    pub corpus: Option<PathBuf>,

    /// After a successful run, append each analyzed repository's metrics snapshot
    /// to the corpus (one `<slug>-<sha-or-date>.json` per repo; overwrites the
    /// same key).  Repos without metrics contribute nothing.
    #[arg(long)]
    pub corpus_add: bool,

    /// Compute cross-repo percentile/quartile placement against the corpus and
    /// fill the benchmark tables.  Requires a resolvable corpus directory; a
    /// corpus with fewer than five peers is disclosed, never silently ranked.
    #[arg(long)]
    pub benchmark: bool,

    /// Disable Mermaid chart rendering under the Graph-Ready Data Appendix tables
    /// (#2366).  Charts are ON by default; this flag OR the manifest key
    /// `[report] mermaid = false` disables them, yielding byte-identical
    /// pre-wave-4 output.  Precedence: this flag forces off regardless of the
    /// manifest key.
    #[arg(long)]
    pub no_mermaid: bool,

    /// Populate the complexity-distribution chart and RED/AMBER finding bands
    /// deterministically from the trusty-analyze daemon (epic #2445).  OFF by
    /// default: a bare run stays scan-only.  Fills metrics ONLY for local-path
    /// repositories that declare no `metrics` file (declared metrics always win),
    /// and only when the repo is already indexed in trusty-search/trusty-analyze.
    /// Fully fail-open — an unindexed repo or an unreachable daemon logs a
    /// warning and falls through to the built-in scan; it never aborts the report.
    /// Daemon socket precedence: manifest `[report].analyze_socket` > env
    /// `PR_INTELLIGENCE_ANALYZER_SOCKET` > the derived default (#6287).
    #[arg(long)]
    pub analyze: bool,

    /// Seconds one corpus-scanning `--analyze` request may take (#6712).
    /// Applies to the complexity histogram and the refactor list, whose cost
    /// scales with the repository: grading a 104k-chunk index takes 41-46 s,
    /// against the 15 s these endpoints used to be allowed, so the §7 complexity
    /// table rendered unavailable on every large repository. Raises the
    /// diagnostics budget too when it exceeds that endpoint's own deadline
    /// ladder; the readiness probe keeps its short budget either way.
    /// Precedence: this flag > manifest `[report].analyze_timeout_secs` > 180.
    #[arg(long, value_name = "SECS")]
    pub analyze_timeout_secs: Option<u64>,
}

impl ReportArgs {
    /// The parser-free description of this run (#6669).
    ///
    /// Why: the library pipeline must not depend on this binary's clap types,
    /// and mapping in one place is what keeps a new flag from being wired to
    /// the wrong field.
    /// What: one field per flag, with `--synthesize` deliberately dropped — it
    /// is a documented no-op.
    /// Test: `tests::report_args_parse_defaults`.
    fn to_request(&self) -> ReportRequest {
        let mut req = ReportRequest::new(self.manifest.clone());
        req.template.clone_from(&self.template);
        req.code_only = self.code_only;
        req.instructions.clone_from(&self.instructions);
        req.out.clone_from(&self.out);
        req.investigate_max_files = self.investigate_max_files;
        req.investigate_max_bytes = self.investigate_max_bytes;
        req.corpus.clone_from(&self.corpus);
        req.corpus_add = self.corpus_add;
        req.benchmark = self.benchmark;
        req.no_mermaid = self.no_mermaid;
        req.analyze = self.analyze;
        req.analyze_timeout_secs = self.analyze_timeout_secs;
        req
    }
}

// ─── Command handler ──────────────────────────────────────────────────────────

/// Execute the `report` subcommand.
///
/// Why: keeps the binary a thin wrapper over the library entry point.
/// What: warns about the deprecated flag, maps the args, runs the pipeline, and
/// prints each written path to STDOUT so `$(trusty-review report ...)` is
/// scriptable.
///
/// # Errors
///
/// Whatever [`run_report`] refuses with — a manifest that will not load, an
/// absent inference credential, an unknown template, or a synthesis pass that
/// produced no verified prose.
///
/// Test: arg parsing via `tests::report_args_parse_defaults`; full render via
/// `tests/report_e2e.rs`.
pub async fn cmd_report(config_path: Option<&Path>, args: ReportArgs) -> Result<()> {
    if args.synthesize {
        eprintln!(
            "[trusty-review report] --synthesize is deprecated and ignored: synthesis is always on"
        );
    }
    let written = run_report(config_path, &args.to_request()).await?;
    for path in &written {
        println!("{}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: clap parsing of the report flags must be stable, and the mapping
    /// onto the library request must not silently drop one.
    /// What: parses a minimal invocation and asserts defaults + overrides on
    /// both the args and the request they produce.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_defaults() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--template",
            "report-technical-dd-cast",
        ])
        .expect("parse");
        assert_eq!(args.manifest, PathBuf::from("m.toml"));
        assert_eq!(args.template.as_deref(), Some("report-technical-dd-cast"));
        assert_eq!(args.out, PathBuf::from("./reports"));
        // M3 flags default off / unset.
        assert!(!args.benchmark);
        assert!(!args.corpus_add);
        assert!(args.corpus.is_none());
        // Epic #2445: --analyze defaults off.
        assert!(!args.analyze);
        // #6669: full scope unless asked otherwise.
        assert!(!args.code_only);

        let req = args.to_request();
        assert_eq!(req.manifest, PathBuf::from("m.toml"));
        assert_eq!(req.template.as_deref(), Some("report-technical-dd-cast"));
        assert_eq!(req.out, PathBuf::from("./reports"));
        assert!(!req.code_only);
    }

    /// Why: the epic #2445 `--analyze` flag must parse.
    /// What: parses `--analyze` and asserts the boolean is set.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_analyze() {
        let args = ReportArgs::try_parse_from(["report", "--manifest", "m.toml", "--analyze"])
            .expect("parse");
        assert!(args.analyze);
        assert!(args.to_request().analyze);
        assert!(
            args.to_request().analyze_timeout_secs.is_none(),
            "#6712: unset defers to the manifest key, then the default"
        );
    }

    /// Why (#6712): a flag that parses but never reaches the request leaves an
    /// operator raising the budget on a slow repository with nothing changed.
    /// What: `--analyze-timeout-secs` parses and lands on the request.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_analyze_timeout() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--analyze",
            "--analyze-timeout-secs",
            "600",
        ])
        .expect("parse");
        assert_eq!(args.analyze_timeout_secs, Some(600));
        assert_eq!(args.to_request().analyze_timeout_secs, Some(600));
    }

    /// Why (#6669): `--template cast --code-only` is the exact invocation the
    /// runbook a third party follows types.
    /// What: both parse, and both reach the request.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_cast_code_only() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--template",
            "cast",
            "--code-only",
        ])
        .expect("parse");
        assert!(args.code_only);
        let req = args.to_request();
        assert_eq!(req.template.as_deref(), Some("cast"));
        assert!(req.code_only);
    }

    /// Why: the M3 corpus/benchmark flags must parse and reach the request.
    /// What: parses `--corpus`, `--corpus-add`, `--benchmark`.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_benchmark() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--corpus",
            "/tmp/corpus",
            "--corpus-add",
            "--benchmark",
        ])
        .expect("parse");
        assert!(args.benchmark);
        assert!(args.corpus_add);
        assert_eq!(args.corpus.as_deref(), Some(Path::new("/tmp/corpus")));

        let req = args.to_request();
        assert!(req.benchmark);
        assert!(req.corpus_add);
        assert_eq!(req.corpus.as_deref(), Some(Path::new("/tmp/corpus")));
    }

    /// Why: the wave-3 investigation budget flags must reach the request.
    /// What: a CLI `--investigate-max-files` lands on the request; the byte cap
    /// stays unset for the manifest to fill.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_investigate_budget() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--synthesize",
            "--investigate-max-files",
            "7",
        ])
        .expect("parse");
        assert_eq!(args.investigate_max_files, Some(7));
        assert!(args.investigate_max_bytes.is_none());

        let req = args.to_request();
        assert_eq!(req.investigate_max_files, Some(7));
        assert!(req.investigate_max_bytes.is_none());
    }
}
