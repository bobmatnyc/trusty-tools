//! Tests for the DD-manifest adapter (#5236, DOC-67 §6).
//!
//! Why: the adapter is pure precisely so its three obligations can be proved
//! here rather than by running an audit — the §6 field mapping, §9's byte-level
//! determinism, and the hard requirement that no configured credential reaches
//! an artifact handed to a third party.
//! What: drives `build_dd_manifest` against fixture `Config` values.
//! Test: this file.

use std::path::PathBuf;

use super::*;
use crate::core::config::{GithubConfig, JiraConfig, RepositoryConfig};

/// A config naming `n` repositories, with no credentials.
fn config_with_repos(entries: &[(&str, Option<&str>)]) -> Config {
    Config {
        repositories: entries
            .iter()
            .map(|(path, name)| RepositoryConfig {
                path: PathBuf::from(path),
                name: name.map(str::to_string),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn options(title: &str) -> DdManifestOptions {
    DdManifestOptions {
        title: title.to_string(),
        ..Default::default()
    }
}

/// Why: §6's table is the contract between two processes; every row it fixes
/// must actually appear, and every field it says to omit must stay absent so
/// trusty-review's own defaults (slug derivation, git_ref, live `--analyze`)
/// apply.
/// Test: itself.
#[test]
fn maps_engagement_metadata() {
    let cfg = config_with_repos(&[("/src/northwind-web", Some("Northwind Web"))]);
    let opts = DdManifestOptions {
        title: "Acme — Technical Due Diligence".to_string(),
        analyst: Some("J. Reviewer".to_string()),
        client: Some("Acme Holdings".to_string()),
        gaps: vec!["Stage `dora` did not complete.".to_string()],
        ..Default::default()
    };

    let manifest = build_dd_manifest(&cfg, &opts).expect("builds");

    assert_eq!(manifest.report.title, "Acme — Technical Due Diligence");
    assert_eq!(manifest.report.analyst.as_deref(), Some("J. Reviewer"));
    assert_eq!(manifest.report.client.as_deref(), Some("Acme Holdings"));
    assert_eq!(manifest.report.gaps.len(), 1);
    assert_eq!(
        manifest.repositories,
        vec![DdRepositoryEntry {
            name: "Northwind Web".to_string(),
            path: PathBuf::from("/src/northwind-web"),
        }]
    );

    // Omitted-by-design keys never appear: `slug` is trusty-review's to derive,
    // `ref` follows HEAD, and a declared `metrics` file would block the live
    // `--analyze` fetch every AUDIT run depends on (§6).
    let toml = manifest.to_toml().expect("serializes");
    for absent in ["slug", "ref =", "metrics", "template"] {
        assert!(!toml.contains(absent), "{absent} must not appear:\n{toml}");
    }
}

/// Why: absent metadata must stay absent rather than becoming an empty string —
/// the template's own fallback ("not stated in source report") is what §2's
/// no-interactivity rule relies on.
/// Test: itself.
#[test]
fn absent_metadata_is_omitted_not_blanked() {
    let cfg = config_with_repos(&[("/src/a", Some("A"))]);
    let toml = build_dd_manifest(&cfg, &options("T"))
        .expect("builds")
        .to_toml()
        .expect("serializes");

    assert!(!toml.contains("analyst"), "{toml}");
    assert!(!toml.contains("client"), "{toml}");
    assert!(!toml.contains("gaps"), "{toml}");
}

/// Why: `RepositoryConfig.name` is optional and its documented fallback is the
/// directory basename; an audit of an org-discovered repo set relies on it.
/// Test: itself.
#[test]
fn names_fall_back_to_the_directory_basename() {
    let cfg = config_with_repos(&[
        ("/src/northwind-web", None),
        ("/src/billing", Some("  ")),
        ("/src/ledger", Some("Ledger Service")),
    ]);

    let manifest = build_dd_manifest(&cfg, &options("T")).expect("builds");
    let names: Vec<&str> = manifest
        .repositories
        .iter()
        .map(|r| r.name.as_str())
        .collect();

    assert_eq!(names, vec!["northwind-web", "billing", "Ledger Service"]);
}

/// Why: repository order is the report's application order; a set that reorders
/// between runs would make two audits of the same state incomparable.
/// Test: itself.
#[test]
fn repositories_keep_config_order() {
    let cfg = config_with_repos(&[("/src/c", None), ("/src/a", None), ("/src/b", None)]);
    let manifest = build_dd_manifest(&cfg, &options("T")).expect("builds");
    let names: Vec<&str> = manifest
        .repositories
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(names, vec!["c", "a", "b"], "config order, never sorted");
}

/// Why: DOC-67 §9 — the same DB and config must produce a byte-identical
/// manifest. A clock read, an environment read, or a hash-ordered collection in
/// the builder would break this and nothing else would catch it.
/// Test: itself.
#[test]
fn two_builds_are_byte_identical() {
    let cfg = config_with_repos(&[
        ("/src/northwind-web", Some("Northwind Web")),
        ("/src/billing", None),
    ]);
    let opts = DdManifestOptions {
        title: "Acme — Technical Due Diligence".to_string(),
        analyst: Some("J. Reviewer".to_string()),
        client: None,
        gaps: vec!["a".to_string(), "b".to_string()],
        ..Default::default()
    };

    let first = build_dd_manifest(&cfg, &opts).expect("builds");
    let second = build_dd_manifest(&cfg, &opts).expect("builds");

    assert_eq!(first, second);
    assert_eq!(
        first.to_toml().expect("serializes"),
        second.to_toml().expect("serializes"),
        "same input, same bytes"
    );
}

/// Why: the manifest is handed to a third party. A credential reaching it is
/// the one failure in this module that cannot be walked back, so the guarantee
/// is tested at the boundary a token can realistically cross — an expanded
/// `${GITHUB_TOKEN}` echoed into a stage's error message, and from there into a
/// gap line.
/// Test: itself.
#[test]
fn configured_token_never_reaches_the_manifest() {
    let token = "ghp_TESTONLYtoken0123456789abcdef"; // pragma: allowlist secret
    let mut cfg = config_with_repos(&[("/src/a", Some("A"))]);
    cfg.github = Some(GithubConfig {
        token: Some(token.to_string()),
        ..Default::default()
    });

    // Every string channel that reaches the file, each carrying the token the
    // way a real failure would: an API error body quoted into a stage message.
    let opts = DdManifestOptions {
        title: format!("Acme ({token})"),
        analyst: Some(format!("analyst {token}")),
        client: Some(format!("client {token}")),
        gaps: vec![format!(
            "Stage `collect` did not complete: GET /orgs/acme/repos returned 401 for {token}"
        )],
        ..Default::default()
    };

    let toml = build_dd_manifest(&cfg, &opts)
        .expect("builds")
        .to_toml()
        .expect("serializes");

    assert!(
        !toml.contains(token),
        "token leaked into the manifest:\n{toml}"
    );
    assert!(
        toml.contains("[REDACTED]"),
        "the value must be replaced, not merely dropped:\n{toml}"
    );
    // The surrounding diagnostic survives — scrubbing must not destroy the gap.
    assert!(toml.contains("Stage `collect` did not complete"), "{toml}");
}

// ---------------------------------------------------------------------------
// The real sweep → gap-line → manifest path, with a credential positioned
// across the excerpt boundary (#5239)
// ---------------------------------------------------------------------------

/// The excerpt cap these tests position a credential across, taken from the
/// production constant rather than copied.
///
/// Why: a copy that drifted would not fail — the token would land wholly inside
/// a wider excerpt, the straddle would stop straddling, and
/// `a_token_straddling_the_excerpt_boundary_leaves_no_fragment` would keep
/// passing while guarding nothing (#5308 review).
const EXCERPT_BOUNDARY: usize = crate::audit::MAX_REASON_CHARS;

/// Run one failed stage through the pipeline `tga audit` actually uses.
///
/// `configured_token_never_reaches_the_manifest` hands `build_dd_manifest`
/// short gap strings it wrote itself, which is why it cannot see this class of
/// bug: the excerpt never runs. This helper drives the real
/// `sweep_gap_lines` → `build_dd_manifest` path instead.
fn manifest_from_stage_failure(cfg: &Config, message: String) -> String {
    let mut stats = crate::audit::AuditSweepStats::default();
    stats.record(
        crate::audit::SweepStage::Collect,
        std::time::Instant::now(),
        Err(anyhow::anyhow!(message)),
    );

    let secrets = configured_secrets(cfg);
    let opts = DdManifestOptions {
        title: "Acme — Technical Due Diligence".to_string(),
        gaps: crate::audit::sweep_gap_lines(&stats, &secrets),
        ..Default::default()
    };
    build_dd_manifest(cfg, &opts)
        .expect("builds")
        .to_toml()
        .expect("serializes")
}

/// A config whose GitHub and JIRA tokens are the two given values.
fn config_with_tokens(github: &str, jira: &str) -> Config {
    let mut cfg = config_with_repos(&[("/src/a", Some("A"))]);
    cfg.github = Some(GithubConfig {
        token: Some(github.to_string()),
        ..Default::default()
    });
    cfg.jira = Some(JiraConfig {
        token: Some(jira.to_string()),
        ..Default::default()
    });
    cfg
}

/// `scrub_secrets`'s shortest scrubbable needle, in characters.
///
/// Why: it is private to `trusty_common::credentials::redact`, and exporting it
/// would add a public item to a crate every other crate depends on for the sake
/// of one assertion here. `scrub_min_chars_is_what_the_fragment_assertions_assume`
/// pins this value against the real behaviour instead, so a change over there
/// fails a test rather than silently weakening every assertion below (#5308
/// review).
const SCRUB_MIN_CHARS: usize = 8;

/// The shortest leading fragment of `token` that no later `scrub_secrets` call
/// could remove — measured in characters, because `scrub_secrets`'s own guard
/// counts characters and a byte slice would panic mid-codepoint.
fn unscrubbable_fragment(token: &str) -> String {
    token.chars().take(SCRUB_MIN_CHARS).collect()
}

/// Why: [`SCRUB_MIN_CHARS`] is a restatement of a constant in another crate, and
/// the fragment assertions are only as strong as that number is right. A needle
/// of exactly this length must be scrubbable and one character shorter must not
/// be — which is what makes a surviving fragment of this length unremovable by
/// anything downstream.
/// Test: itself.
#[test]
fn scrub_min_chars_is_what_the_fragment_assertions_assume() {
    let needle: String = "abcdefghijklmnop".chars().take(SCRUB_MIN_CHARS).collect();
    assert_eq!(
        trusty_common::credentials::scrub_secrets(&format!("value {needle} here"), &[&needle]),
        "value [REDACTED] here",
        "a needle of exactly SCRUB_MIN_CHARS must be scrubbable"
    );

    let shorter: String = needle.chars().take(SCRUB_MIN_CHARS - 1).collect();
    let text = format!("value {shorter} here");
    assert_eq!(
        trusty_common::credentials::scrub_secrets(&text, &[&shorter]),
        text,
        "one character shorter must be refused"
    );
}

/// Assert that not even an unscrubbable prefix of `token` appears in `toml`.
///
/// Any longer fragment contains [`unscrubbable_fragment`]'s, so one assertion
/// covers every fragment length.
fn assert_no_fragment(toml: &str, token: &str, label: &str) {
    assert!(!toml.contains(token), "{label}: full token leaked:\n{toml}");
    assert!(
        !toml.contains(&unscrubbable_fragment(token)),
        "{label}: a prefix fragment survived:\n{toml}"
    );
}

/// Why: `scrub_secrets` matches a credential's whole value, so truncating a
/// stage message before scrubbing it leaves a token that spans the boundary
/// behind as a prefix no later scrub can match — and the manifest is handed to a
/// third party. Found by security review of #5239's gap reporting.
/// Test: itself.
#[test]
fn a_token_straddling_the_excerpt_boundary_leaves_no_fragment() {
    let token = "ghp_STRADDLE0123456789abcdefGHIJKLMNOPQR"; // pragma: allowlist secret
    let lead = "GET /orgs/acme/repos returned 401 for ";
    // Start the token 10 chars short of the cut, so it begins inside the excerpt
    // and ends outside it — the exact geometry that produced the leak.
    let pad = "x".repeat(EXCERPT_BOUNDARY - 10 - lead.chars().count());
    let message = format!(
        "{lead}{pad}{token}: bad credentials. The remainder of this cause chain exists to \
         carry the message past 200 characters so the excerpt is guaranteed to truncate."
    );
    assert!(message.chars().count() > 200, "the excerpt must truncate");

    let cfg = config_with_tokens(token, "unused-jira-token-000");
    let toml = manifest_from_stage_failure(&cfg, message);

    assert_no_fragment(&toml, token, "straddling token");
    // Scrubbing must remove the value, not the diagnostic around it.
    assert!(toml.contains("[REDACTED]"), "{toml}");
    assert!(toml.contains("not assessed"), "{toml}");
}

/// Why: the opposite geometry, and a distinct path. A token sitting entirely
/// beyond the boundary was safe before this fix only by accident — truncation
/// dropped it. Redacting first shortens the text, so text that used to fall
/// outside the excerpt now moves inside it; a second credential must survive
/// that shift as `[REDACTED]`, not as itself.
/// Test: itself.
#[test]
fn a_token_beyond_the_boundary_survives_the_shift_scrubbing_causes() {
    // `early` sits near the start; `late` sits past char 160 in the raw text but
    // moves under it once `early` collapses to `[REDACTED]`.
    let early = "ghp_EARLY0123456789abcdefGHIJKLMNOPQRSTUV"; // pragma: allowlist secret
    let late = "jira_LATE0123456789abcdefGHIJKLMNOPQRSTUV"; // pragma: allowlist secret
    let message = format!(
        "GET /rest/api/3/search failed for {early}; retried with {} and got 401 for {late}, \
         then gave up.",
        "y".repeat(EXCERPT_BOUNDARY)
    );

    let cfg = config_with_tokens(early, late);
    let toml = manifest_from_stage_failure(&cfg, message);

    assert_no_fragment(&toml, early, "early token");
    assert_no_fragment(&toml, late, "late token");
}

/// Why: `[REDACTED]` is longer than most of what it replaces, so scrubbing
/// before truncating could push the excerpt over its budget if the cap were
/// applied to the pre-redaction text. The cap must bind the string that is
/// actually emitted.
/// Test: itself.
#[test]
fn redaction_before_truncation_keeps_the_excerpt_within_budget() {
    // Twelve occurrences, each expanding to `[REDACTED]`, in a message far
    // longer than the cap.
    let token = "ghp_BUDGET0123456789abcdefGHIJKLMNOPQRST"; // pragma: allowlist secret
    let message = std::iter::repeat_n(format!("401 for {token}"), 12)
        .collect::<Vec<_>>()
        .join(" and ");

    let cfg = config_with_tokens(token, "unused-jira-token-000");
    let mut stats = crate::audit::AuditSweepStats::default();
    stats.record(
        crate::audit::SweepStage::Collect,
        std::time::Instant::now(),
        Err(anyhow::anyhow!(message)),
    );
    let line = crate::audit::sweep_gap_lines(&stats, &configured_secrets(&cfg)).remove(0);

    assert!(line.contains('…'), "must still truncate: {line}");
    assert!(
        line.chars().count() < 400,
        "one verbose error must not dominate the Gaps section ({} chars)",
        line.chars().count()
    );
    assert!(!line.contains(&unscrubbable_fragment(token)), "{line}");
}

/// Why: trusty-review rejects a manifest with no repositories, and a message
/// naming `config.yaml` is far more actionable than that rejection.
/// Test: itself.
#[test]
fn empty_config_is_an_actionable_error() {
    let err = build_dd_manifest(&Config::default(), &options("T")).expect_err("no repositories");
    assert!(matches!(err, DdManifestError::NoRepositories));
    assert!(err.to_string().contains("config.yaml"), "{err}");
}

/// Why: the file is only useful if the consumer parses it, and the consumer is
/// a different crate with its own validation. Serializing text that
/// trusty-review would reject is a defect this crate can catch alone.
/// Test: itself.
#[test]
fn round_trips_through_the_review_schema() {
    let cfg = config_with_repos(&[("/src/a", Some("A")), ("/src/b", None)]);
    let opts = DdManifestOptions {
        title: "T".to_string(),
        analyst: Some("An".to_string()),
        client: None,
        gaps: vec!["one gap".to_string()],
        ..Default::default()
    };
    let toml = build_dd_manifest(&cfg, &opts)
        .expect("builds")
        .to_toml()
        .expect("serializes");

    // The shape trusty-review's `load_manifest` requires: a `[report]` table
    // with a title, and one `[[repositories]]` entry per repo carrying exactly
    // one source key (`path`).
    let parsed: toml::Value = toml::from_str(&toml).expect("valid TOML");
    assert_eq!(parsed["report"]["title"].as_str(), Some("T"));
    assert_eq!(parsed["report"]["gaps"].as_array().map(Vec::len), Some(1));
    let repos = parsed["repositories"].as_array().expect("array of tables");
    assert_eq!(repos.len(), 2);
    for repo in repos {
        assert!(repo.get("path").is_some(), "local source key required");
        assert!(repo.get("remote").is_none(), "exactly one source key");
        assert!(repo.get("name").is_some());
    }
}

/// Why: tga resolves a relative repository path against its working directory
/// and trusty-review resolves one against the manifest's directory — and the
/// manifest lands in the output directory, somewhere else entirely. Copying the
/// path through verbatim silently points the renderer at a checkout that does
/// not exist, and the only symptom is a report with no analysis and no stated
/// reason. Caught by the end-to-end smoke run for #5238.
/// Test: itself.
#[test]
fn relative_paths_are_anchored_to_base_dir() {
    let cfg = config_with_repos(&[("./small-repo", Some("Small")), ("/abs/other", Some("Abs"))]);
    let opts = DdManifestOptions {
        title: "T".to_string(),
        base_dir: PathBuf::from("/work/audit"),
        ..Default::default()
    };

    let manifest = build_dd_manifest(&cfg, &opts).expect("builds");

    assert_eq!(
        manifest.repositories[0].path,
        PathBuf::from("/work/audit/./small-repo"),
        "a relative path is anchored, never passed through"
    );
    assert_eq!(
        manifest.repositories[1].path,
        PathBuf::from("/abs/other"),
        "an absolute path is untouched"
    );

    // An empty base leaves paths exactly as configured.
    let bare = build_dd_manifest(&cfg, &options("T")).expect("builds");
    assert_eq!(bare.repositories[0].path, PathBuf::from("./small-repo"));
}
