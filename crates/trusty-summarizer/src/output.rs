//! How the CLI prints one `summarize` record.
//!
//! Why: the library's [`FileRecord`] is the full result; the CLI may cap the
//! outline (B6: a 174-heading outline cost 5,950 tokens before any answer)
//! without changing what the library, the cache or the warm server hold.
//!
//! What: [`render`] serializes a record with the same field order as
//! [`FileRecord`]'s own serialization. With [`RenderOptions::max_outline`]
//! set and exceeded, the outline keeps its first entries and an
//! `outline_omitted` object, right after `outline`, gives the count of the
//! dropped entries and, with references on, the line range they cover.
//!
//! B5: `cache` and `stages` differ between a fresh and a cached run of the
//! same file (cache status, wall times), so by default they are left out
//! and the same file under the same settings prints the same bytes.
//! [`RenderOptions::stats`] (`--stats`) prints them.

use serde::Serialize;

use crate::record::{
    BackendsUsed, CacheStatus, ClassResult, ErrorInfo, FileRecord, OutlineEntry, StageStats,
};
use crate::text::Span;

/// What the CLI prints of a record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderOptions {
    /// Print at most this many outline entries; `None` prints all.
    pub max_outline: Option<usize>,
    /// Print the per-run `cache` and `stages` fields (B5).
    pub stats: bool,
}

/// The outline entries a cap dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OutlineOmitted {
    /// How many entries were dropped.
    pub entries: usize,
    /// The lines the dropped entries cover, `[start, end]`; absent with
    /// references off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<Span>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum View<'a> {
    Ok(OkView<'a>),
    Error(ErrorView<'a>),
}

#[derive(Serialize)]
struct OkView<'a> {
    path: &'a str,
    hash: &'a str,
    bytes: u64,
    lines: usize,
    text_chars: usize,
    tokens_est: usize,
    format: &'a str,
    class: &'a ClassResult,
    summary: &'a str,
    outline: &'a [OutlineEntry],
    #[serde(skip_serializing_if = "Option::is_none")]
    outline_omitted: Option<OutlineOmitted>,
    backends: &'a BackendsUsed,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache: Option<CacheStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stages: Option<&'a [StageStats]>,
}

#[derive(Serialize)]
struct ErrorView<'a> {
    path: &'a str,
    error: &'a ErrorInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    stages: Option<&'a [StageStats]>,
}

/// Keeps the first `max` entries of `outline`; describes the rest.
///
/// What: returns `outline` whole and `None` when it has at most `max`
/// entries; otherwise its first `max` entries and the dropped count, with
/// the dropped entries' line range when they carry ranges.
/// Test: `outline_cap_keeps_first_entries_and_reports_the_rest`
pub fn cap_outline(
    outline: &[OutlineEntry],
    max: usize,
) -> (&[OutlineEntry], Option<OutlineOmitted>) {
    if outline.len() <= max {
        return (outline, None);
    }
    let dropped = &outline[max..];
    let lines = match (
        dropped.first().and_then(|e| e.lines),
        dropped.last().and_then(|e| e.lines),
    ) {
        (Some(a), Some(b)) => Some(Span::new(a.start, b.end)),
        _ => None,
    };
    (
        &outline[..max],
        Some(OutlineOmitted {
            entries: dropped.len(),
            lines,
        }),
    )
}

/// One JSON line (no newline) for `record`.
///
/// Why: see the module doc.
/// What: with `stats` and no cap, the same bytes as
/// `serde_json::to_string(record)`; without `stats`, those bytes less
/// `cache` and `stages`.
/// Test: `stats_render_matches_the_record`,
/// `default_render_is_the_same_for_fresh_and_cached_records`,
/// `outline_cap_keeps_first_entries_and_reports_the_rest`
pub fn render(record: &FileRecord, opts: &RenderOptions) -> serde_json::Result<String> {
    let view = match record {
        FileRecord::Ok(r) => {
            let (outline, outline_omitted) = match opts.max_outline {
                Some(max) => cap_outline(&r.outline, max),
                None => (r.outline.as_slice(), None),
            };
            View::Ok(OkView {
                path: &r.path,
                hash: &r.hash,
                bytes: r.bytes,
                lines: r.lines,
                text_chars: r.text_chars,
                tokens_est: r.tokens_est,
                format: &r.format,
                class: &r.class,
                summary: &r.summary,
                outline,
                outline_omitted,
                backends: &r.backends,
                cache: opts.stats.then_some(r.cache),
                stages: opts.stats.then_some(r.stages.as_slice()),
            })
        }
        FileRecord::Error(r) => View::Error(ErrorView {
            path: &r.path,
            error: &r.error,
            stages: opts.stats.then_some(r.stages.as_slice()),
        }),
    };
    serde_json::to_string(&view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{ErrorRecord, Stage, SummaryRecord};
    use std::time::Duration;

    fn entry(i: usize) -> OutlineEntry {
        OutlineEntry {
            title: format!("Part {i}"),
            lines: Some(Span::new(i * 10 + 1, i * 10 + 10)),
            paragraphs: Some(Span::new(i + 1, i + 1)),
            terms: Vec::new(),
        }
    }

    fn record(n: usize) -> FileRecord {
        FileRecord::Ok(SummaryRecord {
            path: "a.md".into(),
            hash: "sha256:00".into(),
            bytes: 10,
            lines: n * 10,
            text_chars: 9,
            tokens_est: 3,
            format: "markdown".into(),
            class: ClassResult::NoClassifierConfigured,
            summary: "S.".into(),
            outline: (0..n).map(entry).collect(),
            backends: BackendsUsed {
                converter: "direct".into(),
                summary: "extractive-tf".into(),
                classifier: "none".into(),
            },
            cache: CacheStatus::Miss,
            stages: vec![StageStats::timed(Stage::Read, Duration::from_micros(1500))],
        })
    }

    #[test]
    fn stats_render_matches_the_record() {
        let stats = RenderOptions {
            stats: true,
            ..RenderOptions::default()
        };
        let r = record(3);
        assert_eq!(
            render(&r, &stats).unwrap(),
            serde_json::to_string(&r).unwrap()
        );
        let e = FileRecord::Error(ErrorRecord {
            path: "b.pdf".into(),
            error: ErrorInfo {
                kind: "io".into(),
                message: "gone".into(),
                format: None,
            },
            stages: Vec::new(),
        });
        assert_eq!(
            render(&e, &stats).unwrap(),
            serde_json::to_string(&e).unwrap()
        );
    }

    #[test]
    fn default_render_is_the_same_for_fresh_and_cached_records() {
        let fresh = record(3);
        let FileRecord::Ok(mut cached) = fresh.clone() else {
            unreachable!()
        };
        cached.cache = CacheStatus::Hit;
        cached.stages = vec![StageStats::timed(Stage::Cache, Duration::from_micros(7))];
        let cached = FileRecord::Ok(cached);
        let opts = RenderOptions::default();
        let a = render(&fresh, &opts).unwrap();
        assert_eq!(a, render(&cached, &opts).unwrap());
        assert!(!a.contains("\"stages\"") && !a.contains("\"cache\""));
    }

    #[test]
    fn outline_cap_keeps_first_entries_and_reports_the_rest() {
        let r = record(5);
        let opts = RenderOptions {
            max_outline: Some(2),
            ..RenderOptions::default()
        };
        let v: serde_json::Value = serde_json::from_str(&render(&r, &opts).unwrap()).unwrap();
        assert_eq!(v["outline"].as_array().unwrap().len(), 2);
        assert_eq!(v["outline"][1]["title"], "Part 1");
        assert_eq!(
            v["outline_omitted"],
            serde_json::json!({"entries": 3, "lines": [21, 50]})
        );
        // Under the cap: nothing omitted, no field.
        let opts = RenderOptions {
            max_outline: Some(5),
            ..RenderOptions::default()
        };
        let v: serde_json::Value = serde_json::from_str(&render(&r, &opts).unwrap()).unwrap();
        assert!(v.get("outline_omitted").is_none());
        // References off: the count alone.
        let bare: Vec<OutlineEntry> = (0..3)
            .map(|i| OutlineEntry {
                lines: None,
                paragraphs: None,
                ..entry(i)
            })
            .collect();
        let (kept, omitted) = cap_outline(&bare, 0);
        assert!(kept.is_empty());
        assert_eq!(
            omitted,
            Some(OutlineOmitted {
                entries: 3,
                lines: None
            })
        );
    }
}
