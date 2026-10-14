//! Tests for `EngineError` (#9278 T3b-1).

use super::EngineError;
use crate::engine::store::concurrent_open::is_incompatible_format_refusal;
use crate::engine::store::kg::{KgError, KnowledgeGraph};
use crate::engine::store::palace_format::is_format_refusal;
use crate::engine::store::palace_store::PalaceStoreError;

fn too_new() -> PalaceStoreError {
    PalaceStoreError::FormatTooNew {
        palace: "p".into(),
        found: 3,
        supported: 2,
    }
}

/// #9278 T3b-1: a bare `PalaceStoreError` is `Palace`; a wrapper root, an
/// incompatible-format refusal and a plain error are `Other`, where the
/// classifiers still see a refusal and `{e:#}` keeps the chain; an
/// `EngineError` already inside comes back as-is.
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
    assert!(matches!(e, EngineError::Other(_)), "{e:?}");
    assert!(is_format_refusal(&anyhow::Error::from(e)));

    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("bad.redb");
    std::fs::write(&bad, [0xABu8; 4096]).expect("write");
    let refused = KnowledgeGraph::open(&bad).err().expect("refused");
    let e = EngineError::from_internal(anyhow::Error::from(refused).context("open KG"));
    assert!(matches!(e, EngineError::Other(_)), "{e:?}");
    assert!(is_incompatible_format_refusal(
        &anyhow::Error::from(e).context("x")
    ));

    let e = EngineError::from_internal(anyhow::anyhow!("boom").context("ctx"));
    assert!(matches!(e, EngineError::Other(_)), "{e:?}");
    assert_eq!(format!("{e:#}"), "ctx: boom");
    let e = anyhow::Error::from(e);
    assert!(!is_format_refusal(&e) && !is_incompatible_format_refusal(&e));
}

/// #9278 T3b-1, Q7 not applied: a `PalaceStoreError` under a `.context()` is
/// `Other` and keeps the prefix; the classifiers still see the refusal.
#[test]
fn engine_error_of_a_contexted_palace_error_keeps_its_prefix() {
    let e = EngineError::from_internal(anyhow::Error::from(too_new()).context("open KG for p"));
    assert!(matches!(e, EngineError::Other(_)), "{e:?}");
    assert_eq!(format!("{e:#}"), format!("open KG for p: {}", too_new()));
    assert!(is_format_refusal(&anyhow::Error::from(e)));
}
