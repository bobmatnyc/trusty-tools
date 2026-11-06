//! Unit tests for `pipeline::runner_helpers::resolve_diff_token` (#1880).
//!
//! Why: split from `runner_helpers.rs` to keep that file under the 500-line
//! cap while covering the serve-mode empty-token regression directly at the
//! function boundary (no network required — App-mode resolution fails fast
//! on missing credentials/installation before any HTTP call is made).
//! What: exercises all four branches of `resolve_diff_token`: `LocalFile`
//! pass-through, already-resolved-token pass-through, Serve-mode failure
//! without App credentials, and Cli-mode resolution from `config.github_token`.
//! #10762: also the anchor partition of `attach_inline_comments`.
//! Test: this is the test module; each function is a self-contained unit test.

use super::*;
use std::path::PathBuf;

fn config_without_app_creds() -> ReviewConfig {
    let mut config = ReviewConfig::load(None);
    config.github_app_id = None;
    config.github_app_private_key = None;
    config.github_installations = Vec::new();
    config
}

#[tokio::test]
async fn resolve_diff_token_passes_through_local_file() {
    // A LocalFile source needs no GitHub credentials at all; it must pass
    // through unchanged regardless of run mode or config.
    let config = config_without_app_creds();
    let source = DiffSource::LocalFile {
        path: PathBuf::from("/tmp/whatever.diff"),
    };

    let resolved = resolve_diff_token(&source, &config, RunMode::Serve)
        .await
        .expect("LocalFile source must never fail token resolution");

    match resolved {
        DiffSource::LocalFile { path } => assert_eq!(path, PathBuf::from("/tmp/whatever.diff")),
        other => panic!("expected LocalFile to pass through unchanged, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_diff_token_passes_through_already_resolved_token() {
    // A Github source with a non-empty token (the CLI run/compare/calibrate
    // paths resolve up front) must be returned unchanged — no redundant
    // resolution attempt. We prove this by using Serve mode with NO App
    // credentials configured: if resolution were (incorrectly) attempted,
    // this would error; since the token is already set, it must not be.
    let config = config_without_app_creds();
    let source = DiffSource::Github {
        owner: "acme".to_string(),
        repo: "widgets".to_string(),
        pr: 42,
        token: "already-resolved-token".to_string(),
    };

    let resolved = resolve_diff_token(&source, &config, RunMode::Serve)
        .await
        .expect("a non-empty token must never trigger re-resolution");

    match resolved {
        DiffSource::Github { token, .. } => assert_eq!(token, "already-resolved-token"),
        other => panic!("expected Github source to pass through unchanged, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_diff_token_serve_mode_without_app_creds_errors() {
    // This is the #1880 regression: serve-mode webhook/handler paths build
    // `DiffSource::Github` with an empty token placeholder. Without App
    // credentials configured, resolution must fail CLOSED with an actionable
    // error — never silently return an empty token that would surface as an
    // opaque 401 from GitHub.
    let config = config_without_app_creds();
    let source = DiffSource::Github {
        owner: "acme".to_string(),
        repo: "widgets".to_string(),
        pr: 7,
        token: String::new(),
    };

    let err = resolve_diff_token(&source, &config, RunMode::Serve)
        .await
        .expect_err("empty token with no App credentials must fail, not silently proceed");

    let message = err.to_string();
    assert!(
        message.contains("GITHUB_APP_ID") || message.contains("GITHUB_APP_PRIVATE_KEY"),
        "error must name the missing config so an operator can act on it: {message}"
    );
}

#[tokio::test]
async fn resolve_diff_token_cli_mode_resolves_from_config_token() {
    // Cli mode with an empty placeholder token and `config.github_token` set
    // must resolve synchronously (no network) to that configured token.
    let mut config = config_without_app_creds();
    config.github_token = "ghp_configured_pat".to_string(); // pragma: allowlist secret
    let source = DiffSource::Github {
        owner: "acme".to_string(),
        repo: "widgets".to_string(),
        pr: 99,
        token: String::new(),
    };

    let resolved = resolve_diff_token(&source, &config, RunMode::Cli)
        .await
        .expect("Cli mode must resolve from config.github_token");

    match resolved {
        DiffSource::Github {
            token,
            owner,
            repo,
            pr,
        } => {
            assert_eq!(token, "ghp_configured_pat");
            assert_eq!(owner, "acme");
            assert_eq!(repo, "widgets");
            assert_eq!(pr, 99);
        }
        other => panic!("expected a resolved Github source, got {other:?}"),
    }
}

/// #9193: a head is a fork unless both labels name the same owner, compared
/// case-insensitively; a missing label cannot prove the same repository.
#[test]
fn head_is_fork_compares_label_owners() {
    assert!(!head_is_fork(Some("acme:feat/x"), Some("acme:main")));
    assert!(head_is_fork(Some("forker:feat/x"), Some("acme:main")));
    assert!(!head_is_fork(Some("ACME:feat/x"), Some("acme:main")));
    assert!(head_is_fork(None, Some("acme:main")));
    assert!(head_is_fork(Some("acme:feat/x"), None));
    assert!(head_is_fork(None, None));
    assert!(head_is_fork(Some("acme"), Some("acme:main")));
}

// ── #10762: every finding gets an inline anchor or an unanchored reason ──────

/// A diff whose new side is a context line 1, added lines 2..=10, context 11.
const PARTITION_DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,11 @@
 fn ctx() {}
+fn added() {}
+// nit 3
+// nit 4
+// nit 5
+// nit 6
+// nit 7
+// nit 8
+// nit 9
+// nit 10
 fn tail() {}
";

/// A finding on `src/a.rs` at `line` with `effort` and description `desc`.
fn finding(
    line: Option<u32>,
    effort: crate::models::Effort,
    desc: &str,
) -> crate::models::Finding {
    let mut f = crate::models::Finding::new("src/a.rs", "bug", desc, "fix", 0.9, effort);
    f.line = line;
    f
}

/// The #10762 closure fixture: an added-line, a context-line, an off-diff and
/// a no-line finding, eight on-diff nits, and two withheld findings.
fn partition_fixture() -> ReviewResult {
    use crate::models::{Effort, WithheldFinding};
    use crate::pipeline::verify_posted::{NO_VERIFIER_REASON, REFUTED_REASON};

    let mut result = ReviewResult::new("o", "r", 1, "t", "u");
    result.findings = vec![
        finding(Some(2), Effort::Medium, "added"),
        finding(Some(1), Effort::Medium, "context"),
        finding(Some(50), Effort::Medium, "off diff"),
        finding(None, Effort::Medium, "no line"),
    ];
    result
        .findings
        .extend((3..=10).map(|l| finding(Some(l), Effort::Low, &format!("nit {l}"))));
    for reason in [REFUTED_REASON, NO_VERIFIER_REASON] {
        result.withheld_findings.push(WithheldFinding {
            finding: finding(Some(2), Effort::High, "withheld"),
            reason: reason.to_string(),
            missing_fragment: None,
        });
    }
    result
}

/// REGRESSION (#10762): inline + unanchored partition findings + withheld.
///
/// Why: the closure condition of #10762 — no finding is in neither list, and
/// none is in both.
/// What: attaches the closure fixture and asserts every combined index
/// `0..12+2` appears exactly once across the two lists, with the expected
/// reason per unanchored entry, and that the three capped nits are still
/// rendered in the summary body.
/// Test: this test itself.
#[test]
fn partition_invariant_with_fixture() {
    use crate::models::UnanchoredReason;

    let mut result = partition_fixture();
    let total = result.findings.len() + result.withheld_findings.len();
    attach_inline_comments(&mut result, PARTITION_DIFF);

    assert_eq!(result.inline_comments.len() + result.unanchored.len(), total);
    let mut seen: Vec<usize> = result
        .inline_comments
        .iter()
        .map(|c| c.finding_index)
        .chain(result.unanchored.iter().map(|u| u.finding_index))
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, (0..total).collect::<Vec<_>>(), "no index duplicated or missing");

    let inline: Vec<usize> = result
        .inline_comments
        .iter()
        .map(|c| c.finding_index)
        .collect();
    assert_eq!(inline, vec![0, 1, 4, 5, 6, 7, 8], "added, context, first five nits");
    assert_eq!(result.inline_finding_indices, inline);
    let reasons: Vec<(usize, UnanchoredReason)> = result
        .unanchored
        .iter()
        .map(|u| (u.finding_index, u.reason.clone()))
        .collect();
    let withheld = |class: &str| UnanchoredReason::Withheld(class.to_string());
    assert_eq!(
        reasons,
        vec![
            (2, UnanchoredReason::OffDiff),
            (3, UnanchoredReason::NoLine),
            (9, UnanchoredReason::NitCap),
            (10, UnanchoredReason::NitCap),
            (11, UnanchoredReason::NitCap),
            (12, withheld("refuted")),
            (13, withheld("no_verifier")),
        ]
    );

    // The capped nits stay in the summary body, as before #10762.
    assert_eq!(result.suppressed_nits, 3);
    let body = crate::integrations::github::posting::build_review_comment_body(&result);
    for nit in ["nit 8", "nit 9", "nit 10"] {
        assert!(body.contains(&format!("   - {nit}\n")), "{nit} missing: {body}");
    }
}

/// REGRESSION (#10762): an inline entry names its finding and copies its
/// severity and category.
///
/// Why: a consumer posting annotations needs severity and finding identity
/// on the anchor itself. `Finding` has no producing-stage field, so the entry
/// carries no `source`.
/// What: anchors the second of two findings and asserts its entry's
/// `finding_index`, `side`, `category` and `severity` (equal to what
/// `stamp_severities` writes later), and that the JSON has no `source` key.
/// Test: this test itself.
#[test]
fn inline_entry_carries_severity_source_finding_index() {
    use crate::models::{AnchorSide, Effort, FindingCategory, Severity};

    let mut result = ReviewResult::new("o", "r", 1, "t", "u");
    let mut on_diff = finding(Some(2), Effort::High, "conformance");
    on_diff.code_provable = true;
    on_diff.category = FindingCategory::MethodConformance;
    result.findings = vec![finding(Some(50), Effort::Medium, "off diff"), on_diff];
    attach_inline_comments(&mut result, PARTITION_DIFF);

    assert_eq!(result.inline_comments.len(), 1);
    let entry = result.inline_comments[0].clone();
    assert_eq!(entry.finding_index, 1);
    assert_eq!(entry.side, AnchorSide::Right);
    assert_eq!(entry.category, FindingCategory::MethodConformance);
    assert_eq!(entry.severity, Some(Severity::High));
    crate::pipeline::severity::stamp_severities(&mut result);
    assert_eq!(entry.severity, result.findings[1].severity, "matches the stamp");

    let json = serde_json::to_value(&entry).expect("serialize");
    assert_eq!(json["finding_index"], 1);
    assert_eq!(json["side"], "RIGHT");
    assert_eq!(json["severity"], "high");
    assert_eq!(json["category"], "method-conformance");
    assert!(json.get("source").is_none(), "no producing-stage field exists");
}

/// REGRESSION (#10762): withheld findings are partitioned with no survivor.
///
/// Why: `attach_inline_comments` returned early on empty `findings`, so a
/// review whose findings were all withheld reported no `unanchored` entry.
/// What: attaches a result with no findings and two withheld ones, asserting
/// two `withheld:<class>` entries at indices 0 and 1 and no inline entry.
/// Test: this test itself.
#[test]
fn attach_inline_comments_partitions_withheld_findings() {
    use crate::models::UnanchoredReason;

    let mut result = partition_fixture();
    result.findings.clear();
    attach_inline_comments(&mut result, PARTITION_DIFF);

    assert!(result.inline_comments.is_empty());
    let got: Vec<(usize, String)> = result
        .unanchored
        .iter()
        .map(|u| (u.finding_index, String::from(u.reason.clone())))
        .collect();
    assert_eq!(
        got,
        vec![
            (0, "withheld:refuted".to_string()),
            (1, "withheld:no_verifier".to_string()),
        ]
    );
    assert!(matches!(result.unanchored[0].reason, UnanchoredReason::Withheld(_)));
}

/// #10762: the anchor plan does not depend on live or dry-run mode.
///
/// Why: the issue asks for identical `inline_comments` and `unanchored` JSON
/// with and without `--live`.
/// What: attaches the fixture to a dry-run and a live-flagged result and
/// compares the two lists' JSON. This proves only that the plan ignores the
/// mode; the live post path is S3.
/// Test: this test itself.
#[test]
fn inline_and_unanchored_json_identical_live_and_dry() {
    let mut dry = partition_fixture();
    let mut live = partition_fixture();
    live.dry_run = false;
    attach_inline_comments(&mut dry, PARTITION_DIFF);
    attach_inline_comments(&mut live, PARTITION_DIFF);

    let lists = |r: &ReviewResult| {
        serde_json::json!({ "inline": r.inline_comments, "unanchored": r.unanchored })
    };
    assert_eq!(lists(&dry), lists(&live));
    assert!(!dry.unanchored.is_empty() && !dry.inline_comments.is_empty());
}
