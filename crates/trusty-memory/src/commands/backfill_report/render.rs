//! Output rendering for the backfill report.
//!
//! Why: the ticket's bar is that the output be actionable for a human deciding
//! **one drawer at a time**. A fixed-width table cannot meet it — the excerpt is
//! 220 characters, and truncating it to a column width would hide the very text
//! the decision turns on. So the default output is one stanza per drawer, in
//! ranked order, each carrying everything the decision needs and nothing else.
//! `--json` exists for the other consumer: piping a triage list into a script.
//!
//! What: `render_text` writes a header stating the scan's coverage and the
//! tool's read-only contract, then a stanza per row. `render_json` writes the
//! same data as one object. Both take `&mut dyn Write` so tests assert on a
//! buffer rather than on stdout.
//!
//! Test: `text_output_leads_with_the_read_only_contract`,
//! `stanza_carries_every_decision_field`, `json_round_trips`,
//! `empty_census_says_why`.

use std::io::Write;

use anyhow::Result;
use serde_json::json;

use super::candidates::{short_id, Census};
use super::log_index::ScanStats;

/// Width the excerpt is wrapped to, before the stanza indent.
const EXCERPT_WIDTH: usize = 88;
/// Indent applied to every continuation line of a wrapped excerpt.
const EXCERPT_INDENT: &str = "                 ";

/// Render the human-facing report.
///
/// Why: a reader who runs this without having read ADR-0028 must still learn the
/// two facts that make the numbers mean anything — that the tool changes
/// nothing, and that the existing drawers it lists are not migrated by the ADR,
/// so this report is the only route by which they get addressed.
/// What: header, coverage line, per-palace read outcomes when any failed, then
/// the ranked stanzas.
/// Test: `text_output_leads_with_the_read_only_contract`, `empty_census_says_why`.
pub fn render_text(
    out: &mut dyn Write,
    census: &Census,
    stats: &ScanStats,
    limit: usize,
) -> Result<()> {
    writeln!(out, "ADR-0028 backfill classification report")?;
    writeln!(
        out,
        "READ-ONLY. This command never writes a drawer, never sets expires_at, and never"
    )?;
    writeln!(
        out,
        "retires anything. ADR-0028 does not migrate the drawers already on disk — they"
    )?;
    writeln!(
        out,
        "keep competing in L1 exactly as before — so acting on a row below is a human"
    )?;
    writeln!(out, "decision, taken one drawer at a time.")?;
    writeln!(out)?;
    render_coverage(out, census, stats)?;
    writeln!(out)?;

    if census.rows.is_empty() {
        writeln!(
            out,
            "No drawer met the filter. {}",
            if stats.files_scanned == 0 {
                "No hook log was found, so every injection count would be 0 — \
                 this is missing data, not an absence of stale drawers."
            } else {
                "Lower --min-injections to widen the list."
            }
        )?;
        return Ok(());
    }

    for (i, row) in census.rows.iter().take(limit).enumerate() {
        writeln!(
            out,
            "#{rank}  {id}  {palace} / {room}",
            rank = i + 1,
            id = short_id(&row.drawer_id),
            palace = row.palace,
            room = row.room
        )?;
        // The coverage line counts injections estate-wide; this percentage is
        // against the drawer's OWN palace. Name the denominator here so the two
        // numbers cannot be read as contradicting each other.
        writeln!(
            out,
            "    injections   {n}  ({pct:.1}% of {palace} turns)",
            n = row.injections,
            pct = row.share_of_turns * 100.0,
            palace = row.palace
        )?;
        writeln!(
            out,
            "    age          {age:.1} days     importance {imp:.2} -> {eff:.2} effective (90-day half-life)",
            age = row.age_days,
            imp = row.importance,
            eff = row.effective_importance
        )?;
        writeln!(
            out,
            "    expires_at   {}",
            if row.has_expiry {
                "set — already triaged"
            } else {
                "not set"
            }
        )?;
        let signals = if row.signals.is_empty() {
            "none".to_string()
        } else {
            row.signals
                .iter()
                .map(|s| s.label())
                .collect::<Vec<_>>()
                .join(", ")
        };
        writeln!(out, "    signals      {signals}")?;
        writeln!(out, "    excerpt      {}", wrap_excerpt(&row.excerpt))?;
        writeln!(out)?;
    }

    if census.rows.len() > limit {
        writeln!(
            out,
            "({shown} of {total} matching drawers shown — raise --limit for more)",
            shown = limit,
            total = census.rows.len()
        )?;
    }
    Ok(())
}

/// Write the coverage block — what was actually read.
///
/// Why: every number above is a measurement over a specific log window and a
/// specific set of palaces. Stating the window is what separates "this drawer is
/// never injected" from "the logs do not go back far enough to know".
fn render_coverage(out: &mut dyn Write, census: &Census, stats: &ScanStats) -> Result<()> {
    let window = match (stats.earliest, stats.latest) {
        (Some(a), Some(b)) => format!("{} to {}", a.format("%Y-%m-%d"), b.format("%Y-%m-%d")),
        _ => "no entries".to_string(),
    };
    writeln!(
        out,
        "Hook logs: {files} file(s), {inj} prompt-context injections across all palaces, {window}",
        files = stats.files_scanned,
        inj = stats.injections_counted,
    )?;
    if stats.files_failed > 0 {
        writeln!(
            out,
            "  {} log file(s) unreadable and skipped",
            stats.files_failed
        )?;
    }
    let ok = census.outcomes.iter().filter(|o| o.error.is_none()).count();
    writeln!(
        out,
        "Palaces:   {ok} read, {drawers} drawers",
        drawers = census.drawers_total
    )?;
    for o in census.outcomes.iter().filter(|o| o.error.is_some()) {
        writeln!(
            out,
            "  {} could not be read: {}",
            o.palace,
            o.error.as_deref().unwrap_or("unknown")
        )?;
    }
    Ok(())
}

/// Wrap the excerpt at [`EXCERPT_WIDTH`], indenting continuation lines.
///
/// What: greedy word wrap. The excerpt is already whitespace-collapsed to a
/// single line by `drawer_preview`, so splitting on spaces is sufficient.
fn wrap_excerpt(excerpt: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in excerpt.split(' ') {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > EXCERPT_WIDTH
        {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines.join(&format!("\n{EXCERPT_INDENT}"))
}

/// Render the report as one JSON object.
///
/// Why: the human path is the stanza list, but triage across 93 palaces wants a
/// machine-readable list to slice. Same data, no derived verdict here either.
/// Test: `json_round_trips`.
pub fn render_json(
    out: &mut dyn Write,
    census: &Census,
    stats: &ScanStats,
    limit: usize,
) -> Result<()> {
    let rows: Vec<_> = census
        .rows
        .iter()
        .take(limit)
        .map(|r| {
            json!({
                "palace": r.palace,
                "drawer_id": r.drawer_id.to_string(),
                "room": r.room,
                "excerpt": r.excerpt,
                "age_days": r.age_days,
                "injections": r.injections,
                "share_of_turns": r.share_of_turns,
                "importance": r.importance,
                "effective_importance": r.effective_importance,
                "has_expiry": r.has_expiry,
                "signals": r.signals.iter().map(|s| s.label()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let doc = json!({
        "read_only": true,
        "coverage": {
            "log_files_scanned": stats.files_scanned,
            "log_files_failed": stats.files_failed,
            "injections_counted": stats.injections_counted,
            "window_start": stats.earliest.map(|t| t.to_rfc3339()),
            "window_end": stats.latest.map(|t| t.to_rfc3339()),
            "drawers_read": census.drawers_total,
            "palaces_failed": census
                .outcomes
                .iter()
                .filter(|o| o.error.is_some())
                .map(|o| json!({ "palace": o.palace, "error": o.error }))
                .collect::<Vec<_>>(),
        },
        "matching_drawers": census.rows.len(),
        "candidates": rows,
    });
    writeln!(out, "{}", serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}
