//! Per-drawer injection-frequency index over the enriched-prompt hook logs.
//!
//! Why: ADR-0028's Migration step 3 ranks backfill candidates by **injection
//! frequency**, because that is the cost the design actually cares about — a
//! stale drawer nobody retrieves is free, while the ADR's motivating case (§C7)
//! is a 19-day-old session checkpoint reaching 44.8% of turns. Nothing in the
//! drawer store records that number: `access_count` is 0 on every drawer in the
//! estate (§C5), so the store cannot answer "how often was this injected". The
//! only surviving record is the hook log
//! (`<data_root>/logs/enriched-prompts.<date>.jsonl`), which stores the rendered
//! injection text but **no drawer id**. This module recovers the join.
//!
//! What: the injection renders each drawer as one bullet whose body is exactly
//! [`crate::commands::prompt_context::format::drawer_preview`] of its content —
//! a deterministic, whitespace-collapsed 220-char truncation. Parsing those
//! bullets back out and counting distinct injections per bullet body therefore
//! yields, for any drawer, the number of turns its preview reached. Counting is
//! per *entry*, not per bullet: an injection that lists the same drawer under
//! two palace sections still reached one turn.
//!
//! Two details make the recovered count exact rather than approximate:
//!   - Bullets are read only inside a `## Relevant memories` section, so the
//!     `## Relevant KG facts` section's `- ` lines cannot contaminate the index.
//!   - `compose_injection` truncates the whole block at a 4 KiB cap, which can
//!     cut the final bullet mid-preview. Those tails are indexed separately and
//!     matched by prefix, so a drawer that reached a turn but got cut is still
//!     counted. They are rare — 4 of 8,216 `trusty-tools` injections in the live
//!     estate — but dropping them would silently under-count.
//!
//! Test: `bullets_are_counted_once_per_entry`, `kg_facts_section_is_ignored`,
//! `truncated_tail_bullet_matches_by_prefix`,
//! `short_partial_is_not_matched`, `untagged_bullet_counts_exactly`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::prompt_log::PromptLogEntry;

/// Section header introducing the drawer bullets. Matched as a prefix because
/// the header carries the palace slug (`## Relevant memories from palace \`x\``).
const DRAWER_SECTION_PREFIX: &str = "## Relevant memories";
/// Separator `format::compose_injection` writes between the preview and tags.
const TAG_SUFFIX_OPEN: &str = "  _(tags: ";
/// Closing marker of the tag run.
const TAG_SUFFIX_CLOSE: &str = ")_";
/// Only `prompt-context` entries carry drawer bullets.
const DRAWER_INJECTION_KIND: &str = "prompt-context-facts";
/// Filename stem of the rolling hook log.
const LOG_PREFIX: &str = "enriched-prompts.";
/// Filename suffix of the rolling hook log.
const LOG_SUFFIX: &str = ".jsonl";
/// Marker `compose_injection` appends where the byte cap cut the block.
const TRUNCATION_MARKER: char = '…';

/// Shortest byte-cap-truncated tail that may be prefix-matched against a
/// drawer preview.
///
/// Why: a partial is matched with `preview.starts_with(partial)`, so a very
/// short partial could match several unrelated drawers and inflate all of
/// them. Requiring a substantial prefix makes a false attribution require two
/// drawers that agree on their first 60 characters — at which point the human
/// reading the report sees two near-identical excerpts and can tell.
const MIN_PARTIAL_CHARS: usize = 60;

/// What a scan actually read, so the report can state its own coverage.
///
/// Why: an empty or short log window silently produces zeros, which reads
/// identically to "this drawer is never injected" — the opposite conclusion.
/// Surfacing the window and entry count lets the report say which it is.
#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    /// Log files successfully read.
    pub files_scanned: usize,
    /// Log files that could not be opened or read.
    pub files_failed: usize,
    /// JSONL lines parsed into an entry.
    pub entries_read: u64,
    /// Entries that were `prompt-context-facts` and thus counted.
    pub injections_counted: u64,
    /// Earliest counted entry timestamp.
    pub earliest: Option<DateTime<Utc>>,
    /// Latest counted entry timestamp.
    pub latest: Option<DateTime<Utc>>,
}

/// Palace slug plus a rendered drawer preview.
type PreviewKey = (String, String);

/// Injection counts recovered from the hook logs.
#[derive(Debug, Default)]
pub struct InjectionIndex {
    /// Total `prompt-context` injections seen per palace — the denominator for
    /// the "% of turns" column.
    totals: HashMap<String, u64>,
    /// Entries containing a complete rendering of this preview.
    exact: HashMap<PreviewKey, u64>,
    /// Entries whose final bullet was cut by the byte cap, keyed by the surviving
    /// prefix.
    partial: HashMap<PreviewKey, u64>,
    /// Coverage of the scan that produced this index.
    pub stats: ScanStats,
}

impl InjectionIndex {
    /// Scan every `enriched-prompts.*.jsonl` under `dir`.
    ///
    /// Why: the logs roll daily and on a size cap, so the corpus is a directory
    /// of files rather than one stream. A missing directory is not an error —
    /// it means logging was never enabled — and returns an empty index whose
    /// `stats.files_scanned == 0` tells the caller to say so rather than
    /// reporting a estate-wide zero.
    /// What: reads each matching file line by line, deserialises each line as a
    /// [`PromptLogEntry`], and ingests the `prompt-context` ones. A single
    /// unreadable file or unparseable line is skipped, not fatal — a partially
    /// corrupt log still carries usable frequency signal, and refusing to
    /// report at all would be worse.
    /// Test: `scan_dir_on_missing_directory_is_empty_not_error`,
    /// `scan_dir_counts_across_files`.
    pub fn scan_dir(dir: &Path) -> Result<Self> {
        let mut index = Self::default();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(index);
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(LOG_PREFIX) && n.ends_with(LOG_SUFFIX))
            })
            .collect();
        paths.sort();
        for path in paths {
            match std::fs::read_to_string(&path) {
                Ok(body) => {
                    index.stats.files_scanned += 1;
                    index.ingest_file(&body);
                }
                Err(e) => {
                    index.stats.files_failed += 1;
                    tracing::warn!(path = %path.display(), "read prompt log failed: {e:#}");
                }
            }
        }
        Ok(index)
    }

    /// Ingest one log file's contents.
    fn ingest_file(&mut self, body: &str) {
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<PromptLogEntry>(line) else {
                continue;
            };
            self.stats.entries_read += 1;
            if entry.injection_kind != DRAWER_INJECTION_KIND {
                continue;
            }
            self.ingest_entry(&entry);
        }
    }

    /// Count one `prompt-context` injection.
    ///
    /// Why: the unit the report reasons about is a *turn*, so a drawer listed
    /// twice in one injection must add one, not two.
    /// What: collects this entry's distinct preview bodies, then increments each
    /// once. Also advances the palace total and the observed time window.
    /// Test: `bullets_are_counted_once_per_entry`.
    fn ingest_entry(&mut self, entry: &PromptLogEntry) {
        self.stats.injections_counted += 1;
        *self.totals.entry(entry.palace.clone()).or_default() += 1;
        let ts = entry.timestamp;
        self.stats.earliest = Some(self.stats.earliest.map_or(ts, |e| e.min(ts)));
        self.stats.latest = Some(self.stats.latest.map_or(ts, |l| l.max(ts)));

        let (exact, partial) = parse_drawer_bullets(&entry.injection);
        for body in exact {
            *self.exact.entry((entry.palace.clone(), body)).or_default() += 1;
        }
        for body in partial {
            *self
                .partial
                .entry((entry.palace.clone(), body))
                .or_default() += 1;
        }
    }

    /// Injections observed for `palace` — the denominator for "% of turns".
    pub fn total_injections(&self, palace: &str) -> u64 {
        self.totals.get(palace).copied().unwrap_or(0)
    }

    /// Turns in which `preview` reached the model, for `palace`.
    ///
    /// Why: the ranking column. Exact matches dominate; the prefix pass recovers
    /// the byte-cap-truncated tails that would otherwise read as zero.
    /// What: exact-map lookup plus a scan of the (small) partial map for entries
    /// that are a prefix of `preview` and long enough to be unambiguous. The
    /// cap's own `…` marker is stripped before the comparison — it is appended
    /// *after* the cut, so it is never part of the drawer's own preview and a
    /// literal compare against it could never match.
    /// Test: `truncated_tail_bullet_matches_by_prefix`, `short_partial_is_not_matched`.
    pub fn injections_for(&self, palace: &str, preview: &str) -> u64 {
        let exact = self
            .exact
            .get(&(palace.to_string(), preview.to_string()))
            .copied()
            .unwrap_or(0);
        let partial: u64 = self
            .partial
            .iter()
            .filter(|((p, body), _)| {
                if p != palace {
                    return false;
                }
                let stem = body.trim_end_matches(TRUNCATION_MARKER);
                stem.chars().count() >= MIN_PARTIAL_CHARS && preview.starts_with(stem)
            })
            .map(|(_, n)| *n)
            .sum();
        exact + partial
    }

    /// True when no log file was read at all.
    pub fn saw_no_logs(&self) -> bool {
        self.stats.files_scanned == 0
    }
}

/// Split an injection's drawer bullets into complete and byte-cap-truncated
/// preview bodies.
///
/// Why: the two need different matching rules, and mixing them would either drop
/// the truncated tail (under-count) or prefix-match short bullets against
/// unrelated drawers (over-count).
///
/// What: walks lines, tracking whether the current section is a drawer section,
/// and classifies each `- ` bullet. A bullet ending in a well-formed
/// `  _(tags: …)_` run is complete — the text before the run is the preview.
///
/// A bullet without that run is complete too, *except* for one case: the 4 KiB
/// cap cuts the block mid-line and appends `…`, which strips the tag run off
/// whatever bullet it landed in. That can only ever be the **last line of the
/// injection**, and only when the injection ends in the cap's `…`. Those two
/// conditions together identify the truncated tail; everything else without a
/// tag run is simply an untagged drawer, and treating it as a partial would lose
/// it — a short untagged preview never reaches the prefix pass's minimum length.
///
/// Complete bodies are deduplicated so a drawer listed under two palace sections
/// in one injection still counts as one turn.
///
/// Test: `kg_facts_section_is_ignored`, `untagged_bullet_counts_exactly`,
/// `truncated_tail_bullet_matches_by_prefix`,
/// `untagged_bullet_mid_block_is_not_treated_as_a_tail`.
fn parse_drawer_bullets(injection: &str) -> (Vec<String>, Vec<String>) {
    let mut exact: Vec<String> = Vec::new();
    let mut partial: Vec<String> = Vec::new();
    let mut in_drawer_section = false;
    // The cap writes `…` as the final character of the whole injection.
    let cap_truncated = injection.ends_with(TRUNCATION_MARKER);
    let last_line = injection.lines().next_back();

    for line in injection.lines() {
        if line.starts_with("## ") {
            in_drawer_section = line.starts_with(DRAWER_SECTION_PREFIX);
            continue;
        }
        if !in_drawer_section {
            continue;
        }
        let Some(body) = line.strip_prefix("- ") else {
            continue;
        };
        if let Some(preview) = split_tag_run(body) {
            if !exact.iter().any(|e| e == preview) {
                exact.push(preview.to_string());
            }
            continue;
        }
        let is_cap_tail = cap_truncated && last_line == Some(line);
        if is_cap_tail {
            partial.push(body.to_string());
        } else if !exact.iter().any(|e| e == body) {
            exact.push(body.to_string());
        }
    }
    (exact, partial)
}

/// Strip a trailing `  _(tags: …)_` run, returning the preview body.
///
/// Returns `None` when the line carries no well-formed tag run.
fn split_tag_run(body: &str) -> Option<&str> {
    if !body.ends_with(TAG_SUFFIX_CLOSE) {
        return None;
    }
    let at = body.rfind(TAG_SUFFIX_OPEN)?;
    Some(&body[..at])
}
