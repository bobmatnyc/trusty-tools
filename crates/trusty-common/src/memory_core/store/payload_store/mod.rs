//! redb-backed payload sidecar for external integrations.
//!
//! Why: `TrustyBackedMemoryStore` in open-mpm maps caller-supplied string ids
//! onto trusty's `Uuid` keyspace and attaches an arbitrary JSON payload to each
//! entry. The vector data already persists to the usearch index on disk, but
//! the string-id ↔ uuid ↔ JSON mapping was process-local — losing it on
//! restart blocked switching `TrustyBackedMemoryStore` to the production
//! default (issue #52). This module provides the missing durable sidecar so
//! payloads survive a process restart without forcing every embedding adapter
//! to roll its own storage layer.
//!
//! Issue #46 migrates this store from rusqlite to redb so the payload sidecar
//! drops the heavy native dependency chain (rusqlite + r2d2 + r2d2_sqlite) and
//! lines up with the rest of the Memory Palace (`kg_redb.rs`, palace_store).
//! The public `PayloadStore` API is unchanged so `TrustyBackedMemoryStore`
//! continues to work as a drop-in.
//!
//! What: `PayloadStore` opens a single redb database at a caller-supplied path
//! and exposes `upsert` / `get` / `delete` / `exists` / `list_segment` /
//! `lookup_id_for_uuid` / `load_all` over the `PAYLOADS` table defined in
//! `kg_store.rs`. The composite key is `[segment_len][segment][id]` (see
//! `encode_payload_key`); the value is a postcard-encoded `PayloadRecord`
//! that bundles the 16-byte uuid with the JSON payload string.
//!
//! Rows are partitioned by `segment` so a single store can back multiple
//! namespaces (open-mpm's `Segment::AgentMemory`, `CodeIndex`, etc.). Errors
//! flow through the typed `PayloadStoreError` so callers can distinguish I/O
//! from JSON from schema problems.
//!
//! Test: This module's `tests` exercise the full CRUD path plus a reopen
//! round-trip (the load-all method must return every row written by a prior
//! process). The one-shot migration from the legacy `payloads.db` SQLite
//! sidecar was removed in issue #989 (all palaces confirmed migrated).

mod store;
mod types;

pub use store::PayloadStore;
pub use types::{PayloadRow, PayloadStoreError};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn fixture_uuid(b: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[0] = b;
        Uuid::from_bytes(bytes)
    }

    #[test]
    fn roundtrip_persists_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("payloads.db");
        let u = fixture_uuid(1);

        {
            let store = PayloadStore::open(&path).unwrap();
            store
                .upsert("seg-a", "rec-1", u, &json!({"hello": "world"}))
                .unwrap();
        }

        // Reopen — payload must survive.
        let store2 = PayloadStore::open(&path).unwrap();
        let got = store2.get("seg-a", "rec-1").unwrap();
        assert_eq!(got, Some((u, json!({"hello": "world"}))));

        let rows = store2.load_all(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "rec-1");
        assert_eq!(rows[0].uuid, u);
        assert_eq!(rows[0].segment, "seg-a");
    }

    #[test]
    fn get_missing_returns_none() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        assert!(store.get("seg-a", "nope").unwrap().is_none());
    }

    #[test]
    fn delete_drops_row() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        let u = fixture_uuid(2);
        store.upsert("seg-a", "k", u, &json!(42)).unwrap();
        store.delete("seg-a", "k").unwrap();
        assert!(store.get("seg-a", "k").unwrap().is_none());
        // Idempotent — second delete is fine.
        store.delete("seg-a", "k").unwrap();
    }

    #[test]
    fn exists_reports_membership() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        assert!(!store.exists("seg-a", "k").unwrap());
        store
            .upsert("seg-a", "k", fixture_uuid(5), &json!("v"))
            .unwrap();
        assert!(store.exists("seg-a", "k").unwrap());
        assert!(!store.exists("seg-b", "k").unwrap());
        store.delete("seg-a", "k").unwrap();
        assert!(!store.exists("seg-a", "k").unwrap());
    }

    #[test]
    fn lookup_id_for_uuid_round_trips() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        let u = fixture_uuid(7);
        store.upsert("seg-a", "rec-7", u, &json!({"x": 1})).unwrap();
        let got = store.lookup_id_for_uuid("seg-a", u).unwrap();
        assert_eq!(got, Some("rec-7".to_string()));
        // Wrong segment must miss.
        assert!(store.lookup_id_for_uuid("seg-b", u).unwrap().is_none());
    }

    #[test]
    fn load_all_filters_by_segment() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        store.upsert("a", "1", fixture_uuid(1), &json!(1)).unwrap();
        store.upsert("a", "2", fixture_uuid(2), &json!(2)).unwrap();
        store.upsert("b", "3", fixture_uuid(3), &json!(3)).unwrap();

        let only_a = store.load_all(Some("a")).unwrap();
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|r| r.segment == "a"));

        let all = store.load_all(None).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn list_segment_returns_rows() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        store
            .upsert("seg-a", "x", fixture_uuid(1), &json!({"k": "v"}))
            .unwrap();
        store
            .upsert("seg-a", "y", fixture_uuid(2), &json!({"k": "w"}))
            .unwrap();
        store
            .upsert("seg-b", "z", fixture_uuid(3), &json!({"k": "u"}))
            .unwrap();
        let mut rows = store.list_segment("seg-a").unwrap();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "x");
        assert_eq!(rows[0].1, fixture_uuid(1));
        assert!(rows[0].2.contains("\"v\""));
        assert_eq!(rows[1].0, "y");
        assert_eq!(rows[1].1, fixture_uuid(2));

        let other = store.list_segment("seg-b").unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].0, "z");

        let empty = store.list_segment("seg-c").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn callers_passing_payloads_db_get_redb_sibling() {
        // Existing callers (`TrustyBackedMemoryStore`) pass `payloads.db`. Make
        // sure the resolver redirects them to `payloads.redb` so the on-disk
        // store actually uses redb regardless of caller hygiene.
        let dir = tempdir().unwrap();
        let legacy_path = dir.path().join("payloads.db");
        let store = PayloadStore::open(&legacy_path).unwrap();
        store
            .upsert("s", "i", fixture_uuid(9), &json!({"ok": true}))
            .unwrap();
        drop(store);
        let redb_path = dir.path().join("payloads.redb");
        assert!(
            redb_path.exists(),
            "expected redb sibling to be created at {}",
            redb_path.display()
        );
    }
}
