//! Per-file diff splitter — turns `FilteredDiff` output into a `Vec<MapUnit>`
//! (Phase 2, #696 / #680).
//!
//! Why: the map stage needs the diff split into independent, bounded work units
//! before it can fan out to parallel LLM calls.  This module is the only place
//! that understands the splitting rules: whole-file units, hunk sub-chunking
//! for oversized files, and metadata-only classification for
//! deleted/binary/rename-only/summary-only files.
//!
//! What: `split_into_units` is a pure function — it takes the `FilteredDiff`
//! that Stage A/B/C already produced plus the `MapReduceConfig` for budget
//! knobs, and returns a `Vec<MapUnit>` in a deterministic stable order.
//! No I/O, no LLM calls.
//!
//! Test: see `splitter_tests.rs` for comprehensive unit coverage.

use tracing::debug;

use crate::{
    config::mapreduce::MapReduceConfig,
    pipeline::diff_analyzer::models::{FileDisposition, FilteredDiff, FilteredFile, FilteredHunk},
};

use super::unit::{MapUnit, MapUnitKind};

// ─── Render helpers ───────────────────────────────────────────────────────────

/// Render a single `FilteredFile`'s header + hunk list into a diff string.
///
/// Why: `FilteredDiff::render_for_prompt` renders the whole diff; here we need
/// per-file rendering so we can apply the per-file char budget and split by
/// hunk without re-parsing.  Logic mirrors `models.rs:200` but for one file.
/// What: produces `--- a/<path>\n+++ b/<path>\n<hunks>` for `Kept` files;
/// a `# <path>: <summary>\n` line for `SummaryOnly` files; empty string for
/// `Dropped` (should never be passed here).
/// Test: `render_file_basic`, exercised indirectly by all splitter tests.
fn render_file(file: &FilteredFile) -> String {
    match file.disposition {
        FileDisposition::Kept => {
            let header = format!("--- a/{0}\n+++ b/{0}\n", file.filename);
            let hunks = file
                .hunks
                .iter()
                .map(|h| format!("{}\n", h.render()))
                .collect::<String>();
            format!("{header}{hunks}")
        }
        FileDisposition::SummaryOnly => {
            if let Some(ref s) = file.summary_line {
                format!("# {}: {}\n", file.filename, s)
            } else {
                String::new()
            }
        }
        FileDisposition::Dropped => String::new(),
    }
}

/// Render a file header + a specific slice of hunks into a diff string.
///
/// Why: sub-chunking builds multiple units for the same file by rendering
/// different hunk slices; this helper keeps that logic out of `split_into_units`.
/// What: produces the same format as `render_file` but for the given hunk
/// slice only.
/// Test: exercised by `split_oversized_sub_chunks_by_whole_hunk` tests.
fn render_file_hunks(path: &str, hunks: &[FilteredHunk]) -> String {
    let header = format!("--- a/{path}\n+++ b/{path}\n");
    let hunk_body = hunks
        .iter()
        .map(|h| format!("{}\n", h.render()))
        .collect::<String>();
    format!("{header}{hunk_body}")
}

// ─── Classification helpers ───────────────────────────────────────────────────

/// Returns `true` if the file should become a metadata-only unit (no LLM).
///
/// Why: deleted, binary (no hunks + not added), rename-only (renamed with no
/// surviving hunks), and summary-only files never need LLM review; skipping
/// them is the single biggest cost saver on refactor PRs (design doc §2.1).
/// What: classifies by (status, disposition, hunk count):
///   - `"removed"` → always metadata-only (no `+` content to review).
///   - `SummaryOnly` disposition → always metadata-only (fixture/i18n).
///   - No hunks + `"renamed"` → pure rename, metadata-only.
///   - No hunks + not `"added"` → binary or empty diff, metadata-only.
///
/// Test: `classify_deleted_is_metadata`, `classify_rename_no_hunks`,
/// `classify_binary_no_hunks`, `classify_summary_only_is_metadata`.
fn is_metadata_only(file: &FilteredFile) -> bool {
    if file.status == "removed" {
        return true;
    }
    if file.disposition == FileDisposition::SummaryOnly {
        return true;
    }
    if file.hunks.is_empty() {
        // No hunks after Stage A/B.  Could be:
        //   - pure rename (no content change)
        //   - binary file (parser yields no @@ hunks)
        //   - newly-added empty file (status "added") — we could review the
        //     whole file header as a creation notice, but since there is no
        //     diff content, metadata-only is the right call too.
        return true;
    }
    false
}

/// Build the metadata note string for a metadata-only unit.
///
/// Why: the reduce stage surfaces these notes in the partial-result banner so
/// reviewers know which files were not LLM-reviewed and why.
/// What: returns a short human-readable label.
/// Test: covered indirectly by `classify_*` tests in `splitter_tests.rs`.
fn metadata_note(file: &FilteredFile) -> String {
    if file.status == "removed" {
        return "deleted file".to_string();
    }
    if file.disposition == FileDisposition::SummaryOnly {
        return "summary-only (fixture/generated)".to_string();
    }
    if file.hunks.is_empty() {
        if file.status == "renamed" {
            return "rename-only (no content change)".to_string();
        }
        return "binary file or empty diff".to_string();
    }
    "metadata-only".to_string()
}

// ─── Sub-chunker ─────────────────────────────────────────────────────────────

/// Sub-chunk the hunks of an oversized `FilteredFile` into multiple `MapUnit`s.
///
/// Why: a single file whose diff exceeds `per_file_chars` cannot be sent as one
/// map unit without violating the per-call context budget.  Splitting by whole
/// hunks (never mid-hunk) is the only safe boundary that preserves diff
/// semantics.
/// What: packs whole `FilteredHunk`s greedily into chunks, each rendered string
/// ≤ `per_file_chars`.  A single hunk that alone exceeds the budget is emitted
/// as its own unit with `hunk_oversized = true`.  Returns a `Vec<MapUnit>` with
/// correct `chunk_index` / `chunk_total` and `diff_char_count` set.
/// Test: `split_oversized_sub_chunks_by_whole_hunk`,
/// `split_single_giant_hunk_kept_whole_flagged`,
/// `split_exactly_at_budget_one_unit`,
/// `split_oversized_boundary_between_hunks`.
fn sub_chunk_file(file: &FilteredFile, per_file_chars: usize) -> Vec<MapUnit> {
    let header_len = format!("--- a/{0}\n+++ b/{0}\n", file.filename).len();

    // Build (rendered_hunk_string, char_count) pairs for all hunks.
    let rendered_hunks: Vec<(String, usize)> = file
        .hunks
        .iter()
        .map(|h| {
            let s = format!("{}\n", h.render());
            let len = s.len();
            (s, len)
        })
        .collect();

    // Greedy pack: accumulate hunks into a chunk until adding the next hunk
    // would exceed the budget (accounting for the file header).
    let mut chunks: Vec<Vec<usize>> = Vec::new(); // indices into `rendered_hunks`
    let mut current_chunk: Vec<usize> = Vec::new();
    let mut current_len: usize = header_len;

    for (idx, (_, hunk_len)) in rendered_hunks.iter().enumerate() {
        let would_exceed = current_len + hunk_len > per_file_chars;
        if would_exceed && !current_chunk.is_empty() {
            // Flush current chunk and start a new one.
            chunks.push(std::mem::take(&mut current_chunk));
            current_len = header_len;
        }
        current_chunk.push(idx);
        current_len += hunk_len;
    }
    if !current_chunk.is_empty() {
        chunks.push(current_chunk);
    }

    let chunk_total = chunks.len().max(1);

    chunks
        .into_iter()
        .enumerate()
        .map(|(chunk_index, hunk_indices)| {
            let hunks_slice: Vec<FilteredHunk> = hunk_indices
                .iter()
                .map(|&i| file.hunks[i].clone())
                .collect();

            // Detect a single-hunk chunk that itself exceeds the budget.
            let hunk_oversized = hunk_indices.len() == 1
                && (header_len + rendered_hunks[hunk_indices[0]].1) > per_file_chars;

            let diff_text = render_file_hunks(&file.filename, &hunks_slice);
            let diff_char_count = diff_text.len();

            MapUnit {
                file: file.filename.clone(),
                status: file.status.clone(),
                kind: MapUnitKind::Review { diff_text },
                diff_char_count,
                chunk_index,
                chunk_total,
                hunk_oversized,
            }
        })
        .collect()
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Split a `FilteredDiff` into `MapUnit`s according to the given config.
///
/// Why: the map stage needs a flat list of independent, budget-bounded work
/// units before it can fan out.  Centralising the splitting rules here keeps
/// the fan-out loop in `map.rs` (Phase 3) simple and ensures every caller
/// gets the same classification behaviour.
///
/// What: iterates `filtered.files` in their existing order (Stage-A/B already
/// determined this order; it is deterministic across runs on the same input).
/// For each file:
///  1. If `is_metadata_only` → emit one `MetadataOnly` unit.
///  2. Else render the file.  If `rendered.len() <= per_file_chars` → emit one
///     `Review` unit.
///  3. Else sub-chunk by whole hunks via `sub_chunk_file`.
///
/// Budget enforcement: the splitter accumulates `total_char_budget` across
/// all **Review** units emitted so far.  Once the running total would exceed
/// `config.total_char_budget`, remaining files are down-graded to
/// `MetadataOnly` with the note `"budget exhausted"` rather than being
/// silently dropped (honest partial-coverage labelling, per §2.3 of the design
/// doc).  The `max_calls` hard ceiling similarly caps the number of `Review`
/// units; units beyond it become `MetadataOnly("max-calls reached")`.  Both
/// limits apply only to `Review` units; `MetadataOnly` units never count
/// against the budgets (they cost nothing).
///
/// Ordering/stability: units for the same file are contiguous and in
/// `chunk_index` order; files appear in the order of `filtered.files`.
///
/// Test: see `splitter_tests.rs` for comprehensive coverage.
pub fn split_into_units(filtered: &FilteredDiff, config: &MapReduceConfig) -> Vec<MapUnit> {
    let per_file_chars = config.per_file_chars;
    let total_budget = config.total_char_budget;
    let max_calls = config.max_calls;

    let mut units: Vec<MapUnit> = Vec::with_capacity(filtered.files.len());
    let mut total_chars_used: usize = 0;
    let mut review_calls: usize = 0;

    for file in &filtered.files {
        // Step 1: metadata-only classification.
        if is_metadata_only(file) {
            let note = metadata_note(file);
            debug!(
                file = %file.filename,
                status = %file.status,
                %note,
                "splitter: metadata-only unit"
            );
            units.push(MapUnit {
                file: file.filename.clone(),
                status: file.status.clone(),
                kind: MapUnitKind::MetadataOnly { note },
                diff_char_count: 0,
                chunk_index: 0,
                chunk_total: 1,
                hunk_oversized: false,
            });
            continue;
        }

        // Step 2: render the file diff.
        let rendered = render_file(file);
        let rendered_len = rendered.len();

        if rendered_len <= per_file_chars {
            // Fits in one unit.  Check total budget + max_calls before emitting.
            if review_calls >= max_calls {
                debug!(
                    file = %file.filename,
                    review_calls,
                    "splitter: max_calls reached — downgrading to metadata-only"
                );
                units.push(MapUnit {
                    file: file.filename.clone(),
                    status: file.status.clone(),
                    kind: MapUnitKind::MetadataOnly {
                        note: "max-calls reached".to_string(),
                    },
                    diff_char_count: 0,
                    chunk_index: 0,
                    chunk_total: 1,
                    hunk_oversized: false,
                });
                continue;
            }
            if total_chars_used + rendered_len > total_budget {
                debug!(
                    file = %file.filename,
                    total_chars_used,
                    rendered_len,
                    total_budget,
                    "splitter: total_char_budget exhausted — downgrading to metadata-only"
                );
                units.push(MapUnit {
                    file: file.filename.clone(),
                    status: file.status.clone(),
                    kind: MapUnitKind::MetadataOnly {
                        note: "budget exhausted".to_string(),
                    },
                    diff_char_count: 0,
                    chunk_index: 0,
                    chunk_total: 1,
                    hunk_oversized: false,
                });
                continue;
            }
            total_chars_used += rendered_len;
            review_calls += 1;
            debug!(
                file = %file.filename,
                rendered_len,
                review_calls,
                total_chars_used,
                "splitter: single-unit review"
            );
            units.push(MapUnit {
                file: file.filename.clone(),
                status: file.status.clone(),
                kind: MapUnitKind::Review {
                    diff_text: rendered,
                },
                diff_char_count: rendered_len,
                chunk_index: 0,
                chunk_total: 1,
                hunk_oversized: false,
            });
        } else {
            // Step 3: oversized — sub-chunk by whole hunks.
            debug!(
                file = %file.filename,
                rendered_len,
                per_file_chars,
                "splitter: oversized file — sub-chunking by hunk"
            );
            let sub_chunks = sub_chunk_file(file, per_file_chars);
            for mut chunk in sub_chunks {
                if chunk.is_metadata_only() {
                    // Metadata-only sub-chunks (shouldn't occur here, but guard).
                    units.push(chunk);
                    continue;
                }
                if review_calls >= max_calls {
                    chunk.kind = MapUnitKind::MetadataOnly {
                        note: "max-calls reached".to_string(),
                    };
                    chunk.diff_char_count = 0;
                    units.push(chunk);
                    continue;
                }
                if total_chars_used + chunk.diff_char_count > total_budget {
                    chunk.kind = MapUnitKind::MetadataOnly {
                        note: "budget exhausted".to_string(),
                    };
                    chunk.diff_char_count = 0;
                    units.push(chunk);
                    continue;
                }
                total_chars_used += chunk.diff_char_count;
                review_calls += 1;
                units.push(chunk);
            }
        }
    }

    debug!(
        total_units = units.len(),
        review_units = review_calls,
        total_chars_used,
        "splitter: done"
    );
    units
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Tests are split across two sibling files to honour the 500-line cap (CLAUDE.md).
// `splitter_tests.rs`        — single-file, metadata-only, and oversized sub-chunk tests.
// `splitter_budget_tests.rs` — budget enforcement, content, status, and edge cases.

#[cfg(test)]
#[path = "splitter_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "splitter_budget_tests.rs"]
mod budget_tests;
