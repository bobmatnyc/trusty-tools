//! Tests for `EngineError` (#9278 T3b-1, T3c-1).

use super::EngineError;
use crate::engine::retrieval::VerifyNotStored;
use crate::engine::store::concurrent_open::is_incompatible_format_refusal;
use crate::engine::store::hnsw_store::HnswStoreError;
use crate::engine::store::kg::{KgError, KnowledgeGraph};
use crate::engine::store::kg_redb::READ_ONLY_ERROR_MSG;
use crate::engine::store::palace_format::is_format_refusal;
use crate::engine::store::palace_store::PalaceStoreError;

fn too_new() -> PalaceStoreError {
    PalaceStoreError::FormatTooNew {
        palace: "p".into(),
        found: 3,
        supported: 2,
    }
}

/// The KG open's refusal of an incompatible-format file.
fn incompatible() -> KgError {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("bad.redb");
    std::fs::write(&bad, [0xABu8; 4096]).expect("write");
    KnowledgeGraph::open(&bad).err().expect("refused")
}

/// #9278 T3b-1, T3c-1: a bare `PalaceStoreError` is `Palace`; a bare
/// `KgError`, `HnswStoreError` or `VerifyNotStored` is its own variant; a
/// plain error is `Other`, where `{e:#}` keeps the chain; the classifiers
/// still see a refusal; an `EngineError` already inside comes back as-is.
#[test]
fn engine_error_from_internal_maps_each_variant_and_keeps_text() {
    let e = EngineError::from_internal(anyhow::Error::from(too_new()));
    assert!(
        matches!(
            e,
            EngineError::Palace(PalaceStoreError::FormatTooNew { found: 3, .. })
        ),
        "{e:?}"
    );
    assert_eq!(e.to_string(), too_new().to_string());
    let e = EngineError::from_internal(anyhow::Error::from(e));
    assert!(matches!(e, EngineError::Palace(_)), "{e:?}");
    assert!(is_format_refusal(&anyhow::Error::from(e).context("x")));

    let e = EngineError::from_internal(anyhow::Error::from(KgError::Palace(too_new())));
    assert!(matches!(e, EngineError::Kg(KgError::Palace(_))), "{e:?}");
    assert!(is_format_refusal(&anyhow::Error::from(e)));

    let e = EngineError::from_internal(anyhow::Error::from(incompatible()));
    assert!(matches!(e, EngineError::Kg(_)), "{e:?}");
    assert!(is_incompatible_format_refusal(
        &anyhow::Error::from(e).context("x")
    ));

    let e = EngineError::from_internal(anyhow::Error::from(HnswStoreError::ReadOnly));
    assert!(
        matches!(e, EngineError::Hnsw(HnswStoreError::ReadOnly)),
        "{e:?}"
    );
    assert_eq!(e.to_string(), READ_ONLY_ERROR_MSG);

    let missed = VerifyNotStored::new(uuid::Uuid::nil(), "kg write failed");
    let text = missed.to_string();
    let e = EngineError::from_internal(anyhow::Error::from(missed));
    assert!(
        matches!(&e, EngineError::VerifyNotStored(v) if v.drawer.is_nil()),
        "{e:?}"
    );
    assert_eq!(e.to_string(), text);

    let e = EngineError::from_internal(anyhow::anyhow!("boom").context("ctx"));
    assert!(matches!(e, EngineError::Other(_)), "{e:?}");
    assert_eq!(format!("{e:#}"), "ctx: boom");
    let e = anyhow::Error::from(e);
    assert!(!is_format_refusal(&e) && !is_incompatible_format_refusal(&e));
}

/// #9278 T3b-1, Q7 (provisional): a `PalaceStoreError` under a `.context()`
/// is `Palace` and prints only its own text.
#[test]
fn engine_error_of_a_contexted_palace_error_is_palace_without_its_prefix() {
    let e = EngineError::from_internal(anyhow::Error::from(too_new()).context("open KG for p"));
    assert!(matches!(e, EngineError::Palace(_)), "{e:?}");
    assert_eq!(format!("{e:#}"), too_new().to_string());
    assert!(is_format_refusal(&anyhow::Error::from(e)));
}

/// #9278 T3c-1, Q7b not applied: a `KgError`, `HnswStoreError` or
/// `VerifyNotStored` under a `.context()` is `Other` and keeps the prefix;
/// the classifiers still see a refusal inside.
#[test]
fn typed_roots_under_a_context_stay_other_with_their_prefix() {
    let ctx = |e: anyhow::Error| EngineError::from_internal(e.context("ctx"));
    let e = ctx(KgError::Palace(too_new()).into());
    assert!(matches!(e, EngineError::Other(_)), "{e:?}");
    assert_eq!(format!("{e:#}"), format!("ctx: {}", too_new()));
    assert!(is_format_refusal(&anyhow::Error::from(e)));
    let e = ctx(HnswStoreError::ReadOnly.into());
    assert!(matches!(e, EngineError::Other(_)), "{e:?}");
    assert_eq!(format!("{e:#}"), format!("ctx: {READ_ONLY_ERROR_MSG}"));
    let e = ctx(VerifyNotStored::new(uuid::Uuid::nil(), "r").into());
    assert!(matches!(e, EngineError::Other(_)), "{e:?}");
}

/// #9278 T3c-1: both classifiers find a refusal inside `EngineError::Kg` and
/// `EngineError::Hnsw`, as the variant's payload or inside the internal error
/// it boxes, under a `.context()`.
#[test]
fn classifiers_see_a_refusal_through_kg_and_hnsw_variants() {
    let wrap = |e: EngineError| anyhow::Error::from(e).context("forget");
    let e = wrap(EngineError::Kg(KgError::Palace(too_new())));
    assert!(is_format_refusal(&e), "Kg payload: {e:#}");
    let e = wrap(EngineError::Hnsw(HnswStoreError::Palace(too_new())));
    assert!(is_format_refusal(&e), "Hnsw payload: {e:#}");
    let e = wrap(EngineError::Kg(incompatible()));
    assert!(is_incompatible_format_refusal(&e), "Kg internal: {e:#}");
    let boxed = HnswStoreError::from_internal(incompatible().into());
    let e = wrap(EngineError::Hnsw(boxed));
    assert!(is_incompatible_format_refusal(&e), "Hnsw internal: {e:#}");
    let e = wrap(EngineError::VerifyNotStored(VerifyNotStored::new(
        uuid::Uuid::nil(),
        "r",
    )));
    assert!(!is_format_refusal(&e) && !is_incompatible_format_refusal(&e));
}
