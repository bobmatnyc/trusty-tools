//! `sld-lint` — CLI front end for the Spec-Linked Documentation (DOC-38) linter.
//!
//! Why: a thin `anyhow`/clap wrapper keeps all logic in the unit-testable
//! library ([`trusty_sld_lint`]); this binary just parses flags, runs the
//! engine, prints diagnostics, and maps the result to an exit code.
//! What: with no subcommand, parses `--root`/`--strict`/`--allowlist`, runs
//! [`trusty_sld_lint::run`], prints each `path:line: [severity check] message`,
//! prints a one-line summary, and exits non-zero when any error-severity
//! finding survives the allowlist. The `gap-report` subcommand instead runs
//! [`trusty_sld_lint::gap::run_gap_report`] (issue #595 slice 1).
//! Test: exercised end-to-end by the wrapper `scripts/check_sld.sh` and the
//! library's `tests/lint_real_tree.rs`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use trusty_sld_lint::{run, LintOptions, ALLOWLIST_FILE};

/// Lint a repository for Spec-Linked Documentation (DOC-38) conformance.
#[derive(Parser, Debug)]
#[command(
    name = "sld-lint",
    version,
    about = "Lint Spec-Linked Documentation (SLD / DOC-38) references and spec-doc conventions.",
    long_about = "\
sld-lint enforces the DOC-38 Spec-Linked Documentation standard.

CHECKS
  Reference resolution (ALWAYS, everywhere in scope):
    - ref-anchor-mismatch  a reference's anchor must equal its id (§2.1 self-check)
    - ref-traversal        a reference path must not contain `..` or be absolute
    - ref-path-missing     the repo-root-relative target file must exist
    - ref-anchor-missing   no anchor — exact OR base-id — matches in the target
    - ref-revision-drift   ADVISORY (warning, never fails the lint): the target
                           carries the same base id at a DIFFERENT revision (a
                           ~v1 ref resolves a ~v2 section) — drift is flagged,
                           not enforced (DOC-38 §1.3, §4.4)
    - frontmatter-schema   `spec_refs:` YAML must be schema-valid (§2.5)
  Spec-document conventions (opted-in specs by default; ALL specs under --strict):
    - spec-header          the bold-field header block is present (§4.2)
    - spec-catalog         the self-labeled DOC-N has a catalog row (§4.5)
    - spec-id-grammar      every {#SPEC-…} anchor is a valid id (§2.1)
    - anchor-id-mismatch   an anchor equals its section's **ID:** (§4.3)
  Ratchet enforcement (--strict only, see STRICTNESS below):
    - allowlist-stale      a grandfathered entry no longer matches any
                           violation and must be removed

STRICTNESS / GRANDFATHERING
  Existing specs do not yet carry `spec_refs:` frontmatter (retrofit is DOC-38
  §10 F5/F6), so by DEFAULT the full spec-document checks run only on files that
  have OPTED IN (they declare `spec_refs:` frontmatter). Reference resolution runs
  everywhere regardless. Pass --strict to apply the full spec-document checks to
  EVERY spec — the eventual post-retrofit mode.

  Documented pre-existing exceptions (the DOC-28 self-label collision, the DOC-34
  catalog gap) are grandfathered via the allowlist TSV (default
  .sld-lint-allowlist.tsv). The ratchet is mechanically enforced under --strict:
  an entry whose (path, check) no longer matches any real violation is flagged
  `allowlist-stale` and fails the run until the row is removed — so the list can
  only shrink. (There is currently no guarded generator that refuses to ADD a
  new entry, unlike check_line_cap.sh --update; adding a row is a manual, reviewed
  edit.)

SCOPE
  docs/specs/**/*.md  (frontmatter references + spec-document checks)
  crates/**           (inline `# Spec References` blocks; test/benchmark files
                       are excluded — they carry synthetic fixture references)

Exits non-zero when any error-severity finding survives the allowlist."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Repository root to lint (contains `docs/specs/` and `crates/`).
    #[arg(long, default_value = ".")]
    root: PathBuf,

    /// Apply the full spec-document checks to every spec, not only opted-in ones.
    #[arg(long)]
    strict: bool,

    /// Grandfather-allowlist TSV path (default: <root>/.sld-lint-allowlist.tsv).
    #[arg(long)]
    allowlist: Option<PathBuf>,
}

/// `sld-lint` subcommands beyond the default lint run.
///
/// Why: issue #595 adds a read-only bidirectional traceability gap report as a
/// subcommand rather than a second binary, so it shares the crate's discovery
/// scope and CLI conventions.
/// What: [`Command::GapReport`] — see [`trusty_sld_lint::gap`].
#[derive(Subcommand, Debug)]
enum Command {
    /// Bidirectional SLD traceability gap report (backward + forward gaps).
    GapReport {
        /// Repository root to scan (contains `docs/specs/` and `crates/`).
        #[arg(long, default_value = ".")]
        root: PathBuf,

        /// Emit machine-readable JSON instead of (in addition to) the summary.
        #[arg(long)]
        json: bool,

        /// Exit non-zero when the report finds any gap or broken reference.
        ///
        /// Default: report-and-succeed (exit 0) — this is a read-only report,
        /// not a gate, so it can never unexpectedly break CI (issue #595).
        #[arg(long)]
        strict: bool,
    },
}

/// Parse flags, run the linter, print findings, and return an exit code.
///
/// Why: keeps `main` a thin adapter — no check logic, just I/O and exit mapping.
/// What: dispatches to [`run_gap_report_cmd`] for `gap-report`, else builds
/// [`LintOptions`], runs [`run`], prints each diagnostic and a summary line,
/// and returns `FAILURE` when the report is not clean.
/// Test: covered end-to-end by `tests/lint_real_tree.rs` + `scripts/check_sld.sh`.
fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    if let Some(Command::GapReport { root, json, strict }) = cli.command {
        return Ok(run_gap_report_cmd(&root, json, strict));
    }

    let strict = cli.strict;
    let allowlist_path = cli
        .allowlist
        .unwrap_or_else(|| cli.root.join(ALLOWLIST_FILE));
    let opts = LintOptions {
        root: cli.root,
        strict,
        allowlist_path,
    };

    let report = run(&opts)?;
    for diag in &report.diagnostics {
        println!("{diag}");
    }
    let errors = report.error_count();
    let warnings = report.diagnostics.len() - errors;
    println!(
        "sld-lint: scanned {} spec doc(s) + {} code file(s); {errors} error(s), {warnings} warning(s){}",
        report.spec_docs,
        report.code_files,
        if strict { " [strict]" } else { "" }
    );

    if report.is_clean() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Run the `gap-report` subcommand: print output, choose an exit code.
///
/// Why: kept separate from `main` so the dispatch in `main` stays a one-line
/// match arm.
/// What: runs [`trusty_sld_lint::gap::run_gap_report`], prints the JSON (when
/// `json`) and always prints the human [`GapReport::summary`], then returns
/// `FAILURE` only when `strict` is set AND the report is not
/// [`GapReport::is_strict_clean`] — the default is report-and-succeed so this
/// subcommand can never unexpectedly break CI (issue #595).
/// Test: covered end-to-end by `tests/gap_report_real_tree.rs`.
fn run_gap_report_cmd(root: &Path, json: bool, strict: bool) -> ExitCode {
    let report = trusty_sld_lint::gap::run_gap_report(root);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).expect("gap report serializes")
        );
    }
    println!("{}", report.summary());

    if strict && !report.is_strict_clean() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
