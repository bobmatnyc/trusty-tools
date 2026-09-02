//! `trusty-analyze report` — the CAST technical-DD report verb (#6669).
//!
//! Why: the owner ruling for the fast-tracked code-only audit put the operator
//! front door on trusty-analyze, not trusty-review. This crate already embeds
//! the trusty-review pipeline for its `tr_review_*` tools, so the verb is a
//! wiring change rather than a second implementation: the manifest loading,
//! template precedence, credential preflight, investigation and synthesis all
//! stay in `trusty_review::report::run_report`, the library entry point
//! `trusty-review report` itself drives.
//! What: [`run`] maps the CLI's arguments onto a
//! [`ReportRequest`](trusty_review::report::ReportRequest) and prints the
//! written paths. The `cast` template alias and `--code-only` are resolved
//! inside that entry point, so this verb and `trusty-review report` cannot
//! disagree about what either means.
//! Test: `crates/trusty-analyze/tests/report_cli.rs`.

use std::path::PathBuf;

use anyhow::Result;
use trusty_review::report::{run_report, ReportRequest};

/// Build the request one `trusty-analyze report` invocation describes.
///
/// Why: separating the mapping from the run is what lets a test assert that
/// every flag reaches the pipeline without making a network call.
/// What: one field per flag; every option this verb does not expose keeps the
/// pipeline's own default (see [`ReportRequest::new`]). `--analyze` defaults ON
/// here, unlike in `trusty-review report`, because this binary IS the analyze
/// daemon's CLI — an operator running the report from it means to use the
/// metrics it serves, and the fetch is fail-open either way.
/// Test: `report_cli::request_carries_every_flag`.
#[must_use]
pub fn request(
    manifest: PathBuf,
    template: Option<String>,
    code_only: bool,
    out: PathBuf,
    instructions: Option<PathBuf>,
    no_analyze: bool,
) -> ReportRequest {
    let mut req = ReportRequest::new(manifest);
    req.template = template;
    req.code_only = code_only;
    req.out = out;
    req.instructions = instructions;
    req.analyze = !no_analyze;
    req
}

/// Run the report and print each written path to STDOUT.
///
/// Why: the paths on STDOUT make `$(trusty-analyze report …)` scriptable, the
/// same contract `trusty-review report` already has.
/// What: delegates to [`run_report`]; progress goes to STDERR from inside the
/// pipeline.
///
/// # Errors
///
/// Whatever [`run_report`] refuses with — a manifest that will not load, an
/// absent `OPENROUTER_API_KEY`, an unknown template name, or a synthesis pass
/// that produced no verified prose.
///
/// Test: `crates/trusty-analyze/tests/report_cli.rs::a_missing_manifest_is_refused_by_name`.
pub async fn run(req: ReportRequest) -> Result<()> {
    let written = run_report(None, &req).await?;
    for path in &written {
        println!("{}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#6669): a flag that parses but never reaches the pipeline is worse
    /// than one that does not exist — the operator sees no error and gets the
    /// wrong report. This pins the mapping.
    /// What: every flag lands on the request, and `--analyze` defaults ON here.
    /// Test: this test itself.
    #[test]
    fn request_carries_every_flag() {
        let req = request(
            PathBuf::from("/e/manifest.toml"),
            Some("cast".to_string()),
            true,
            PathBuf::from("/e/out"),
            Some(PathBuf::from("/e/instructions.md")),
            false,
        );
        assert_eq!(req.manifest, PathBuf::from("/e/manifest.toml"));
        assert_eq!(req.template.as_deref(), Some("cast"));
        assert!(req.code_only);
        assert_eq!(req.out, PathBuf::from("/e/out"));
        assert_eq!(
            req.instructions.as_deref(),
            Some(std::path::Path::new("/e/instructions.md"))
        );
        assert!(
            req.analyze,
            "this binary IS the analyzer, so the fetch is on unless refused"
        );
    }

    /// Why: `--no-analyze` is the only way to get the scan-only behaviour
    /// `trusty-review report` has by default, so it must actually turn the
    /// fetch off.
    /// What: the flag clears `analyze`.
    /// Test: this test itself.
    #[test]
    fn no_analyze_turns_the_fetch_off() {
        let req = request(
            PathBuf::from("m.toml"),
            None,
            false,
            PathBuf::from("./reports"),
            None,
            true,
        );
        assert!(!req.analyze);
        assert!(!req.code_only);
        assert!(req.template.is_none());
    }
}
