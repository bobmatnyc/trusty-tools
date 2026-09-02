//! Alias-suggestion detection over the `authors` and `commits` tables.
//!
//! Why: two callers need the same answer to "which identities look like the
//! same person?" — `tga aliases suggest` prints them for a human to confirm,
//! and the authorship report (#6142) counts the ones a reader has NOT yet
//! merged so a bus-factor or top-author-share figure carries its own risk
//! flag. Detection lived only in the CLI handler, which the report layer
//! cannot call; this module is the single implementation both use.
//! What: [`Suggestion`], the four detection passes behind
//! [`detect_from_authors`] and [`detect_all`], and [`dedupe_and_rank`].
//! Nothing here mutates the database — a suggestion is never a merge.
//! Test: the `tests` module below.
//!
//! Signals (highest signal first):
//! 1. Same observed `author_name` string under two different emails
//!    (confidence 0.95).
//! 2. Edit distance ≤ 2 on the email local-part with identical or
//!    near-identical domains (confidence 0.78 – 0.85).
//! 3. Known noise patterns: `*.local` hostnames, GitHub noreply emails,
//!    domain-typo corrections against the configured canonical domain
//!    (confidence 0.78 – 0.90 depending on signal strength).
//! 4. Commit-SHA co-occurrence: the same SHA authored under two emails
//!    (confidence 0.92).

use std::collections::{BTreeMap, HashSet};

use rusqlite::Connection;
use strsim::levenshtein;

use crate::collect::identity::resolver::email_domain_matches;
use crate::core::errors::Result;

/// The threshold separating MED-confidence suggestions (a mere hint) from
/// HIGH-confidence ones (safe to auto-accept, and worth flagging on a report).
///
/// Why: one constant drives the CLI's display label, its `--auto-accept`
/// gating, and the authorship report's risk flag, so the three cannot drift.
/// Test: `tests::the_weakest_noise_signal_stays_below_the_high_cutoff`.
pub const HIGH_CONFIDENCE_CUTOFF: f64 = 0.85;

/// A single alias suggestion ranked by `confidence`.
///
/// Why: the detector runs four passes and dedupes by `(src, dst)` pair; this
/// is the shared shape so each pass produces homogeneous output the dedup
/// step can sort and filter.
/// What: the source email (the one that would be merged away), the
/// destination canonical email (the one kept), the confidence score, and a
/// short human-readable reason.
/// Test: every `tests::*` case in this module.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Suggestion {
    /// Source identity email — merged away when the suggestion is accepted.
    pub src: String,
    /// Destination canonical email — retained when the suggestion is accepted.
    pub dst: String,
    /// Confidence in `0.0..=1.0`; at or above [`HIGH_CONFIDENCE_CUTOFF`] the
    /// pair is HIGH-confidence.
    pub confidence: f64,
    /// Short human-readable reason naming the signal that produced the pair.
    pub reason: String,
}

/// Every suggestion derivable from the `authors` table alone, ranked.
///
/// Why (#6142): the authorship report runs this on every build, so it must
/// stay proportional to the number of DISTINCT IDENTITIES rather than to the
/// number of commits — on a 594 MB extract database the commit-SHA pass
/// (see [`detect_all`]) scans every row, which a report cannot afford.
/// What: runs the same-name, edit-distance, and noise-pattern passes, then
/// dedupes and ranks with `floor` applied. `canonical_domain` enables the
/// `.local` / noreply / domain-typo signals; pass `None` to skip them.
/// Test: `tests::detect_from_authors_finds_same_name_pair`.
///
/// # Errors
///
/// Propagates [`crate::core::errors::TgaError::DbError`] from any query.
pub fn detect_from_authors(
    conn: &Connection,
    canonical_domain: Option<&str>,
    floor: f64,
) -> Result<Vec<Suggestion>> {
    let mut out = Vec::new();
    out.extend(detect_same_name_pairs(conn)?);
    out.extend(detect_edit_distance_pairs(conn)?);
    out.extend(detect_noise_patterns(conn, canonical_domain)?);
    Ok(dedupe_and_rank(out, floor))
}

/// [`detect_from_authors`] plus the commit-SHA co-occurrence pass.
///
/// Why: the CLI can afford a full scan of `commits`, and the same SHA
/// recorded under two emails is the strongest evidence available — git
/// itself recorded both against identical content.
/// What: the three author-table passes plus
/// `detect_commit_sha_cooccurrence`, deduped and ranked together.
/// Test: `tests::detect_all_adds_the_sha_signal`.
///
/// # Errors
///
/// Propagates [`crate::core::errors::TgaError::DbError`] from any query.
pub fn detect_all(
    conn: &Connection,
    canonical_domain: Option<&str>,
    floor: f64,
) -> Result<Vec<Suggestion>> {
    let mut out = Vec::new();
    out.extend(detect_same_name_pairs(conn)?);
    out.extend(detect_edit_distance_pairs(conn)?);
    out.extend(detect_noise_patterns(conn, canonical_domain)?);
    out.extend(detect_commit_sha_cooccurrence(conn)?);
    Ok(dedupe_and_rank(out, floor))
}

/// Signal 1: identical observed display names under two different emails.
///
/// Why: the strongest hint short of an explicit user action; if two authors
/// share `canonical_name` and only the email differs, they are almost
/// certainly the same person.
/// What: groups distinct `(canonical_name, canonical_email)` pairs by name
/// and emits one suggestion per non-canonical email pointing at the
/// alphabetically-first email for the name (a deterministic destination).
/// Test: `tests::same_name_different_email_detected`.
fn detect_same_name_pairs(conn: &Connection) -> Result<Vec<Suggestion>> {
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
/// prefix) and similar near-misses, without merging genuinely different people.
/// What: cross-joins all distinct emails and computes Levenshtein distance on
/// the local-part; requires the domains to be identical or one a suffix of the
/// other, and drops local-parts shorter than three characters.
/// Test: `tests::edit_distance_local_part_detected`.
fn detect_edit_distance_pairs(conn: &Connection) -> Result<Vec<Suggestion>> {
    let mut stmt = conn.prepare("SELECT canonical_email FROM authors")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let emails: Vec<String> = rows.filter_map(|r| r.ok()).collect();

    let mut out: Vec<Suggestion> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for i in 0..emails.len() {
        for j in (i + 1)..emails.len() {
            let a = &emails[i];
            let b = &emails[j];
            let Some((la, da)) = split_email(a) else {
                continue;
            };
            let Some((lb, db_)) = split_email(b) else {
                continue;
            };
            // Identical local-parts are the same-name signal's job, and a very
            // short local-part is a false-positive factory.
            if la == lb || la.len() < 3 || lb.len() < 3 {
                continue;
            }
            let dist = levenshtein(&la, &lb);
            if dist == 0 || dist > 2 {
                continue;
            }
            let domains_match = da == db_ || da.ends_with(&db_) || db_.ends_with(&da);
            if !domains_match {
                continue;
            }
            let confidence = if dist == 1 { 0.85 } else { 0.78 };
            // The alphabetically earlier email is the destination so the
            // suggestion is stable across runs.
            let (src, dst) = if a < b {
                (b.clone(), a.clone())
            } else {
                (a.clone(), b.clone())
            };
            if seen.insert((src.clone(), dst.clone())) {
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
/// Why: these patterns dominate alias-table noise on a real corpus.
/// What:
/// - `<local>@<host>.local` → an identity at `<local>@<canonical_domain>`.
/// - `<id>+<login>@users.noreply.github.com` → an identity whose local-part
///   is the login (HIGH) or contains it (MED).
/// - A domain within edit distance 2 of `canonical_domain` → the corrected
///   address, when that identity exists.
///
/// Every arm requires the destination to already exist in `authors`, so a
/// suggestion never invents an identity.
/// Test: `tests::dotlocal_email_routed_to_canonical_domain`.
fn detect_noise_patterns(
    conn: &Connection,
    canonical_domain: Option<&str>,
) -> Result<Vec<Suggestion>> {
    let mut stmt = conn.prepare("SELECT canonical_email FROM authors")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let emails: Vec<String> = rows.filter_map(|r| r.ok()).collect();

    let mut out: Vec<Suggestion> = Vec::new();
    for email in &emails {
        let Some((local, domain)) = split_email(email) else {
            continue;
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
                for other in &emails {
                    if other == email {
                        continue;
                    }
                    let Some((other_local, _)) = split_email(other) else {
                        continue;
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

/// Signal 4: the same commit SHA attributed to two different emails.
///
/// Why: git itself recorded both emails against identical content — this
/// happens when a commit is cherry-picked or rebased and the author email
/// changes in transit.
/// What: groups `commits` by SHA and emits a 0.92-confidence suggestion for
/// any SHA carrying two or more distinct `author_email` values.
/// Test: `tests::same_sha_two_emails_detected`.
fn detect_commit_sha_cooccurrence(conn: &Connection) -> Result<Vec<Suggestion>> {
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

/// Dedupe on `(src, dst)`, keep the highest-confidence reason, sort
/// descending by confidence, and apply `floor`.
///
/// Why: separate passes produce overlapping pairs; a caller wants one line
/// per pair with the strongest reason, in a stable order.
/// What: folds into a map keyed on the pair, retains the highest score, then
/// sorts by confidence descending and `src` ascending.
/// Test: `tests::dedupe_keeps_highest_confidence`.
pub fn dedupe_and_rank(input: Vec<Suggestion>, floor: f64) -> Vec<Suggestion> {
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

/// Split an email into a lowercased `(local, domain)`, or `None` when
/// either half is missing.
///
/// Test: `tests::split_email_basic`.
pub fn split_email(email: &str) -> Option<(String, String)> {
    let at = email.rfind('@')?;
    let local = email[..at].to_lowercase();
    let domain = email[at + 1..].to_lowercase();
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    Some((local, domain))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;
    use rusqlite::params;

    fn insert_author(db: &Database, name: &str, email: &str) -> i64 {
        db.connection()
            .execute(
                "INSERT INTO authors (canonical_name, canonical_email, aliases) \
                 VALUES (?1, ?2, '[]')",
                params![name, email],
            )
            .expect("insert author");
        db.connection().last_insert_rowid()
    }

    fn insert_commit(db: &Database, sha: &str, author_id: i64) {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_id, author_name, author_email, timestamp, \
                 message, repository) VALUES (?1, ?2, 'n', 'e', '2024-01-01T00:00:00Z', 'm', 'r')",
                params![sha, author_id],
            )
            .expect("insert commit");
    }

    #[test]
    fn same_name_different_email_detected() {
        let db = Database::open_in_memory().expect("open");
        insert_author(&db, "Bob Matsuoka", "bob@matsuoka.com");
        insert_author(&db, "Bob Matsuoka", "robert.matsuoka@duettoresearch.com");
        let out = detect_same_name_pairs(db.connection()).expect("detect");
        assert_eq!(out.len(), 1, "exactly one pair expected, got {out:?}");
        assert!(out[0].confidence >= 0.9);
        assert!(out[0].reason.contains("same canonical_name"));
    }

    #[test]
    fn edit_distance_local_part_detected() {
        let db = Database::open_in_memory().expect("open");
        insert_author(&db, "Alice", "alice@example.com");
        insert_author(&db, "Other", "alicea@example.com");
        let out = detect_edit_distance_pairs(db.connection()).expect("detect");
        assert!(
            out.iter().any(|s| s.reason.contains("edit-distance")),
            "expected an edit-distance suggestion, got {out:?}"
        );
    }

    #[test]
    fn dotlocal_email_routed_to_canonical_domain() {
        let db = Database::open_in_memory().expect("open");
        insert_author(&db, "Bob", "bob@HOST.local");
        insert_author(&db, "Bob", "bob@duettoresearch.com");
        let out =
            detect_noise_patterns(db.connection(), Some("duettoresearch.com")).expect("detect");
        assert!(
            out.iter().any(|s| s.reason.contains(".local hostname")),
            "expected .local suggestion, got {out:?}"
        );
    }

    #[test]
    fn github_noreply_routed_to_login() {
        let db = Database::open_in_memory().expect("open");
        insert_author(
            &db,
            "A",
            "129991831+andreramosduetto@users.noreply.github.com",
        );
        insert_author(&db, "B", "andreramosduetto@duettoresearch.com");
        let out =
            detect_noise_patterns(db.connection(), Some("duettoresearch.com")).expect("detect");
        assert!(
            out.iter().any(|s| s.reason.contains("GitHub noreply")),
            "expected github noreply suggestion, got {out:?}"
        );
    }

    #[test]
    fn domain_typo_detected_against_canonical_domain() {
        let db = Database::open_in_memory().expect("open");
        insert_author(&db, "Carol", "carol@duettoresearh.com"); // typo
        insert_author(&db, "Carol", "carol@duettoresearch.com");
        let out =
            detect_noise_patterns(db.connection(), Some("duettoresearch.com")).expect("detect");
        assert!(
            out.iter().any(|s| s.reason.contains("domain typo")),
            "expected domain-typo suggestion, got {out:?}"
        );
    }

    #[test]
    fn same_sha_two_emails_detected() {
        // The production schema enforces UNIQUE on `commits.sha`, so two rows
        // sharing a SHA cannot be staged against the standard `Database`. The
        // detector reads only `(sha, author_email)`, so a stripped-down table
        // on a fresh connection reproduces the data shape it consumes.
        let conn = rusqlite::Connection::open_in_memory().expect("open");
        conn.execute("CREATE TABLE commits (sha TEXT, author_email TEXT)", [])
            .expect("create commits");
        conn.execute(
            "INSERT INTO commits (sha, author_email) VALUES \
             ('shared-sha', 'a@example.com'), ('shared-sha', 'b@example.com')",
            [],
        )
        .expect("insert");

        let out = detect_commit_sha_cooccurrence(&conn).expect("detect");
        assert!(
            out.iter().any(|s| s.reason.contains("same SHA")),
            "expected commit-SHA co-occurrence, got {out:?}"
        );
    }

    #[test]
    fn dedupe_keeps_highest_confidence() {
        let input = vec![
            Suggestion {
                src: "x".into(),
                dst: "y".into(),
                confidence: 0.7,
                reason: "weak".into(),
            },
            Suggestion {
                src: "x".into(),
                dst: "y".into(),
                confidence: 0.95,
                reason: "strong".into(),
            },
        ];
        let out = dedupe_and_rank(input, 0.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reason, "strong");
        assert!((out[0].confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn confidence_floor_filters() {
        let input = vec![
            Suggestion {
                src: "a".into(),
                dst: "b".into(),
                confidence: 0.6,
                reason: "weak".into(),
            },
            Suggestion {
                src: "c".into(),
                dst: "d".into(),
                confidence: 0.95,
                reason: "strong".into(),
            },
        ];
        let out = dedupe_and_rank(input, 0.85);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].src, "c");
    }

    #[test]
    fn the_weakest_noise_signal_stays_below_the_high_cutoff() {
        // The report flags HIGH pairs only, so a partial GitHub-noreply match
        // (0.78) must not survive a floor set at the cutoff — otherwise the
        // flag would fire on a hint nobody considers actionable.
        let db = Database::open_in_memory().expect("open");
        insert_author(&db, "A", "1+alicedev@users.noreply.github.com");
        insert_author(&db, "B", "alicedevlead@corp.com");
        let below = detect_from_authors(db.connection(), Some("corp.com"), 0.5).expect("detect");
        assert!(
            below.iter().any(|s| s.reason.contains("(partial)")),
            "the fixture must produce the partial-match signal, got {below:?}"
        );
        let at_cutoff =
            detect_from_authors(db.connection(), Some("corp.com"), HIGH_CONFIDENCE_CUTOFF)
                .expect("detect");
        assert!(
            !at_cutoff.iter().any(|s| s.reason.contains("(partial)")),
            "a 0.78 partial match must not reach the HIGH floor, got {at_cutoff:?}"
        );
    }

    #[test]
    fn detect_from_authors_finds_same_name_pair() {
        let db = Database::open_in_memory().expect("open");
        insert_author(&db, "Alice", "alice@corp.com");
        insert_author(&db, "Alice", "alice@personal.com");
        let out =
            detect_from_authors(db.connection(), None, HIGH_CONFIDENCE_CUTOFF).expect("detect");
        assert_eq!(out.len(), 1, "one HIGH pair expected, got {out:?}");
        assert_eq!(out[0].src, "alice@personal.com");
        assert_eq!(out[0].dst, "alice@corp.com");
    }

    #[test]
    fn detect_all_adds_the_sha_signal() {
        // `detect_from_authors` must not read `commits`; `detect_all` must.
        let db = Database::open_in_memory().expect("open");
        let a = insert_author(&db, "A", "aaa@example.com");
        let b = insert_author(&db, "B", "bbb@example.com");
        insert_commit(&db, "sha-a", a);
        insert_commit(&db, "sha-b", b);

        // Neither pass finds anything: different names, distant local-parts,
        // distinct SHAs.
        let from_authors = detect_from_authors(db.connection(), None, 0.5).expect("authors pass");
        assert!(from_authors.is_empty(), "got {from_authors:?}");
        let all = detect_all(db.connection(), None, 0.5).expect("all passes");
        assert!(all.is_empty(), "got {all:?}");
    }

    #[test]
    fn split_email_basic() {
        assert_eq!(
            split_email("Bob@Example.COM"),
            Some(("bob".to_string(), "example.com".to_string()))
        );
        assert_eq!(split_email("no-at"), None);
        assert_eq!(split_email("@nolocal.com"), None);
        assert_eq!(split_email("local@"), None);
    }
}
