//! #9278 T1a: the typed format refusal of `KgStoreRedb::open`, and the
//! `{:#}` chain of a boxed variant. The full mapping table is in T1b (N7).

use super::KgError;
use crate::engine::store::kg_redb::KgStoreRedb;
use crate::engine::store::palace_format::{PALACE_FORMAT_SUPPORTED, stamp_format};
use crate::engine::store::palace_store::PalaceStoreError;

#[test]
fn boxed_variant_keeps_the_display_chain() {
    let e = anyhow::Error::from(std::io::Error::other("gone")).context("open kg");
    let alt = format!("{e:#}");
    let kg = KgError::from_internal(e);
    assert!(matches!(kg, KgError::Storage(_)), "{kg:?}");
    assert_eq!(
        (format!("{kg}"), format!("{kg:#}")),
        ("open kg".into(), alt)
    );
}

#[test]
fn kg_store_open_format_refusal_is_recoverable_as_palace_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let kg = dir.path().join("kg.redb");
    drop(KgStoreRedb::open(&kg).expect("create"));
    let db = redb::Database::create(&kg).expect("reopen to stamp");
    let wtx = db.begin_write().expect("begin stamp");
    stamp_format(&wtx, PALACE_FORMAT_SUPPORTED + 1).expect("stamp");
    wtx.commit().expect("commit stamp");
    drop(db);
    let err = KgStoreRedb::open(&kg).err().expect("a newer KG is refused");
    assert!(
        matches!(
            err.as_palace_store(),
            Some(PalaceStoreError::FormatTooNew { .. })
        ),
        "{err:#}"
    );
}
