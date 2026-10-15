//! A format-0 palace, written by hand from the old on-disk shapes (#9274).
//!
//! Why: since #9918 a writer open migrates a format-0 palace 0 → 1 → 2,
//! and the old shapes
//! are private to this crate. A consumer test (trusty-mpm's SM palace) needs
//! the same fixture this crate's tests use, never a copy of a real palace.
//! What: [`write_format_0_palace_with`] writes `palace.json` without
//! `format_version`, an `identity.txt`, and a `kg.redb` with pre-#61 drawer
//! rows, the pre-#4810 triple key and no `kg_schema` table. Compiled for this
//! crate's tests and under the `memory-core-test-support` feature.
//! Test: `a_format_0_palace_migrates_to_2_at_first_writer_open`.

use super::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, TRIPLES, TripleValue, encode_u64, encode_value,
};
use anyhow::Context;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The triples every format-0 fixture carries: two active, one closed.
pub const FORMAT_0_TRIPLES: [(&str, &str, &str, Option<i64>); 3] = [
    ("room:General", "contains", "drawer:a", None),
    ("alice", "knows", "bob", None),
    ("alice", "knows", "carol", Some(1_700_000_100_000)),
];

/// The drawers every format-0 fixture carries.
pub const FORMAT_0_DRAWERS: [&str; 2] = ["first v0 drawer", "second v0 drawer"];

/// The pre-#61 drawer row shape (`LegacyDrawerRecord`), field for field.
#[derive(Serialize)]
struct V0Drawer {
    room_id: String,
    content: String,
    importance: f32,
    tags: Vec<String>,
    source_file: Option<String>,
    created_at_ms: i64,
}

/// The pre-#4810 triple key: `[subject_len][subject][predicate]`.
pub fn legacy_triple_key(subject: &str, predicate: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(subject.len() as u16).to_be_bytes());
    out.extend_from_slice(subject.as_bytes());
    out.extend_from_slice(predicate.as_bytes());
    out
}

/// Write a plain format-0 palace `id` into `dir`.
pub fn write_format_0_palace(dir: &Path, id: &str) -> anyhow::Result<()> {
    write_format_0_palace_with(dir, id, |_| {})
}

/// Write a format-0 palace `id` into `dir`, creating it.
///
/// What: `extra` runs inside the KG write transaction, so a test can add a
/// malformed row or a marker the old code wrote. Fails if `dir` already holds
/// a `kg.redb` another handle has open.
pub fn write_format_0_palace_with(
    dir: &Path,
    id: &str,
    extra: fn(&redb::WriteTransaction),
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let json = serde_json::json!({
        "id": id,
        "name": id,
        "description": null,
        "created_at": "2025-06-01T00:00:00Z",
        "data_dir": dir,
        "schema_version": 1,
    });
    std::fs::write(dir.join("palace.json"), serde_json::to_vec_pretty(&json)?)
        .context("write palace.json")?;
    std::fs::write(dir.join("identity.txt"), b"v0 identity").context("write identity.txt")?;

    let db = trusty_common::redb_cache::create_palace_db(&dir.join("kg.redb"))
        .context("create v0 kg.redb")?;
    let wtx = db.begin_write()?;
    {
        let mut drawers = wtx.open_table(DRAWERS)?;
        for content in FORMAT_0_DRAWERS {
            let row = V0Drawer {
                room_id: uuid::Uuid::new_v4().to_string(),
                content: content.into(),
                importance: 0.5,
                tags: vec!["v0".into()],
                source_file: None,
                created_at_ms: 1_700_000_000_000,
            };
            let bytes = encode_value(&row)?;
            drawers.insert(uuid::Uuid::new_v4().as_bytes().as_slice(), bytes.as_slice())?;
        }
        let mut triples = wtx.open_table(TRIPLES)?;
        let mut counts = wtx.open_table(ACTIVE_SUBJECT_COUNTS)?;
        let mut per_subject: BTreeMap<&str, u64> = BTreeMap::new();
        for (s, p, o, valid_to_ms) in FORMAT_0_TRIPLES {
            let value = TripleValue {
                object: o.to_string(),
                valid_from_ms: 1_700_000_000_000,
                valid_to_ms,
                confidence: 1.0,
                provenance: None,
            };
            let core = legacy_triple_key(s, p);
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
    extra(&wtx);
    wtx.commit().context("commit v0 kg.redb")?;
    Ok(())
}
