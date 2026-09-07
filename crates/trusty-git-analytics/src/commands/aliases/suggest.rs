//! `tga aliases suggest` — print probable alias pairs (issue #347).
//!
//! Why: detection itself moved to [`tga::collect::identity::suggest`] under
//! #6142, because the authorship report needs the same answer and cannot call
//! a CLI handler. What remains here is presentation, the `--auto-accept`
//! merge, and the `--review-file` near-miss artifact (#6993) — all three are
//! CLI-only concerns.
//! What: [`run`] resolves the configured canonical domain, calls
//! [`tga::collect::identity::suggest::detect_all`], prints the ranked pairs at
//! or above `--confidence`, optionally applies the HIGH-confidence ones, and
//! optionally writes the pairs BELOW `--confidence` to a review TSV.
//! Test: `tests` in `suggest_tests.rs`.

use std::io::{self, Write};
use std::path::Path;

use rusqlite::params;
use tga::collect::identity::resolver::configured_canonical_domain;
use tga::collect::identity::suggest::{detect_all, Suggestion, HIGH_CONFIDENCE_CUTOFF};
use tga::core::config::Config;
use tga::core::db::Database;

/// The lowest confidence a pair may score and still reach the `--review-file`
/// artifact.
///
/// Why (#6993): a near miss is the band between "not worth a human's time" and
/// "printed as a suggestion". 0.50 is the wide-net figure the no-suggestions
/// message already points operators at, so the review file covers exactly the
/// pairs a `--confidence 0.5` run would have printed.
/// Test: `tests::the_review_file_lists_only_near_miss_pairs`.
const NEAR_MISS_FLOOR: f64 = 0.50;

/// Header row of the `--review-file` TSV, naming the columns issue #6993 asks
/// for: both identities, the reason, the confidence, and the column an
/// operator edits to confirm the pair.
const REVIEW_HEADER: &str = "src\tdst\treason\tconfidence\tconfirmed";

/// Public entry point invoked by the CLI dispatcher.
///
/// Why: the dispatcher only knows about config + DB + flag values; the
/// detection algorithm lives in the library so the report layer shares it.
/// What: collects ranked suggestions above `confidence_floor`, prints them to
/// stdout, writes the near misses to `review_file` when one is given, and
/// (with `auto_accept`) merges the HIGH-confidence pairs.
/// Test: `tests::auto_accept_only_merges_high`,
/// `tests::config_canonical_domain_threads_through`.
pub(super) fn run(
    config: &Config,
    db: &mut Database,
    confidence_floor: f64,
    auto_accept: bool,
    review_file: Option<&Path>,
) -> anyhow::Result<()> {
    let mut out = io::stdout().lock();
    run_to(
        config,
        db,
        confidence_floor,
        auto_accept,
        review_file,
        &mut out,
    )
}

/// [`run`] with the destination for its report lines injected.
///
/// Why (#6993): the review file's contract is that stdout keeps showing only
/// the at-or-above-threshold pairs, which is only assertable when a test can
/// read what was printed.
/// What: identical to [`run`], writing every report line to `out` instead of
/// stdout. Merge warnings still go to stderr.
/// Test: `tests::{the_review_file_lists_only_near_miss_pairs,
/// no_review_file_is_written_without_the_flag}`.
fn run_to<W: Write>(
    config: &Config,
    db: &mut Database,
    confidence_floor: f64,
    auto_accept: bool,
    review_file: Option<&Path>,
    out: &mut W,
) -> anyhow::Result<()> {
    let canonical_domain = configured_canonical_domain(config);

    // #6993: a review file needs the pairs BELOW `--confidence`, so detection
    // runs down to the near-miss floor and the partition below restores the
    // printed set to exactly what an unflagged run would show.
    let detect_floor = match review_file {
        Some(_) => NEAR_MISS_FLOOR.min(confidence_floor),
        None => confidence_floor,
    };
    let (suggestions, near_misses): (Vec<Suggestion>, Vec<Suggestion>) =
        detect_all(db.connection(), canonical_domain.as_deref(), detect_floor)?
            .into_iter()
            .partition(|s| s.confidence >= confidence_floor);

    if let Some(path) = review_file {
        write_review_file(path, &near_misses)?;
        writeln!(
            out,
            "Wrote {count} near-miss candidate(s) (confidence {NEAR_MISS_FLOOR:.2} up to \
             {confidence_floor:.2}) to {path} — set `confirmed` to `yes` on a row to accept it.",
            count = near_misses.len(),
            path = path.display()
        )?;
    }

    if suggestions.is_empty() {
        writeln!(
            out,
            "No alias suggestions found above confidence {confidence_floor:.2}. \
             (Try `--confidence 0.5` for a wider net.)"
        )?;
        return Ok(());
    }

    writeln!(
        out,
        "Suggested aliases (confidence ≥ {confidence_floor:.2}):"
    )?;
    for s in &suggestions {
        let label = if s.confidence >= HIGH_CONFIDENCE_CUTOFF {
            "HIGH"
        } else {
            "MED "
        };
        writeln!(
            out,
            "  {label}  {src} → {dst}  [{reason}]",
            src = s.src,
            dst = s.dst,
            reason = s.reason
        )?;
    }
    writeln!(out)?;

    if auto_accept {
        let mut accepted = 0usize;
        for s in &suggestions {
            if s.confidence < HIGH_CONFIDENCE_CUTOFF {
                continue;
            }
            // Re-fetch in case an earlier merge already collapsed the row.
            let still_exists = super::lookup_author(db, &s.src)?.is_some()
                && super::lookup_author(db, &s.dst)?.is_some();
            if !still_exists {
                continue;
            }
            match apply_merge(db, &s.src, &s.dst) {
                Ok(n) => {
                    accepted += 1;
                    writeln!(
                        out,
                        "Merged {} → {} ({} commits reassigned)",
                        s.src, s.dst, n
                    )?;
                }
                Err(e) => {
                    eprintln!("WARN: skip merge {} → {}: {e}", s.src, s.dst);
                }
            }
        }
        writeln!(out, "Auto-accepted {accepted} HIGH-confidence merge(s).")?;
    } else {
        writeln!(
            out,
            "Run `tga aliases merge <source> <dest>` to accept individual pairs, \
             or `tga aliases suggest --auto-accept --confidence {HIGH_CONFIDENCE_CUTOFF:.2}` \
             to accept all HIGH-confidence pairs at once."
        )?;
    }
    Ok(())
}

/// Write the near-miss candidates to `path` as a TSV an operator edits.
///
/// Why (#6993): a pair scoring just under `--confidence` never appears in the
/// normal output, so the split it represents stays invisible until someone
/// re-runs with a lower threshold and remembers what they saw. The artifact
/// carries the same four facts the suggestion line prints, plus the
/// `confirmed` column that makes a human's answer durable.
/// What: writes `REVIEW_HEADER` followed by one row per candidate, in the
/// detector's own confidence-descending order, with `confirmed` seeded to
/// `no`. The file is truncated on every run, and an empty candidate list still
/// produces the header so the operator sees the answer rather than a missing
/// file. Nothing beyond the printed suggestion's own fields is written.
/// Test: `tests::the_review_file_lists_only_near_miss_pairs`.
fn write_review_file(path: &Path, near_misses: &[Suggestion]) -> anyhow::Result<()> {
    let mut body = String::from(REVIEW_HEADER);
    body.push('\n');
    for s in near_misses {
        body.push_str(&format!(
            "{src}\t{dst}\t{reason}\t{confidence:.2}\tno\n",
            src = tsv_field(&s.src),
            dst = tsv_field(&s.dst),
            reason = tsv_field(&s.reason),
            confidence = s.confidence
        ));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, body)?;
    Ok(())
}

/// One TSV cell with the delimiters neutralised.
///
/// Why: `src` and `dst` come from the database, so a tab or newline inside an
/// address would silently shift every later column.
/// Test: covered by `tests::the_review_file_lists_only_near_miss_pairs`.
fn tsv_field(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

/// Apply a merge between two existing identities, returning the number of
/// commits reassigned.
///
/// Why: `--auto-accept` needs to perform merges without going through the
/// interactive confirm path in the parent module.
/// What: identical to the body of [`super::merge`] but with no prompt and a
/// numeric return for the auto-accept summary line.
/// Test: covered by `tests::auto_accept_only_merges_high` end-to-end.
fn apply_merge(db: &mut Database, src_email: &str, dst_email: &str) -> anyhow::Result<usize> {
    let (src_id, _, src_aliases_json) = super::lookup_author(db, src_email)?
        .ok_or_else(|| anyhow::anyhow!("source identity not found: {src_email}"))?;
    let (dst_id, _, dst_aliases_json) = super::lookup_author(db, dst_email)?
        .ok_or_else(|| anyhow::anyhow!("destination identity not found: {dst_email}"))?;
    let mut src_aliases: Vec<String> = serde_json::from_str(&src_aliases_json).unwrap_or_default();
    let mut dst_aliases: Vec<String> = serde_json::from_str(&dst_aliases_json).unwrap_or_default();
    dst_aliases.append(&mut src_aliases);
    dst_aliases.push(src_email.to_string());
    dst_aliases.sort();
    dst_aliases.dedup();
    let merged_aliases = serde_json::to_string(&dst_aliases)?;
    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE commits SET author_id = ?1 WHERE author_id = ?2",
        params![dst_id, src_id],
    )?;
    tx.execute(
        "UPDATE authors SET aliases = ?1 WHERE id = ?2",
        params![merged_aliases, dst_id],
    )?;
    tx.execute("DELETE FROM authors WHERE id = ?1", params![src_id])?;
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
#[path = "suggest_tests.rs"]
mod tests;
