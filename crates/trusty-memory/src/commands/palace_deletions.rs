//! `trusty-memory palace deletions <name>` — the maintenance deletion trail (#8732).
//!
//! Why: in #8729 drawers left live palaces with no record of which pass removed
//! them, or which drawer each one duplicated. The dream and purge passes now
//! append a per-drawer record to the palace's journal; this command reads it.
//! What: READ-ONLY. Reads `<data_dir>/maintenance_deletions*.jsonl` and prints
//! the newest `limit` records, optionally only those naming one drawer as the
//! removed or the surviving side. It opens no store and takes no lock, so it is
//! safe with the daemon up.
//! Test: `palace_deletions_shows_the_survivor_and_score_for_a_removed_drawer`.

use anyhow::{Context, Result};
use trusty_common::memory_core::maintenance_log::{read_journal, MaintenanceDeletion};
use trusty_common::memory_core::palace::Palace;
use uuid::Uuid;

/// Render the deletion trail for `palace` as text or JSON.
///
/// Why: `resolve` reads the machine's real data root; taking the `Palace` as an
/// argument lets a test drive the report against a fixture.
/// What: filters by `drawer` (removed or surviving side), keeps the newest
/// `limit` records in chronological order, and renders them.
/// Test: `palace_deletions_shows_the_survivor_and_score_for_a_removed_drawer`.
pub(crate) fn deletions_report(
    name: &str,
    palace: &Palace,
    drawer: Option<Uuid>,
    limit: usize,
    json: bool,
) -> Result<String> {
    let journal = read_journal(&palace.data_dir)
        .with_context(|| format!("read deletion journal in {}", palace.data_dir.display()))?;
    let total = journal.records.len();
    let matching: Vec<&MaintenanceDeletion> = journal
        .records
        .iter()
        .filter(|r| drawer.is_none_or(|id| r.drawer_id == id || r.survivor_id == Some(id)))
        .collect();
    let shown = &matching[matching.len().saturating_sub(limit)..];
    if json {
        return Ok(format!("{}\n", serde_json::to_string_pretty(shown)?));
    }
    let mut out = format!(
        "palace={name} journal={} records={total} matching={} shown={}\n",
        palace.data_dir.display(),
        matching.len(),
        shown.len()
    );
    if journal.malformed > 0 {
        out.push_str(&format!(
            "  note: {} unparseable line(s) skipped\n",
            journal.malformed
        ));
    }
    if shown.is_empty() {
        out.push_str("  no maintenance deletions recorded\n");
    }
    for r in shown {
        out.push_str(&format!(
            "{} {} drawer={}",
            r.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            r.reason,
            r.drawer_id
        ));
        if let Some(survivor) = r.survivor_id {
            out.push_str(&format!(" survivor={survivor}"));
        }
        if let Some(score) = r.score {
            out.push_str(&format!(" score={score:.4}"));
        }
        out.push_str(&format!(" pid={}\n", r.pid));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::memory_core::maintenance_log::{record, DeletionReason};
    use trusty_common::memory_core::palace::PalaceId;

    /// Why: #8729's open question is "which surviving drawer did each removed
    /// one duplicate, at what score" — the report must answer it per drawer.
    #[test]
    fn palace_deletions_shows_the_survivor_and_score_for_a_removed_drawer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let palace = Palace {
            id: PalaceId::new("trail"),
            name: "trail".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().to_path_buf(),
        };
        let (removed, survivor, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let dedup = MaintenanceDeletion::new(&palace.id, removed, DeletionReason::DreamDedup)
            .with_survivor(survivor, Some(0.9731));
        let prune = MaintenanceDeletion::new(&palace.id, other, DeletionReason::DreamPrune);
        record(Some(&palace.data_dir), &dedup);
        record(Some(&palace.data_dir), &prune);

        let text = deletions_report("trail", &palace, Some(removed), 50, false).expect("report");
        assert!(text.contains("records=2 matching=1 shown=1"), "{text}");
        assert!(
            text.contains(&format!(
                "dream_dedup drawer={removed} survivor={survivor} score=0.9731"
            )),
            "{text}"
        );
        assert!(!text.contains(&other.to_string()), "{text}");

        let json = deletions_report("trail", &palace, None, 1, true).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.as_array().map(Vec::len), Some(1));
        assert_eq!(parsed[0]["reason"], "dream_prune", "newest record is kept");
    }
}
