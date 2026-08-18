//! Authorship & Key-Person Risk: deterministic tables from the tga-authored
//! authorship artifact, plus the Key Facts block's author/trajectory rows
//! (#5453, #6004).
//!
//! Why: owner ruling (issue #5453, 2026-08-18) — key-man risks render IN a
//! dedicated section, not scattered across Top-Risks rows; the section
//! carries a high-level trailing-12-month development-trajectory narrative
//! and a bus-factor/concentration/single-author-subsystem table. All figures
//! come from `RepositoryReport.authorship`, which `model.rs` already loaded
//! fail-open (#5453/#6004) — a repository whose artifact failed to load
//! carries `None` here and simply contributes no row, its gap already stated
//! in `model.gaps`.
//! What: [`push_authorship_rows`] fills the section's per-repository table;
//! [`fill_authorship_facts`] completes the Key Facts block's
//! `facts_author_count`/`facts_trajectory` rows PR A left as named gaps.
//! Test: `reporter_authorship_tests.rs`.

use super::fill::Scope;
use super::model::ReportModel;
use super::provenance::{Provenance, tag};

/// Push one `authorship_row` per repository carrying a loaded authorship
/// artifact.
///
/// Why: the deterministic half of the Authorship & Key-Person Risk section —
/// bus factor, concentration, and single-author subsystems are exactly the
/// key-man risk figures issue #5453 asked to be rendered in this section
/// rather than folded into Top Risks.
/// What: `au_single_author_subsystems` joins the list (or states "none" as a
/// real, measured fact — an empty list is a genuine finding, not an absence);
/// `au_caveats` restates the derivation's own data-trap disclosures verbatim
/// so the section's limitations travel with its numbers.
/// Test: `reporter_authorship_tests::populates_rows_from_loaded_artifacts`.
pub fn push_authorship_rows(root: &mut Scope, model: &ReportModel) {
    for repo in &model.repositories {
        let Some(a) = &repo.authorship else { continue };
        let mut row = Scope::new();
        row.set("au_app_name", repo.name.clone());
        row.set(
            "au_distinct_authors",
            tag(a.distinct_authors.to_string(), Provenance::Measured),
        );
        row.set(
            "au_bus_factor",
            tag(a.bus_factor.to_string(), Provenance::Measured),
        );
        row.set(
            "au_top_author_share",
            tag(
                format!("{:.0}%", a.top_author_share_pct),
                Provenance::Measured,
            ),
        );
        let subsystems = if a.single_author_subsystems.is_empty() {
            "none".to_string()
        } else {
            a.single_author_subsystems.join(", ")
        };
        row.set(
            "au_single_author_subsystems",
            tag(subsystems, Provenance::Measured),
        );
        if let Some(trend) = a.trajectory_summary() {
            row.set("au_trajectory", tag(trend, Provenance::Measured));
        }
        root.push_block("authorship_row", row);
    }
    if let Some(caveats) = caveats_line(model) {
        root.set("au_caveats", tag(caveats, Provenance::Measured));
    }
}

/// The union of every loaded artifact's caveats, de-duplicated, prefixed for
/// standalone-paragraph rendering.
///
/// Why: the template places `{{au_caveats}}` on its own paragraph line (no
/// wrapping prose) SPECIFICALLY so an unset value renders as the bare
/// honesty marker and is dropped by `polish.rs`'s standalone-paragraph rule
/// (an exact `trimmed == HONESTY_MARKER` match) — wrapping text here would
/// defeat that and leak "not stated in source data" into a report with no
/// authorship data at all. The "Derivation caveats: " label therefore lives
/// in the VALUE, not the template.
fn caveats_line(model: &ReportModel) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    for repo in &model.repositories {
        let Some(a) = &repo.authorship else { continue };
        for c in &a.caveats {
            if !seen.contains(&c.as_str()) {
                seen.push(c);
            }
        }
    }
    if seen.is_empty() {
        None
    } else {
        Some(format!("Derivation caveats: {}", seen.join(" ")))
    }
}

/// Complete the Key Facts block's author-count and trajectory rows (#6004).
///
/// Why: PR A shipped `facts_total_loc`/`facts_complexity_summary`/etc. and
/// left `facts_author_count`/`facts_trajectory` unset (rendering as a named
/// gap) because no authorship data existed yet. This is that completion.
/// What: `facts_author_count` sums `distinct_authors` across every
/// repository with a loaded artifact — a simplification that does not
/// de-duplicate an author active in more than one repository (caveated
/// inline, since `AuthorshipSummary` carries no cross-repo author identity).
/// `facts_trajectory` states the single repository's trend when there is
/// exactly one, or a qualified "see Authorship section" pointer when there
/// are several (a single merged sentence would blur per-repo trends that
/// disagree). `facts_work_estimate` stays unset — see `reporter_facts.rs`'s
/// module doc; tga has no effort-estimation metric.
/// Test: `reporter_authorship_tests::completes_key_facts_author_rows`.
pub fn fill_authorship_facts(root: &mut Scope, model: &ReportModel) {
    let with_authorship: Vec<&super::authorship::AuthorshipSummary> = model
        .repositories
        .iter()
        .filter_map(|r| r.authorship.as_ref())
        .collect();
    if with_authorship.is_empty() {
        return;
    }

    let total_authors: u64 = with_authorship.iter().map(|a| a.distinct_authors).sum();
    let note = if with_authorship.len() > 1 {
        " (sum across repositories — an author active in more than one is counted once per \
         repository)"
    } else {
        ""
    };
    root.set(
        "facts_author_count",
        tag(format!("{total_authors}{note}"), Provenance::Measured),
    );

    let trajectory = match with_authorship.as_slice() {
        [only] => only.trajectory_summary(),
        _ => Some("varies by application — see Authorship & Key-Person Risk".to_string()),
    };
    if let Some(t) = trajectory {
        root.set("facts_trajectory", tag(t, Provenance::Measured));
    }
}

#[cfg(test)]
#[path = "reporter_authorship_tests.rs"]
mod tests;
