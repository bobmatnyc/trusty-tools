//! Issue preview builder for the bug-reporting pipeline.
//!
//! Why: The user must see the exact Markdown that will be filed as a GitHub
//!      issue before they can give informed consent. The preview body IS the
//!      filed body — the same function is called both during `preview_bug_report`
//!      and immediately before `POST /repos/.../issues`. This eliminates any
//!      "what you see is not what gets filed" risk.
//! What: [`build_preview`] takes an [`AggregatedError`] and returns a
//!       [`IssuePreview`] containing the scrubbed title, body, and labels ready
//!       to send to the GitHub API. The body embeds the hidden fingerprint
//!       marker (`<!-- trusty-bug-fingerprint: <fp> -->`) so the dedup search
//!       can find existing issues by fingerprint.
//! Test: `tests::title_format`, `tests::body_contains_fingerprint_marker`,
//!       `tests::crate_label_resolved`, `tests::unknown_crate_no_crate_label`.

use super::scrubber::scrub;
use super::types::AggregatedError;

/// The maximum title length (GitHub enforces 256 chars; we use 200 to leave
/// room for the crate prefix and truncation marker).
const MAX_TITLE_CHARS: usize = 200;

/// The structured output of [`build_preview`].
///
/// Why: carrying title/body/labels as a typed struct lets both the MCP preview
///      tool and the HTTP filing endpoint reuse the same data without
///      re-serializing through JSON multiple times.
/// What: `title` is the GitHub issue title; `body` is the full Markdown body
///       (scrubbed, with fingerprint marker embedded); `labels` is the slice
///       of repo labels to apply (`bug`, `auto-reported`, + optional crate
///       label); `fingerprint` is the SHA-256 hex (64 chars) used for dedup.
/// Test: asserted in every `tests::*` case.
#[derive(Debug, Clone)]
pub struct IssuePreview {
    /// The GitHub issue title, scrubbed and truncated.
    pub title: String,
    /// The full Markdown body including the hidden fingerprint marker.
    pub body: String,
    /// Labels to apply: always includes `bug` and `auto-reported`; may include
    /// a crate-specific label.
    pub labels: Vec<String>,
    /// The SHA-256 fingerprint hex string for dedup lookup.
    pub fingerprint: String,
    /// Scrubbing changes logged during preview construction (shown to user).
    pub scrub_changes: Vec<super::scrubber::ScrubChange>,
}

/// Build an issue preview from an aggregated error.
///
/// Why: both the `preview_bug_report` MCP tool and the actual filing path
///      must produce the same output — calling this once then reusing the
///      result guarantees "preview = what gets filed".
/// What: scrubs the message, fields, and code location; builds the Markdown
///       body; embeds the hidden fingerprint marker; assembles the label list.
/// Test: `tests::title_format`, `tests::body_contains_fingerprint_marker`,
///       `tests::crate_label_resolved`, `tests::scrub_changes_logged`.
#[must_use]
pub fn build_preview(error: &AggregatedError) -> IssuePreview {
    let record = &error.record;

    // Scrub the message and fields.
    let (clean_message, mut scrub_changes) = scrub(&record.message);
    let (clean_fields, field_changes) = scrub(&record.fields);
    scrub_changes.extend(field_changes);

    // Build the title: [crate] clean_message (truncated).
    let raw_title = format!("[{}] {}", record.crate_target, clean_message);
    let title = if raw_title.chars().count() > MAX_TITLE_CHARS {
        let truncated: String = raw_title.chars().take(MAX_TITLE_CHARS - 3).collect();
        format!("{truncated}...")
    } else {
        raw_title
    };

    // Build the code location string.
    let location = match (&record.file, record.line) {
        (Some(f), Some(l)) => {
            let (clean_f, path_changes) = scrub(f);
            scrub_changes.extend(path_changes);
            format!("`{clean_f}:{l}`")
        }
        (Some(f), None) => {
            let (clean_f, path_changes) = scrub(f);
            scrub_changes.extend(path_changes);
            format!("`{clean_f}`")
        }
        _ => "_unknown_".to_string(),
    };

    let fp = &record.fingerprint;

    // Build the Markdown body.
    let body = format!(
        "## Auto-reported error\n\n\
         <!-- trusty-bug-fingerprint: {fp} -->\n\n\
         **Crate**: `{crate_target}` v{version}  \n\
         **OS / Arch**: {os} / {arch}  \n\
         **Occurrences**: {occ}  \n\
         **Location**: {location}  \n\n\
         ### Error message\n\n\
         ```\n\
         {message}\n\
         ```\n\n\
         {fields_section}\
         ---\n\n\
         *Filed automatically by the trusty-mpm bug-capture system.  \n\
         Fingerprint: `{fp}`*\n",
        crate_target = record.crate_target,
        version = record.crate_version,
        os = record.os,
        arch = record.arch,
        occ = error.occurrences,
        message = clean_message,
        fields_section = if clean_fields.is_empty() {
            String::new()
        } else {
            format!("### Additional fields\n\n```\n{clean_fields}\n```\n\n")
        },
    );

    // Resolve the crate label.
    let labels = build_labels(&record.crate_target);

    IssuePreview {
        title,
        body,
        labels,
        fingerprint: fp.clone(),
        scrub_changes,
    }
}

/// Map a `crate_target` (tracing event target, e.g. `trusty_search::indexer`)
/// to the matching repo label.
///
/// Why: issues are triaged by crate; GitHub repo labels use the crate name
///      (hyphens, not underscores). Unknown crates get no crate label —
///      inventing labels would silently fail on the GitHub API.
/// What: strips the module path suffix (everything after the first `::`) and
///       converts underscores to hyphens, then looks the result up in the
///       known-crate table. Returns a label string when found, `None` otherwise.
/// Test: `tests::crate_label_resolved`, `tests::unknown_crate_no_crate_label`.
fn crate_label(crate_target: &str) -> Option<&'static str> {
    // Strip module path (e.g. "trusty_search::indexer" → "trusty_search").
    let crate_name = crate_target.split("::").next().unwrap_or(crate_target);
    // Convert underscores to hyphens.
    let normalized = crate_name.replace('_', "-");
    match normalized.as_str() {
        "trusty-search" => Some("trusty-search"),
        "trusty-memory" => Some("trusty-memory"),
        "trusty-mpm" | "trusty-mpmd" => Some("trusty-mpm"),
        "trusty-common" => Some("trusty-common"),
        "trusty-analyze" => Some("trusty-analyze"),
        "tga" | "trusty-git-analytics" => Some("tga"),
        _ => None,
    }
}

/// Assemble the full label list for an issue.
///
/// Why: every auto-reported issue gets `bug` and `auto-reported`; the optional
///      crate label lets triage filter by component.
/// What: starts with `["bug", "auto-reported"]`; pushes the crate label when
///       [`crate_label`] resolves to `Some`.
/// Test: `tests::crate_label_resolved`, `tests::unknown_crate_no_crate_label`.
fn build_labels(crate_target: &str) -> Vec<String> {
    let mut labels = vec!["bug".to_string(), "auto-reported".to_string()];
    if let Some(label) = crate_label(crate_target) {
        labels.push(label.to_string());
    }
    labels
}

#[cfg(test)]
mod tests {
    use trusty_common::error_capture::CapturedError;

    use super::*;
    use crate::daemon::bug_report::types::AggregatedError;

    fn make_agg(crate_target: &str, message: &str) -> AggregatedError {
        AggregatedError {
            record: CapturedError {
                timestamp_secs: 1_700_000_000,
                crate_target: crate_target.to_string(),
                crate_version: "0.5.0".to_string(),
                message: message.to_string(),
                fields: String::new(),
                file: Some("src/lib.rs".to_string()),
                line: Some(42),
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
                fingerprint: "a".repeat(64),
            },
            occurrences: 3,
        }
    }

    #[test]
    fn title_format() {
        let agg = make_agg("trusty_search::indexer", "index open failed");
        let preview = build_preview(&agg);
        assert!(
            preview.title.starts_with("[trusty_search::indexer]"),
            "title: {}",
            preview.title
        );
        assert!(
            preview.title.contains("index open failed"),
            "{}",
            preview.title
        );
    }

    #[test]
    fn body_contains_fingerprint_marker() {
        let agg = make_agg("trusty_mpm", "something broke");
        let preview = build_preview(&agg);
        let marker = format!("<!-- trusty-bug-fingerprint: {} -->", "a".repeat(64));
        assert!(
            preview.body.contains(&marker),
            "body missing fingerprint marker:\n{}",
            preview.body
        );
    }

    #[test]
    fn crate_label_resolved() {
        // trusty_search::indexer → "trusty-search" label.
        let labels = build_labels("trusty_search::indexer");
        assert!(labels.contains(&"trusty-search".to_string()), "{labels:?}");
        assert!(labels.contains(&"bug".to_string()));
        assert!(labels.contains(&"auto-reported".to_string()));
    }

    #[test]
    fn unknown_crate_no_crate_label() {
        let labels = build_labels("some_unknown_crate");
        assert_eq!(labels, vec!["bug", "auto-reported"], "{labels:?}");
    }

    #[test]
    fn scrub_changes_logged_for_path_in_message() {
        let mut agg = make_agg("trusty_memory", "failed to open /Users/alice/db");
        agg.record.message = "failed to open /Users/alice/db".to_string();
        let preview = build_preview(&agg);
        assert!(
            preview
                .scrub_changes
                .iter()
                .any(|c| c.pattern == "AbsolutePath"),
            "expected path scrub: {:?}",
            preview.scrub_changes
        );
        assert!(
            !preview.body.contains("/Users/alice"),
            "path must be scrubbed from body"
        );
    }

    #[test]
    fn long_title_truncated() {
        let long_msg = "x".repeat(300);
        let agg = make_agg("trusty_mpm", &long_msg);
        let preview = build_preview(&agg);
        assert!(
            preview.title.chars().count() <= MAX_TITLE_CHARS,
            "title too long: {} chars",
            preview.title.chars().count()
        );
        assert!(
            preview.title.ends_with("..."),
            "expected ellipsis: {}",
            preview.title
        );
    }

    #[test]
    fn fingerprint_in_preview() {
        let agg = make_agg("trusty_search", "err");
        let preview = build_preview(&agg);
        assert_eq!(preview.fingerprint, "a".repeat(64));
    }

    #[test]
    fn occurrence_count_in_body() {
        let agg = make_agg("trusty_mpm", "boom");
        let preview = build_preview(&agg);
        assert!(
            preview.body.contains("3"),
            "occurrences not in body: {}",
            preview.body
        );
    }
}
