//! The tga → trusty-review authorship artifact (#5453, #6004).
//!
//! Why: owner ruling 2026-08-18 — the DD report needs a dedicated Authorship &
//! Key-Person Risk section carrying ownership concentration, bus factor,
//! single-author subsystems, and a high-level trailing-12-month trajectory.
//! #5468 (CLOSED) ruled all contributor-profiling derivation lives in tga,
//! never trusty-review — this module is that derivation. It mirrors
//! [`super::ticketing`]'s read-side shape (a pure query-and-serialize builder;
//! the caller writes the file), but is PER-REPOSITORY rather than
//! engagement-wide, because `commits.repository` distinguishes repositories
//! within tga's one-database-per-engagement audit flow and ownership is a
//! per-codebase question — mirrors the precedent
//! `RepositoryEntry.velocity: Option<PathBuf>` (DOC-67 §8 velocity spec) sets
//! for a per-repo artifact field, rather than ticketing's engagement-wide one.
//!
//! ## The five data traps (issue #5453) — what this module handles vs. caveats
//!
//! - **Bot commits** — HANDLED. [`is_bot`] excludes machine-authored commits
//!   (`dependabot`, `renovate[bot]`, `github-actions[bot]`, and similar) by a
//!   name/email pattern match before any aggregation runs.
//! - **Merge commits** — HANDLED. The query filters `is_merge = 0`; an
//!   inflated merger count never reaches the aggregation.
//! - **Squash-merge attribution, no `.mailmap` identity merging, vendored-path
//!   exclusion** — NOT handled (each needs, respectively: nothing extra for
//!   GitHub squash since `commits.author_name`/`author_email` already read
//!   the PR author verbatim, per issue #5453's own finding — the residual
//!   risk is a LOCAL `git merge --squash` by a human, which this derivation
//!   cannot distinguish from a real commit; a per-engagement `.mailmap`/alias
//!   table, which does not exist in this schema; and a vendored-path filter,
//!   which needs a configurable ignore-list this module does not have). Named
//!   explicitly in [`CAVEATS`], which the report section
//!   renders verbatim rather than silently omitting the limitation.
//!
//! What: [`AuthorshipSummary`], [`build_authorship_summary`] which reads it
//! from an open database for one repository, and
//! [`AuthorshipSummary::to_json`].
//! Test: `super::authorship_tests`.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::core::errors::Result;

/// Schema tag written into the artifact — pairs with trusty-review's
/// `report::authorship::SUPPORTED_SCHEMA_MAJOR`.
pub const AUTHORSHIP_SCHEMA_VERSION: &str = "v0";

/// One repository's ownership/bus-factor/trajectory figures.
///
/// Why: this is the whole tga→trusty-review authorship seam. Every figure is
/// derived from `commits JOIN files`, never invented or estimated beyond the
/// stated approximations (see field docs).
/// What: `bus_factor` — the smallest number of top-touching authors whose
/// combined share reaches 50% of all counted file-touches; `top_author_share`
/// — the single largest author's share of all touches, as a percentage;
/// `single_author_subsystems` — top-level path segments touched by exactly
/// one non-bot author; `distinct_authors` and `monthly_trajectory` feed the
/// report's Key Facts block and the trailing-12-month narrative.
/// Test: `super::authorship_tests::builds_from_seeded_commits`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct AuthorshipSummary {
    /// Artifact schema tag; always [`AUTHORSHIP_SCHEMA_VERSION`].
    pub schema_version: String,
    /// The repository name these figures describe (matches `commits.repository`).
    pub repository: String,
    /// Distinct non-bot authors with at least one non-merge commit.
    pub distinct_authors: u64,
    /// Smallest number of top-touching authors whose combined file-touch
    /// share reaches 50%. `0` when there is no data.
    pub bus_factor: u64,
    /// The single largest author's share of all file-touches, in `0.0..=100.0`.
    pub top_author_share_pct: f64,
    /// Top-level path segments (e.g. `src`, `docs`) touched by exactly one
    /// non-bot author, sorted.
    pub single_author_subsystems: Vec<String>,
    /// One entry per active month in the trailing 12 months, oldest first.
    pub monthly_trajectory: Vec<MonthlyActivity>,
    /// Data-trap limitations this run did NOT correct for (issue #5453) —
    /// rendered verbatim by the report section's caption.
    pub caveats: Vec<String>,
}

/// One month's active-author/commit-volume figures.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MonthlyActivity {
    /// `YYYY-MM`.
    pub month: String,
    /// Distinct non-bot authors with a non-merge commit in this month.
    pub active_authors: u64,
    /// Non-merge, non-bot commits in this month.
    pub commits: u64,
}

/// The standing caveats every artifact this module writes carries (#5453).
const CAVEATS: &[&str] = &[
    "Squash-merge attribution: a GitHub squash-merge preserves the PR author, but a local \
     `git merge --squash` by a human does not — this run cannot distinguish the two.",
    "No .mailmap / identity-alias merging: one person committing under multiple names or \
     emails is counted as multiple authors, which UNDERSTATES concentration and bus factor.",
    "No vendored-path exclusion: a checked-in vendor/dependency directory can make its \
     committer look like the sole owner of thousands of paths.",
];

/// Name/email substrings identifying a machine-authored commit (issue #5453).
///
/// Why: a case-insensitive substring match is deliberately permissive — a
/// missed bot inflates a human's apparent ownership, which is the more
/// dangerous direction of error for a key-man risk figure.
const BOT_MARKERS: &[&str] = &[
    "[bot]",
    "dependabot",
    "renovate",
    "github-actions",
    "gitlab-ci",
    "greenkeeper",
];

/// True when `name` or `email` identifies a machine author.
fn is_bot(name: &str, email: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let email = email.to_ascii_lowercase();
    BOT_MARKERS
        .iter()
        .any(|m| name.contains(m) || email.contains(m))
}

/// The top-level path segment of a file path — this module's "subsystem".
fn subsystem_of(path: &str) -> String {
    path.split('/').next().unwrap_or(path).to_string()
}

/// The `YYYY-MM` key of an ISO-8601 timestamp (first 7 characters).
fn month_of(timestamp: &str) -> Option<String> {
    timestamp.get(0..7).map(str::to_string)
}

/// Build one repository's authorship summary from an open database.
///
/// Why: the single function that reads `commits JOIN files` for this artifact
/// — every other item in this module is a pure helper it calls.
/// What: reads every non-merge commit for `repository`, drops bot-authored
/// rows (issue #5453), then aggregates: per-author file-touch counts (for
/// bus factor / concentration), per-subsystem author sets (for single-author
/// subsystems), and per-month distinct-author/commit counts limited to the
/// most recent 12 active months (for the trajectory).
/// Test: `super::authorship_tests::{builds_from_seeded_commits,
/// bots_and_merges_are_excluded, single_author_subsystem_detected}`.
///
/// # Errors
///
/// Propagates [`crate::core::errors::TgaError::DbError`] from either query.
pub fn build_authorship_summary(conn: &Connection, repository: &str) -> Result<AuthorshipSummary> {
    let mut stmt = conn.prepare(
        "SELECT c.author_name, c.author_email, c.timestamp, f.path \
         FROM commits c JOIN files f ON f.commit_id = c.id \
         WHERE c.repository = ?1 AND c.is_merge = 0",
    )?;
    let rows = stmt.query_map(params![repository], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut touches_by_author: BTreeMap<String, u64> = BTreeMap::new();
    let mut authors_by_subsystem: BTreeMap<String, std::collections::BTreeSet<String>> =
        BTreeMap::new();
    let mut months: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    let mut month_commits: BTreeMap<String, u64> = BTreeMap::new();

    for row in rows {
        let (name, email, timestamp, path) = row?;
        if is_bot(&name, &email) {
            continue;
        }
        let author_key = if email.is_empty() {
            name.clone()
        } else {
            email.clone()
        };
        *touches_by_author.entry(author_key.clone()).or_insert(0) += 1;
        authors_by_subsystem
            .entry(subsystem_of(&path))
            .or_default()
            .insert(author_key.clone());
        if let Some(month) = month_of(&timestamp) {
            months.entry(month.clone()).or_default().insert(author_key);
            *month_commits.entry(month).or_insert(0) += 1;
        }
    }

    let total_touches: u64 = touches_by_author.values().sum();
    let mut ranked: Vec<(&String, &u64)> = touches_by_author.iter().collect();
    ranked.sort_by_key(|(_, n)| std::cmp::Reverse(**n));

    let top_author_share_pct = match ranked.first() {
        Some((_, n)) if total_touches > 0 => (**n as f64 / total_touches as f64) * 100.0,
        _ => 0.0,
    };
    let bus_factor = bus_factor_of(&ranked, total_touches);

    let mut single_author_subsystems: Vec<String> = authors_by_subsystem
        .into_iter()
        .filter(|(_, authors)| authors.len() == 1)
        .map(|(subsystem, _)| subsystem)
        .collect();
    single_author_subsystems.sort();

    // Trailing 12 active months, oldest first — matches the report's
    // "trajectory by month" framing without depending on wall-clock "now",
    // which would make two runs of the same database disagree.
    let mut month_keys: Vec<String> = months.keys().cloned().collect();
    month_keys.sort();
    let recent: Vec<String> = month_keys.into_iter().rev().take(12).rev().collect();
    let monthly_trajectory = recent
        .into_iter()
        .map(|month| MonthlyActivity {
            active_authors: months.get(&month).map(|s| s.len() as u64).unwrap_or(0),
            commits: month_commits.get(&month).copied().unwrap_or(0),
            month,
        })
        .collect();

    Ok(AuthorshipSummary {
        schema_version: AUTHORSHIP_SCHEMA_VERSION.to_string(),
        repository: repository.to_string(),
        distinct_authors: touches_by_author.len() as u64,
        bus_factor,
        top_author_share_pct,
        single_author_subsystems,
        monthly_trajectory,
        caveats: CAVEATS.iter().map(|s| s.to_string()).collect(),
    })
}

/// The smallest number of top-ranked authors whose combined share reaches 50%.
///
/// Why: the classic "bus factor" heuristic — how many people, if all left,
/// would take half the project's institutional knowledge with them.
/// What: `0` with no data; otherwise walks `ranked` (already sorted
/// descending) accumulating touches until the running total reaches half of
/// `total`.
fn bus_factor_of(ranked: &[(&String, &u64)], total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    let half = total as f64 / 2.0;
    let mut running = 0u64;
    for (i, (_, n)) in ranked.iter().enumerate() {
        running += **n;
        if running as f64 >= half {
            return (i + 1) as u64;
        }
    }
    ranked.len() as u64
}

impl AuthorshipSummary {
    /// Serialize to the JSON text trusty-review's loader reads.
    ///
    /// Why: keeping serialization here means the artifact's shape is testable
    /// without touching disk, exactly as [`super::ticketing`] does.
    /// What: `serde_json::to_string_pretty` over the declared field order.
    ///
    /// # Errors
    ///
    /// [`crate::core::errors::TgaError`] when serialization fails.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

#[cfg(test)]
#[path = "authorship_tests.rs"]
mod authorship_tests;
