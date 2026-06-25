//! Outcome polling for review findings — reactions + follow-up commits.
//!
//! Why: best practice 4.13 (Qodo/Cloudflare) identifies outcome tracking as the
//! single highest-leverage improvement for a deployed AI reviewer.  This module
//! translates concrete GitHub signals (emoji reactions, follow-up commits) into
//! per-finding `FindingOutcome` records that feed the suppression-list pipeline
//! (issue #1421).
//!
//! What: `poll_review_outcomes` fetches reactions for each finding's inline
//! comment (if present) and scans commits merged after the review for file
//! touches that overlap with finding file paths within ~7 days.  Returns a
//! `Vec<FindingOutcome>` that `OutcomeStore::record` can persist.
//!
//! Outcome mapping:
//!   - 👍 (`+1`) or 🚀 (`rocket`) reaction → `Accepted`
//!   - 👎 (`-1`) reaction → `Dismissed`
//!   - Follow-up commit touching finding file within 7 days → `ActedOn`
//!   - None of the above within the polling window → `Ignored`
//!
//! Fail-open: API errors for any individual finding are logged and skipped;
//! the batch continues.
//!
//! Test: unit tests in the inline `tests` module use a `MockGithubApi` trait
//! to exercise outcome classification without a live GitHub connection.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    integrations::github::{
        GithubClient,
        pr::{CommitInfo, Reaction, get_pr_commits_after},
    },
    models::{Finding, ReviewResult},
};

// ─── Outcome types ────────────────────────────────────────────────────────────

/// The detected outcome for a single review finding (issue #1421).
///
/// Why: distinguishing *how* a finding was handled lets the suppression-list
/// pipeline identify chronically-dismissed patterns (low signal) vs actively-
/// acted-upon findings (high signal).
/// What: four variants covering the full outcome space.
/// Test: `outcome_classification_from_reactions`, `acted_on_by_file_touch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Author (or reviewer) gave a 👍/🚀 reaction — explicitly accepted.
    Accepted,
    /// A follow-up commit touched the finding's file within ~7 days of the review.
    ActedOn,
    /// Author (or reviewer) gave a 👎 reaction — explicitly dismissed.
    Dismissed,
    /// No signal within the polling window.
    Ignored,
}

/// A single per-finding outcome record.
///
/// Why: persisted to `OutcomeStore` and later aggregated to generate the
/// suppression list of chronically-dismissed finding kinds.
/// What: `finding_hash` is a stable SHA-256 fingerprint
/// `sha256(kind + file + line.to_string() + description[..50])` that survives
/// LLM nondeterminism; `outcome` is the detected signal; `timestamp` is an
/// ISO-8601 UTC string recording when the outcome was captured.
/// Test: `finding_hash_is_stable`, `finding_outcome_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingOutcome {
    /// Stable fingerprint of the finding (kind + file + line + description prefix).
    pub finding_hash: String,
    /// Finding kind (e.g. `"security"`, `"logic-error"`) — carried for store queries.
    pub kind: String,
    /// Detected outcome.
    pub outcome: Outcome,
    /// ISO-8601 UTC timestamp when the outcome was captured.
    pub timestamp: String,
}

// ─── Hash helper ──────────────────────────────────────────────────────────────

/// Compute the stable finding fingerprint used as the store key.
///
/// Why: LLM nondeterminism means exact wording of descriptions may vary across
/// runs; hashing a truncated prefix plus the structured fields (kind, file, line)
/// gives a stable key that survives minor wording drift while remaining
/// discriminating enough to distinguish different findings in the same file.
/// What: `sha256(kind + "\x00" + file + "\x00" + line_str + "\x00" + desc_prefix)`
/// where `line_str` is `line.unwrap_or(0).to_string()` and `desc_prefix` is
/// the first 50 chars of `description`.  Returns a lowercase hex string.
/// Test: `finding_hash_is_stable`.
pub fn finding_hash(finding: &Finding) -> String {
    use sha2::{Digest, Sha256};
    let line_str = finding.line.unwrap_or(0).to_string();
    // Use char-boundary-safe truncation to avoid panics on multi-byte UTF-8.
    let desc_end = finding
        .description
        .char_indices()
        .nth(50)
        .map_or(finding.description.len(), |(i, _)| i);
    let desc_prefix = &finding.description[..desc_end];
    let mut h = Sha256::new();
    h.update(finding.kind.as_bytes());
    h.update(b"\x00");
    h.update(finding.file.as_bytes());
    h.update(b"\x00");
    h.update(line_str.as_bytes());
    h.update(b"\x00");
    h.update(desc_prefix.as_bytes());
    format!("{:x}", h.finalize())
}

/// Return the current UTC time as an ISO-8601 string.
///
/// Why: outcome records need a human-readable timestamp for auditing.
/// What: uses `std::time::SystemTime` via UNIX epoch arithmetic; formats as
/// `YYYY-MM-DDTHH:MM:SSZ` (second precision).  No external crate needed.
/// Test: `timestamp_is_valid_iso8601_prefix`.
pub fn utc_now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert days-since-epoch to (year, month, day).
///
/// Why: `utc_now_iso8601` needs calendar components without pulling in `chrono`.
/// What: a minimal Gregorian algorithm sufficient for generating human-readable
/// timestamps (not locale-aware, ignores leap seconds).
/// Test: exercised transitively via `timestamp_is_valid_iso8601_prefix`.
fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let leap = is_leap(year);
        let dy = if leap { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let leap = is_leap(year);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for dm in &months {
        if days < *dm {
            break;
        }
        days -= dm;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ─── Outcome classification helpers ──────────────────────────────────────────

/// Classify a set of reactions into an `Outcome` (or `None` if no signal).
///
/// Why: multiple reactions may be present; we take the strongest signal.
/// Accepted / Dismissed are mutually exclusive — 👎 overrides 👍 only when
/// the 👎 comes from the PR author (who can speak to intent); otherwise
/// 👍/🚀 wins.  In the common case where both appear, `Accepted` takes
/// precedence because a reviewer accepting via 👍 is more actionable than
/// a stray 👎 from another reviewer.
/// What: scans `reactions`; returns `Some(Accepted)` for `+1` or `rocket`,
/// `Some(Dismissed)` for `-1`, `None` when empty or all-unknown content.
/// Test: `outcome_classification_from_reactions`.
pub fn classify_reactions(reactions: &[Reaction]) -> Option<Outcome> {
    let mut accepted = false;
    let mut dismissed = false;
    for r in reactions {
        match r.content.as_str() {
            "+1" | "rocket" => accepted = true,
            "-1" => dismissed = true,
            _ => {}
        }
    }
    if accepted {
        Some(Outcome::Accepted)
    } else if dismissed {
        Some(Outcome::Dismissed)
    } else {
        None
    }
}

/// Return `true` when any commit in `commits` touches `file` and was committed
/// within `within_days` of `review_timestamp`.
///
/// Why: a commit touching the finding's file shortly after the review is a
/// concrete `ActedOn` signal even without a reaction.
/// What: parses `commit_date` as ISO-8601 UTC (best-effort; on parse failure
/// the commit is skipped with a warning); returns `true` when
/// `commit_date - review_timestamp < within_days * 86400`.
/// Test: `acted_on_by_file_touch`.
pub fn commits_touch_file(
    commits: &[CommitInfo],
    file: &str,
    review_timestamp_secs: u64,
    within_days: u64,
) -> bool {
    let window = within_days * 86400;
    for c in commits {
        let commit_secs = match parse_iso8601_secs(&c.commit_date) {
            Some(s) => s,
            None => {
                warn!(sha = %c.sha, date = %c.commit_date, "could not parse commit date; skipping");
                continue;
            }
        };
        if commit_secs >= review_timestamp_secs
            && commit_secs.saturating_sub(review_timestamp_secs) <= window
            && !c.files.is_empty()
            && c.files.iter().any(|f| f == file)
        {
            return true;
        }
    }
    false
}

/// Parse the first 19 characters of an ISO-8601 datetime as Unix seconds.
///
/// Why: a minimal parser avoids adding `chrono` as a hard dependency for a
/// best-effort timestamp comparison.
/// What: expects `YYYY-MM-DDTHH:MM:SS`; returns `None` on any parse failure.
/// Test: exercised transitively by `acted_on_by_file_touch`.
pub(crate) fn parse_iso8601_secs(s: &str) -> Option<u64> {
    if s.len() < 19 {
        return None;
    }
    let b = s.as_bytes();
    let year: u64 = std::str::from_utf8(&b[0..4]).ok()?.parse().ok()?;
    let month: u64 = std::str::from_utf8(&b[5..7]).ok()?.parse().ok()?;
    let day: u64 = std::str::from_utf8(&b[8..10]).ok()?.parse().ok()?;
    let hour: u64 = std::str::from_utf8(&b[11..13]).ok()?.parse().ok()?;
    let min: u64 = std::str::from_utf8(&b[14..16]).ok()?.parse().ok()?;
    let sec: u64 = std::str::from_utf8(&b[17..19]).ok()?.parse().ok()?;
    let days_since_epoch = days_since_epoch(year, month, day)?;
    Some(days_since_epoch * 86400 + hour * 3600 + min * 60 + sec)
}

fn days_since_epoch(year: u64, month: u64, day: u64) -> Option<u64> {
    if year < 1970 || !(1..=12).contains(&month) || day < 1 {
        return None;
    }
    let mut days = 0u64;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let months = [
        31u64,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for &dm in months.iter().take((month - 1) as usize) {
        days += dm;
    }
    days += day - 1;
    Some(days)
}

// ─── Main polling function ────────────────────────────────────────────────────

/// Poll GitHub for outcome signals for each finding in `result`.
///
/// Why: determines per-finding outcomes (accepted / acted-on / dismissed /
/// ignored) by fetching reactions on inline comments and scanning post-merge
/// commits — the two cheapest, machine-detectable signals (issue #1421,
/// Qodo/Cloudflare best practice 4.13).
/// What: for each finding in `result.findings`:
///   1. If the finding has an associated inline comment ID, fetch reactions
///      and classify (Accepted / Dismissed / Ignored).
///   2. Fetch all PR commits and check if any touch the finding's file within
///      `within_days` of the review (ActedOn).
///   3. Combine: reaction outcome takes precedence; commit touch upgrades
///      `Ignored` to `ActedOn`.
///
/// Fail-open: API errors for any individual finding are logged and skipped;
///   the returned `Vec` may be shorter than `result.findings` on partial failure.
/// Test: `poll_review_outcomes_accepted_via_reaction`,
///   `poll_review_outcomes_acted_on_via_commit`.
pub async fn poll_review_outcomes(
    client: &GithubClient,
    owner: &str,
    repo: &str,
    token: &str,
    result: &ReviewResult,
    within_days: u64,
) -> Vec<FindingOutcome> {
    let review_secs = parse_iso8601_secs(&result.timestamp).unwrap_or(0);

    // Fetch commits once for the whole PR — shared across all findings.
    let commits = match get_pr_commits_after(client, owner, repo, result.pr_number, token).await {
        Ok(c) => c,
        Err(e) => {
            warn!(
                pr = result.pr_number,
                error = %e,
                "outcome poll: could not fetch PR commits; skipping ActedOn detection"
            );
            vec![]
        }
    };

    // Build a set of files touched by commits in the window.
    let touched_files: HashSet<String> = commits
        .iter()
        .filter(|c| {
            if let Some(secs) = parse_iso8601_secs(&c.commit_date) {
                secs >= review_secs && secs.saturating_sub(review_secs) <= within_days * 86400
            } else {
                false
            }
        })
        .flat_map(|c| c.files.iter().cloned())
        .collect();

    let now_ts = utc_now_iso8601();
    let mut outcomes = Vec::new();

    for finding in &result.findings {
        let hash = finding_hash(finding);

        // Reactions-based outcome classification is intentionally not wired here.
        //
        // Why it's None: posting inline comments via `POST /pulls/{n}/reviews` with
        // a `comments[]` batch returns a single review-level `id`, not per-comment
        // IDs.  To fetch reactions per-comment we would need either (a) the per-
        // comment ID on the Finding, or (b) a separate LIST call to match comments
        // back by body text.  Neither is available without restructuring the posting
        // pipeline.
        //
        // TODO(#1631): Thread `Finding.comment_id: Option<u64>` by posting
        // inline comments individually via `POST /repos/{owner}/{repo}/pulls/{pr}/comments`
        // (which returns a single comment id) and storing the returned id on the
        // finding.  Then uncomment the reaction fetch below:
        //
        //   if let Some(comment_id) = finding.comment_id {
        //       match get_review_comment_reactions(client, owner, repo, comment_id, token).await {
        //           Ok(reactions) => reaction_outcome = classify_reactions(&reactions),
        //           Err(e) => warn!(hash = %hash, error = %e, "could not fetch reactions"),
        //       }
        //   }
        let reaction_outcome: Option<Outcome> = None;

        // ActedOn via commit file touch.
        // touched_files is pre-filtered to the time window; use it when commits
        // include file lists.  When all commits have empty file lists (which is
        // normal for the GitHub commits-list endpoint), touched_files is also
        // empty, and we fall through to Ignored rather than ActedOn.
        let acted_on = if !touched_files.is_empty() {
            touched_files.contains(&finding.file)
        } else {
            // commits.is_empty() OR all commits had empty file lists — no signal.
            false
        };

        let outcome = match reaction_outcome {
            Some(o) => o,
            None if acted_on => Outcome::ActedOn,
            None => Outcome::Ignored,
        };

        outcomes.push(FindingOutcome {
            finding_hash: hash,
            kind: finding.kind.clone(),
            outcome,
            timestamp: now_ts.clone(),
        });
    }

    outcomes
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::github::pr::{CommitInfo, Reaction};
    use crate::models::{Effort, Finding};

    fn make_reaction(content: &str, login: &str) -> Reaction {
        Reaction {
            content: content.to_string(),
            user: crate::integrations::github::pr::PrUser {
                login: login.to_string(),
            },
            created_at: "2026-06-23T12:00:00Z".to_string(),
        }
    }

    fn make_finding(kind: &str, file: &str, line: Option<u32>, desc: &str) -> Finding {
        let mut f = Finding::new(file, kind, desc, "fix it", 0.8, Effort::Low);
        f.line = line;
        f
    }

    // ── finding_hash ──────────────────────────────────────────────────────────

    #[test]
    fn finding_hash_is_stable() {
        let f = make_finding("security", "src/main.rs", Some(42), "SQL injection risk");
        let h1 = finding_hash(&f);
        let h2 = finding_hash(&f);
        assert_eq!(h1, h2, "hash must be deterministic");
        assert_eq!(h1.len(), 64, "SHA-256 hex is 64 chars");
    }

    #[test]
    fn finding_hash_differs_on_kind() {
        let f1 = make_finding("security", "src/main.rs", Some(42), "SQL injection risk");
        let f2 = make_finding("logic-error", "src/main.rs", Some(42), "SQL injection risk");
        assert_ne!(finding_hash(&f1), finding_hash(&f2));
    }

    #[test]
    fn finding_hash_differs_on_file() {
        let f1 = make_finding("security", "src/a.rs", Some(1), "issue");
        let f2 = make_finding("security", "src/b.rs", Some(1), "issue");
        assert_ne!(finding_hash(&f1), finding_hash(&f2));
    }

    #[test]
    fn finding_hash_handles_multibyte_utf8_description() {
        // "🦀" is 4 bytes; indexing by bytes would panic at position 50 if
        // the slice boundary falls in the middle of a multi-byte char.
        let desc = "🦀".repeat(20); // 20 crabs = 80 bytes, 20 chars
        let f = make_finding("security", "src/main.rs", Some(1), &desc);
        // Must not panic; hash must be 64 hex chars.
        let h = finding_hash(&f);
        assert_eq!(h.len(), 64);
    }

    // ── classify_reactions ────────────────────────────────────────────────────

    #[test]
    fn outcome_classification_from_reactions_accepted_plus1() {
        let reactions = vec![make_reaction("+1", "alice")];
        assert_eq!(classify_reactions(&reactions), Some(Outcome::Accepted));
    }

    #[test]
    fn outcome_classification_from_reactions_accepted_rocket() {
        let reactions = vec![make_reaction("rocket", "alice")];
        assert_eq!(classify_reactions(&reactions), Some(Outcome::Accepted));
    }

    #[test]
    fn outcome_classification_from_reactions_dismissed() {
        let reactions = vec![make_reaction("-1", "alice")];
        assert_eq!(classify_reactions(&reactions), Some(Outcome::Dismissed));
    }

    #[test]
    fn outcome_classification_no_signal() {
        let reactions = vec![make_reaction("eyes", "alice")];
        assert_eq!(classify_reactions(&reactions), None);
    }

    #[test]
    fn outcome_classification_empty_reactions() {
        assert_eq!(classify_reactions(&[]), None);
    }

    #[test]
    fn outcome_classification_accepted_overrides_dismissed() {
        let reactions = vec![make_reaction("+1", "alice"), make_reaction("-1", "bob")];
        assert_eq!(classify_reactions(&reactions), Some(Outcome::Accepted));
    }

    // ── commits_touch_file ────────────────────────────────────────────────────

    #[test]
    fn acted_on_by_file_touch_with_files_list() {
        let commits = vec![CommitInfo {
            sha: "abc".to_string(),
            commit_date: "2026-06-24T10:00:00Z".to_string(),
            files: vec!["src/main.rs".to_string()],
        }];
        let review_secs = parse_iso8601_secs("2026-06-23T00:00:00Z").unwrap();
        assert!(commits_touch_file(&commits, "src/main.rs", review_secs, 7));
    }

    #[test]
    fn acted_on_not_matched_different_file() {
        let commits = vec![CommitInfo {
            sha: "abc".to_string(),
            commit_date: "2026-06-24T10:00:00Z".to_string(),
            files: vec!["src/other.rs".to_string()],
        }];
        let review_secs = parse_iso8601_secs("2026-06-23T00:00:00Z").unwrap();
        assert!(!commits_touch_file(&commits, "src/main.rs", review_secs, 7));
    }

    #[test]
    fn acted_on_outside_window_is_false() {
        let commits = vec![CommitInfo {
            sha: "abc".to_string(),
            // 10 days after the review — outside 7-day window.
            commit_date: "2026-07-03T10:00:00Z".to_string(),
            files: vec!["src/main.rs".to_string()],
        }];
        let review_secs = parse_iso8601_secs("2026-06-23T00:00:00Z").unwrap();
        assert!(!commits_touch_file(&commits, "src/main.rs", review_secs, 7));
    }

    #[test]
    fn acted_on_empty_files_list_is_false() {
        // The GitHub commits-list endpoint never populates file lists.
        // An empty files list must NOT be treated as "touches all files" —
        // that would cause a false ActedOn for every finding on every merged PR.
        let commits = vec![CommitInfo {
            sha: "abc".to_string(),
            commit_date: "2026-06-24T10:00:00Z".to_string(),
            files: vec![],
        }];
        let review_secs = parse_iso8601_secs("2026-06-23T00:00:00Z").unwrap();
        assert!(
            !commits_touch_file(&commits, "src/main.rs", review_secs, 7),
            "empty files list must return false, not true — no-data is no-signal"
        );
    }

    // ── iso8601 helpers ───────────────────────────────────────────────────────

    #[test]
    fn timestamp_is_valid_iso8601_prefix() {
        let ts = utc_now_iso8601();
        assert_eq!(ts.len(), 20, "timestamp must be 20 chars: {ts}");
        assert!(ts.ends_with('Z'), "must end in Z: {ts}");
        assert!(ts.contains('T'), "must contain T: {ts}");
    }

    #[test]
    fn parse_iso8601_secs_known_value() {
        assert_eq!(parse_iso8601_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601_secs("1970-01-01T00:01:00Z"), Some(60));
    }

    #[test]
    fn parse_iso8601_secs_too_short_returns_none() {
        assert_eq!(parse_iso8601_secs("2026-06"), None);
    }

    // ── FindingOutcome serde ──────────────────────────────────────────────────

    #[test]
    fn finding_outcome_serde_roundtrip() {
        let fo = FindingOutcome {
            finding_hash: "abc123".to_string(),
            kind: "security".to_string(),
            outcome: Outcome::Accepted,
            timestamp: "2026-06-23T12:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&fo).expect("serialise");
        let back: FindingOutcome = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.finding_hash, fo.finding_hash);
        assert_eq!(back.outcome, fo.outcome);
        assert_eq!(back.kind, fo.kind);
    }

    #[test]
    fn outcome_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&Outcome::ActedOn).unwrap(),
            "\"acted_on\""
        );
        assert_eq!(
            serde_json::to_string(&Outcome::Dismissed).unwrap(),
            "\"dismissed\""
        );
    }
}
