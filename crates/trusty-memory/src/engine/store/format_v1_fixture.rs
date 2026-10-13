//! A format-1 palace, written by hand from the 0.33.x on-disk shapes (#9275).
//!
//! Why: the 1 → 2 migration, and every `PalaceRegistry` consumer that owns a
//! palace root (trusty-mpm's SM and portfolio palaces), need a format-1 palace
//! to migrate, never a copy of a real palace. The checked-in blob under
//! `testdata/palace-format/v1/` is the canary that this builder matches what
//! the shipped binary wrote (ADR-0067 D4).
//! What: [`write_format_1_palace_with`] writes `palace.json` with
//! `format_version: 1`, an `identity.txt`, and a `kg.redb` with current-shape
//! drawer rows, triples under the #4810 key, active-subject counts and both
//! `kg_schema` markers at 1. It writes no `index.usearch.redb`: the vector
//! index is derived, and a format-1 palace never holds a `{uuid}#{n}` row.
//! Compiled for this crate's tests and under `memory-core-test-support`.
//! Test: `a_format_1_palace_migrates_to_2_at_first_writer_open`.

use super::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, DrawerRecord, TRIPLES, TripleValue, encode_triple_key,
    encode_u64, encode_value,
};
use super::palace_format::stamp_format;
use anyhow::Context;
use std::collections::BTreeMap;
use std::path::Path;

/// The triples every format-1 fixture carries: two active, one closed.
pub const FORMAT_1_TRIPLES: [(&str, &str, &str, Option<i64>); 3] = [
    ("room:General", "contains", "drawer:a", None),
    ("alice", "knows", "bob", None),
    ("alice", "knows", "carol", Some(1_700_000_100_000)),
];

/// The drawers every format-1 fixture carries.
pub const FORMAT_1_DRAWERS: [&str; 2] = ["first v1 drawer", "second v1 drawer"];

/// Write a plain format-1 palace `id` into `dir`.
pub fn write_format_1_palace(dir: &Path, id: &str) -> anyhow::Result<()> {
    write_format_1_palace_with(dir, id, |_| {})
}

/// Write a format-1 palace `id` into `dir`, creating it.
///
/// What: `extra` runs inside the KG write transaction after both markers are
/// stamped, so a test can overwrite a marker or add a row. Fails if `dir`
/// already holds a `kg.redb` another handle has open.
pub fn write_format_1_palace_with(
    dir: &Path,
    id: &str,
    extra: fn(&redb::WriteTransaction),
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let json = serde_json::json!({
        "id": id,
        "name": id,
        "description": null,
        "created_at": "2026-10-01T00:00:00Z",
        "data_dir": dir,
        "schema_version": 1,
        "format_version": 1,
    });
    std::fs::write(dir.join("palace.json"), serde_json::to_vec_pretty(&json)?)
        .context("write palace.json")?;
    std::fs::write(dir.join("identity.txt"), b"v1 identity").context("write identity.txt")?;

    let db =
        trusty_common::redb_cache::create_palace_db(&dir.join("kg.redb")).context("create v1 kg.redb")?;
    let wtx = db.begin_write()?;
    {
        let mut drawers = wtx.open_table(DRAWERS)?;
        for content in FORMAT_1_DRAWERS {
            let row = DrawerRecord {
                room_id: uuid::Uuid::new_v4().to_string(),
                content: content.into(),
                importance: 0.5,
                tags: vec!["v1".into()],
                source_file: None,
                created_at_ms: 1_759_000_000_000,
                drawer_type: None,
                expires_at_ms: None,
                completed_at_ms: None,
                fact_key: None,
            };
            let bytes = encode_value(&row)?;
            drawers.insert(uuid::Uuid::new_v4().as_bytes().as_slice(), bytes.as_slice())?;
        }
        let mut triples = wtx.open_table(TRIPLES)?;
        let mut counts = wtx.open_table(ACTIVE_SUBJECT_COUNTS)?;
        let mut per_subject: BTreeMap<&str, u64> = BTreeMap::new();
        for (s, p, o, valid_to_ms) in FORMAT_1_TRIPLES {
            let value = TripleValue {
                object: o.to_string(),
                valid_from_ms: 1_700_000_000_000,
                valid_to_ms,
                confidence: 1.0,
                provenance: None,
            };
            let core = encode_triple_key(s, p, o);
            let key = if valid_to_ms.is_some() {
                let mut k = b"hist:".to_vec();
                k.extend_from_slice(&core);
                k.extend_from_slice(&value.valid_from_ms.to_be_bytes());
                k
            } else {
                *per_subject.entry(s).or_insert(0) += 1;
                core
            };
            let bytes = encode_value(&value)?;
            triples.insert(key.as_slice(), bytes.as_slice())?;
        }
        for (s, n) in per_subject {
            counts.insert(s.as_bytes(), encode_u64(n).as_slice())?;
        }
    }
    stamp_format(&wtx, 1)?;
    extra(&wtx);
    wtx.commit().context("commit v1 kg.redb")?;
    Ok(())
}
