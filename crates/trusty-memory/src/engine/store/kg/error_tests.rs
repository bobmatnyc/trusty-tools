//! #9278 T1a/T1b-1/T1b-2: `KgError::from_internal` mapping, Display parity,
//! and the typed format refusals of `KgStoreRedb::open` and `KnowledgeGraph::open`.

use super::{AdjacencyDesync, KgError, KnowledgeGraph};
use crate::engine::store::concurrent_open::is_incompatible_format_refusal;
use crate::engine::store::kg_redb::{KgStoreRedb, READ_ONLY_ERROR_MSG};
use crate::engine::store::palace_format::{
    PALACE_FORMAT_SUPPORTED, is_format_refusal, stamp_format,
};
use crate::engine::store::palace_store::PalaceStoreError;
use crate::engine::store::write_deadline::WriteTxnError;
use std::time::Duration;

fn deadline() -> WriteTxnError {
    WriteTxnError::DeadlineExceeded {
        palace: "p".into(),
        store: "kg.redb",
        stage: "commit",
        budget: Duration::from_secs(1),
        held: Duration::from_secs(2),
    }
}

fn variant(e: &KgError) -> &'static str {
    match e {
        KgError::ReadOnly => "ReadOnly",
        KgError::WriteDeadline(_) => "WriteDeadline",
        KgError::AdjacencyDesync(_) => "AdjacencyDesync",
        KgError::Palace(_) => "Palace",
        KgError::Storage(_) => "Storage",
        KgError::Codec(_) => "Codec",
        KgError::Other(_) => "Other",
    }
}

#[test]
fn from_internal_maps_each_variant_and_keeps_display() {
    let desync = AdjacencyDesync::CommittedButStale {
        operation: "assert",
        committed: 1,
        cause: "poisoned",
    };
    let json = serde_json::from_str::<u8>("x").expect_err("bad json");
    let too_new = || PalaceStoreError::FormatTooNew {
        palace: "p".into(),
        found: 2,
        supported: 1,
    };
    let cases = [
        (anyhow::Error::new(too_new()).context("open"), "Palace"),
        (
            anyhow::Error::new(deadline()).context("commit"),
            "WriteDeadline",
        ),
        (anyhow::Error::new(desync.clone()), "AdjacencyDesync"),
        (
            anyhow::Error::new(KgError::ReadOnly).context("outer"),
            "ReadOnly",
        ),
        (
            anyhow::Error::from(std::io::Error::other("gone")).context("open kg"),
            "Storage",
        ),
        (anyhow::Error::from(json).context("decode"), "Codec"),
        (anyhow::anyhow!("boom").context("ctx"), "Other"),
    ];
    for (e, want) in cases {
        let (plain, alt) = (format!("{e}"), format!("{e:#}"));
        let kg = KgError::from_internal(e);
        assert_eq!(variant(&kg), want, "{alt}");
        if matches!(want, "Storage" | "Codec" | "Other") {
            assert_eq!((format!("{kg}"), format!("{kg:#}")), (plain, alt.clone()));
            assert_eq!(format!("{:#}", anyhow::Error::from(kg)), alt);
        }
    }
    let typed = KgError::from_internal(anyhow::Error::new(deadline()).context("c"));
    assert_eq!(typed.as_write_deadline(), Some(&deadline()));
    let typed = KgError::from_internal(anyhow::Error::new(desync.clone()));
    assert_eq!(typed.as_adjacency_desync(), Some(&desync));
    let typed = KgError::from_internal(anyhow::Error::new(too_new()).context("open"));
    assert!(
        matches!(
            typed.as_palace_store(),
            Some(PalaceStoreError::FormatTooNew { .. })
        ),
        "{typed:?}"
    );
    assert_eq!(KgError::ReadOnly.to_string(), READ_ONLY_ERROR_MSG);
    let poisoned = KgError::poisoned(()); // #9278 Q6: text unchanged
    assert_eq!(variant(&poisoned), "Other");
    assert_eq!(poisoned.to_string(), "kg adjacency lock poisoned");
    assert!(size_of::<KgError>() <= 128, "{}", size_of::<KgError>());
    fn send_sync<T: Send + Sync + 'static>() {}
    send_sync::<KgError>();
}

/// A KG file at `kg` stamped at palace format `format`.
fn stamped_kg(kg: &std::path::Path, format: u32) {
    drop(KgStoreRedb::open(kg).expect("create"));
    let db = redb::Database::create(kg).expect("reopen to stamp");
    let wtx = db.begin_write().expect("begin stamp");
    stamp_format(&wtx, format).expect("stamp");
    wtx.commit().expect("commit stamp");
}

#[test]
fn kg_store_open_format_refusal_is_recoverable_as_palace_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let kg = dir.path().join("kg.redb");
    stamped_kg(&kg, PALACE_FORMAT_SUPPORTED + 1);
    let err = KgStoreRedb::open(&kg).err().expect("a newer KG is refused");
    assert!(
        matches!(
            err.as_palace_store(),
            Some(PalaceStoreError::FormatTooNew { .. })
        ),
        "{err:#}"
    );
}

/// #9278 T1b-2: every refusal of a typed `KnowledgeGraph::open` (an
/// incompatible redb file; a newer, unmigrated or too-old palace format) is
/// still recognised by the open-retry classifiers after a caller's `?`.
#[test]
fn knowledge_graph_open_refusals_stay_classified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("bad.redb");
    std::fs::write(&bad, [0xABu8; 4096]).expect("write");
    let err = anyhow::Error::from(KnowledgeGraph::open(&bad).err().expect("refused"));
    assert!(is_incompatible_format_refusal(&err), "{err:#}");
    for format in [PALACE_FORMAT_SUPPORTED + 1, PALACE_FORMAT_SUPPORTED - 1, 0] {
        let kg = dir.path().join(format!("kg{format}.redb"));
        stamped_kg(&kg, format);
        let err = anyhow::Error::from(KnowledgeGraph::open(&kg).err().expect("refused"));
        assert!(is_format_refusal(&err), "format {format}: {err:#}");
    }
}
