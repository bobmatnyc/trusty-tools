//! `tga aliases suggest` — print probable alias pairs (issue #347).
//!
//! Why: detection itself moved to [`tga::collect::identity::suggest`] under
//! #6142, because the authorship report needs the same answer and cannot call
//! a CLI handler. What remains here is presentation and the `--auto-accept`
//! merge, both of which are CLI-only concerns.
//! What: [`run`] resolves the configured canonical domain, calls
//! [`tga::collect::identity::suggest::detect_all`], prints the ranked pairs,
//! and optionally applies the HIGH-confidence ones.
//! Test: `tests` in `suggest_tests.rs`.

use rusqlite::params;
use tga::collect::identity::suggest::{detect_all, HIGH_CONFIDENCE_CUTOFF};
use tga::core::config::Config;
use tga::core::db::Database;

/// Public entry point invoked by the CLI dispatcher.
///
/// Why: the dispatcher only knows about config + DB + flag values; the
/// detection algorithm lives in the library so the report layer shares it.
/// What: collects ranked suggestions above `confidence_floor`, prints them,
/// and (with `auto_accept`) merges the HIGH-confidence pairs.
/// Test: `tests::auto_accept_only_merges_high`,
/// `tests::config_canonical_domain_threads_through`.
pub(super) fn run(
    config: &Config,
    db: &mut Database,
    confidence_floor: f64,
    auto_accept: bool,
) -> anyhow::Result<()> {
    let canonical_domain = config
        .team
        .as_ref()
        .and_then(|t| t.canonical_domain.as_deref())
        .map(|d| d.trim().trim_start_matches('@').to_lowercase())
        .filter(|d| !d.is_empty());

    let suggestions = detect_all(
        db.connection(),
        canonical_domain.as_deref(),
        confidence_floor,
    )?;

    if suggestions.is_empty() {
        println!(
            "No alias suggestions found above confidence {confidence_floor:.2}. \
             (Try `--confidence 0.5` for a wider net.)"
        );
        return Ok(());
    }

    println!("Suggested aliases (confidence ≥ {confidence_floor:.2}):");
    for s in &suggestions {
        let label = if s.confidence >= HIGH_CONFIDENCE_CUTOFF {
            "HIGH"
        } else {
            "MED "
        };
        println!(
            "  {label}  {src} → {dst}  [{reason}]",
            src = s.src,
            dst = s.dst,
            reason = s.reason
        );
    }
    println!();

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
                    println!("Merged {} → {} ({} commits reassigned)", s.src, s.dst, n);
                }
                Err(e) => {
                    eprintln!("WARN: skip merge {} → {}: {e}", s.src, s.dst);
                }
            }
        }
        println!("Auto-accepted {accepted} HIGH-confidence merge(s).");
    } else {
        println!(
            "Run `tga aliases merge <source> <dest>` to accept individual pairs, \
             or `tga aliases suggest --auto-accept --confidence {HIGH_CONFIDENCE_CUTOFF:.2}` \
             to accept all HIGH-confidence pairs at once."
        );
    }
    Ok(())
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
