//! `tga aliases suggest` — auto-detect probable alias pairs (issue #347).
//!
//! Scans the `authors` and `commits` tables for high-signal hints that two
//! rows likely refer to the same engineer, ranks them by confidence, and
//! prints suggestions. With `--auto-accept`, applies the merge directly
//! for HIGH-confidence pairs above the threshold.
//!
//! Signals (highest signal first):
//! 1. Same observed `author_name` string under two different emails
//!    (confidence 0.95).
//! 2. Edit distance ≤ 2 on the email local-part with identical or
//!    near-identical domains (confidence 0.85).
//! 3. Known noise patterns: `*.local` hostnames, GitHub noreply emails,
//!    domain-typo corrections against the configured canonical_domain
//!    (confidence 0.75 – 0.90 depending on signal strength).
//! 4. Commit-SHA co-occurrence: the same SHA authored under two emails
//!    (confidence 0.90).

use std::collections::{BTreeMap, HashSet};

use rusqlite::params;
use strsim::levenshtein;
use tga::core::config::Config;
use tga::core::db::Database;

use tga::collect::identity::resolver::email_domain_matches;

/// Why: the threshold separating MED-confidence suggestions (mere hint) from
/// HIGH-confidence ones (safe to auto-accept). Pinned at the top so a single
/// constant drives both display labels and `--auto-accept` gating.
/// What: confidence at or above this value renders as `HIGH`; below it but
/// above the user's `--confidence` floor renders as `MED`.
/// Test: covered indirectly by `tests::auto_accept_only_merges_high`.
const HIGH_CONFIDENCE_CUTOFF: f64 = 0.85;

/// A single alias suggestion ranked by `confidence`.
///
/// Why: the suggester runs four passes and dedupes by `(src, dst)` pair; this
/// is the shared shape so each pass produces homogeneous output the dedup
/// step can sort and filter.
/// What: holds the source email (to be merged), the destination canonical
/// email (kept), the confidence score, and a short human-readable reason
/// surfaced in the printed output.
/// Test: indirectly via every `tests::*` case in this module.
#[derive(Debug, Clone)]
pub(crate) struct Suggestion {
    src: String,
    dst: String,
    confidence: f64,
    reason: String,
}

/// Public entry point invoked by the CLI dispatcher.
///
/// Why: the dispatcher only knows about config + DB + flag values; concrete
/// detection lives here so unit tests can exercise the algorithm without
/// going through clap.
/// What: collects suggestions from every detection pass, sorts by descending
/// confidence, applies the `--confidence` floor, prints, and (optionally)
/// auto-merges the HIGH-confidence pairs.
/// Test: see `tests::same_name_different_email_detected` and the other
/// signal-specific tests below.
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

    let mut suggestions: Vec<Suggestion> = Vec::new();
    suggestions.extend(detect_same_name_pairs(db)?);
    suggestions.extend(detect_edit_distance_pairs(db)?);
    suggestions.extend(detect_noise_patterns(db, canonical_domain.as_deref())?);
    suggestions.extend(detect_commit_sha_cooccurrence(db)?);

    let suggestions = dedupe_and_rank(suggestions, confidence_floor);

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

/// Signal 1: identical observed display names under two different emails.
///
/// Why: the strongest possible hint short of an explicit user action; if two
/// authors share `author_name` and only the email differs, they are almost
/// certainly the same person.
/// What: groups distinct `(canonical_name, canonical_email)` pairs by name
/// and emits one suggestion per non-canonical email pointing at the
/// alphabetically-first email for the name (deterministic destination).
/// Test: see `tests::same_name_different_email_detected`.
fn detect_same_name_pairs(db: &Database) -> anyhow::Result<Vec<Suggestion>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(
        "SELECT canonical_name, canonical_email FROM authors \
         ORDER BY canonical_name, canonical_email",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in rows {
        let (name, email) = r?;
        groups.entry(name.to_lowercase()).or_default().push(email);
    }

    let mut out: Vec<Suggestion> = Vec::new();
    for (_name, mut emails) in groups {
        if emails.len() < 2 {
            continue;
        }
        emails.sort();
        emails.dedup();
        if emails.len() < 2 {
            continue;
        }
        let dst = emails[0].clone();
        for src in emails.into_iter().skip(1) {
            out.push(Suggestion {
                src,
                dst: dst.clone(),
                confidence: 0.95,
                reason: "same canonical_name".to_string(),
            });
        }
    }
    Ok(out)
}

/// Signal 2: email local-parts within edit distance ≤ 2, near-identical domains.
///
/// Why: catches `alice@co.com` vs `alice@contractor.co.com` (contractor
/// prefix), `bob@co.com` vs `b.matsuoka@co.com` only when truly close, etc.
/// What: cross-joins all distinct emails, computes Levenshtein on the
/// local-part. Emits MED-confidence (0.80) for distance ≤ 2 with matching
/// domains; lower for cross-domain matches.
/// Test: see `tests::edit_distance_local_part_detected`.
fn detect_edit_distance_pairs(db: &Database) -> anyhow::Result<Vec<Suggestion>> {
    let conn = db.connection();
    let mut stmt = conn.prepare("SELECT canonical_email FROM authors")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let emails: Vec<String> = rows.filter_map(|r| r.ok()).collect();

    let mut out: Vec<Suggestion> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for i in 0..emails.len() {
        for j in (i + 1)..emails.len() {
            let a = &emails[i];
            let b = &emails[j];
            let (la, da) = match split_email(a) {
                Some(x) => x,
                None => continue,
            };
            let (lb, db_) = match split_email(b) {
                Some(x) => x,
                None => continue,
            };
            // Skip if local-parts are identical (the same-name signal is
            // already a stronger producer for that case) or if either is
            // very short (false-positive risk).
            if la == lb || la.len() < 3 || lb.len() < 3 {
                continue;
            }
            let dist = levenshtein(&la, &lb);
            if dist == 0 || dist > 2 {
                continue;
            }
            // Domain check: must be identical, or one domain must be a
            // suffix of the other (e.g. `co.com` vs `contractor.co.com`).
            let domains_match = da == db_ || da.ends_with(&db_) || db_.ends_with(&da);
            if !domains_match {
                continue;
            }
            // Lower confidence the larger the edit distance.
            let confidence = if dist == 1 { 0.85 } else { 0.78 };
            // Pick the shorter / alphabetically earlier as the destination
            // so the suggestion is stable across runs.
            let (src, dst) = if a < b {
                (b.clone(), a.clone())
            } else {
                (a.clone(), b.clone())
            };
            let key = (src.clone(), dst.clone());
            if seen.insert(key) {
                out.push(Suggestion {
                    src,
                    dst,
                    confidence,
                    reason: format!("edit-distance {dist} on local-part"),
                });
            }
        }
    }
    Ok(out)
}

/// Signal 3: known noise patterns — `.local`, GitHub noreply, domain typos.
///
/// Why: these patterns appear constantly in commit history and account for
/// most of the alias-table noise on a real corpus.
/// What:
/// - `<local>@<host>.local` → try to find an identity at
///   `<local>@<canonical_domain>` (HIGH if found, no suggestion otherwise).
/// - `<id>+<login>@users.noreply.github.com` → look for any identity whose
///   local-part contains the login (HIGH on exact local-part match, MED on
///   substring match).
/// - Domain edit-distance to `canonical_domain`: catches typos like
///   `duettoresearh.com` vs `duettoresearch.com` (HIGH on dist ≤ 2).
///
/// Test: see `tests::dotlocal_email_routed_to_canonical_domain`.
fn detect_noise_patterns(
    db: &Database,
    canonical_domain: Option<&str>,
) -> anyhow::Result<Vec<Suggestion>> {
    let conn = db.connection();
    let mut stmt = conn.prepare("SELECT canonical_email FROM authors")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let emails: Vec<String> = rows.filter_map(|r| r.ok()).collect();

    let mut out: Vec<Suggestion> = Vec::new();
    for email in &emails {
        let (local, domain) = match split_email(email) {
            Some(x) => x,
            None => continue,
        };

        // 3a. `.local` hostnames (e.g. `bob@ENCDXPAHDLT0616.local`).
        if domain.ends_with(".local") {
            if let Some(canon_dom) = canonical_domain {
                let target = format!("{local}@{canon_dom}");
                if emails.iter().any(|e| e.eq_ignore_ascii_case(&target)) {
                    out.push(Suggestion {
                        src: email.clone(),
                        dst: target,
                        confidence: 0.90,
                        reason: ".local hostname → org email".to_string(),
                    });
                }
            }
        }

        // 3b. GitHub noreply emails: `<id>+<login>@users.noreply.github.com`.
        if domain == "users.noreply.github.com" {
            if let Some(login) = local.split_once('+').map(|(_, l)| l) {
                // Look for an identity whose local-part is exactly the login.
                for other in &emails {
                    if other == email {
                        continue;
                    }
                    let (other_local, _) = match split_email(other) {
                        Some(x) => x,
                        None => continue,
                    };
                    if other_local == login {
                        out.push(Suggestion {
                            src: email.clone(),
                            dst: other.clone(),
                            confidence: 0.90,
                            reason: format!("GitHub noreply login '{login}'"),
                        });
                    } else if other_local.contains(login) || login.contains(&other_local) {
                        out.push(Suggestion {
                            src: email.clone(),
                            dst: other.clone(),
                            confidence: 0.78,
                            reason: format!("GitHub noreply login '{login}' (partial)"),
                        });
                    }
                }
            }
        }

        // 3c. Domain edit-distance against canonical_domain (catches typos).
        if let Some(canon_dom) = canonical_domain {
            if !email_domain_matches(email, canon_dom) {
                let dist = levenshtein(&domain, canon_dom);
                if dist > 0 && dist <= 2 {
                    let target = format!("{local}@{canon_dom}");
                    if emails.iter().any(|e| e.eq_ignore_ascii_case(&target)) {
                        out.push(Suggestion {
                            src: email.clone(),
                            dst: target,
                            confidence: 0.88,
                            reason: format!("domain typo '{domain}' (dist {dist})"),
                        });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Signal 4: same commit SHA attributed to two different emails.
///
/// Why: this is the strongest possible evidence — git itself recorded both
/// emails against the same content. Happens when a commit was cherry-picked
/// or rebased and the author email changed in transit.
/// What: groups commits by SHA and emits HIGH-confidence (0.92) suggestions
/// for any SHA with two distinct author_email values.
/// Test: see `tests::same_sha_two_emails_detected`.
fn detect_commit_sha_cooccurrence(db: &Database) -> anyhow::Result<Vec<Suggestion>> {
    let conn = db.connection();
    let mut stmt =
        conn.prepare("SELECT sha, author_email FROM commits ORDER BY sha, author_email")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in rows {
        let (sha, email) = r?;
        groups.entry(sha).or_default().push(email);
    }

    let mut out: Vec<Suggestion> = Vec::new();
    for (sha, mut emails) in groups {
        emails.sort();
        emails.dedup();
        if emails.len() < 2 {
            continue;
        }
        let dst = emails[0].clone();
        for src in emails.into_iter().skip(1) {
            out.push(Suggestion {
                src,
                dst: dst.clone(),
                confidence: 0.92,
                reason: format!("same SHA {short}", short = &sha[..sha.len().min(8)]),
            });
        }
    }
    Ok(out)
}

/// Dedupe suggestions on `(src, dst)`, keep the highest-confidence reason,
/// then sort descending by confidence and apply the user's floor.
///
/// Why: separate detection passes may produce overlapping pairs; we want a
/// single line per pair with the strongest reason, and stable ordering for
/// deterministic CLI output.
/// What: walks the input, builds a map keyed on the canonical pair, retains
/// the highest score, then sorts.
/// Test: see `tests::dedupe_keeps_highest_confidence`.
fn dedupe_and_rank(input: Vec<Suggestion>, floor: f64) -> Vec<Suggestion> {
    let mut by_pair: BTreeMap<(String, String), Suggestion> = BTreeMap::new();
    for s in input {
        let key = (s.src.clone(), s.dst.clone());
        match by_pair.get(&key) {
            Some(existing) if existing.confidence >= s.confidence => {}
            _ => {
                by_pair.insert(key, s);
            }
        }
    }
    let mut out: Vec<Suggestion> = by_pair
        .into_values()
        .filter(|s| s.confidence >= floor)
        .collect();
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.src.cmp(&b.src))
    });
    out
}

/// Apply a merge between two existing identities, returning the number of
/// commits reassigned.
///
/// Why: `--auto-accept` needs to perform merges without going through the
/// interactive confirm path in the parent module; this is a small private
/// helper that runs the same DB transaction.
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

/// Why: every detection pass needs to split an email into (local, domain)
/// lowercased; pulling this into one place keeps casing logic consistent.
/// What: returns `Some((local, domain))` or `None` for malformed input.
/// Test: implicit in every pass that calls it.
fn split_email(email: &str) -> Option<(String, String)> {
    let at = email.rfind('@')?;
    let local = email[..at].to_lowercase();
    let domain = email[at + 1..].to_lowercase();
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    Some((local, domain))
}

#[cfg(test)]
#[path = "suggest_tests.rs"]
mod tests;
