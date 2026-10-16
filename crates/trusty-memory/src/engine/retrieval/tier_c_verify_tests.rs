//! Library contract of a slot's `verify_with` command (#9390).
//!
//! Why: `remember_with_options` is the one enforcement point every surface
//! reaches, so the refusals must hold there, not only in the MCP handler.
//! What: drives `PalaceHandle::remember_with_options` against a real redb
//! store and reads the commands back through `KnowledgeGraph::verify_with_many`.
//! Test: this file IS the tests.

use super::*;
use crate::engine::palace::{PalaceId, RoomType};
use crate::engine::store::{kg::KnowledgeGraph, vector::UsearchStore};
use tempfile::tempdir;
use uuid::Uuid;

const SLOT: &str = "pr:9390/state";
const CMD: &str = "gh pr view 9390 --json headRefOid";

fn make_handle(dir: &std::path::Path) -> PalaceHandle {
    seed_shared_embedder_with_mock();
    let vs = UsearchStore::new(dir.join("idx.usearch"), 384).unwrap();
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();
    PalaceHandle::new(PalaceId::new("verify"), "Verify palace".to_string(), vs, kg)
}

async fn write(
    handle: &PalaceHandle,
    content: &str,
    fact_key: Option<&str>,
    verify: &str,
) -> Result<Uuid, crate::engine::EngineError> {
    handle
        .remember_with_options(
            content.to_string(),
            RoomType::General,
            vec![],
            0.5,
            RememberOptions {
                fact_key: fact_key.map(str::to_string),
                verify_with: Some(verify.to_string()),
                ..RememberOptions::forced()
            },
        )
        .await
}

/// A command with no slot, a blank command, and a secret-shaped command each
/// fail the write, and no drawer reaches redb.
#[tokio::test]
async fn a_verify_command_without_a_fact_key_is_refused_and_writes_nothing() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let content = "PR 9390 head is 5a6a8e3613 on the stacked branch";

    for (fact_key, verify, needle) in [
        (None, CMD, "fact_key"),
        (Some(SLOT), " \t\n", "empty"),
        (
            Some(SLOT),
            "gh api -H token sk-abcdefghijklmnopqrstuvwxyz01234567890123", // pragma: allowlist secret
            "verify command refused",
        ),
    ] {
        let err = write(&handle, content, fact_key, verify)
            .await
            .expect_err("the write must be refused");
        assert!(format!("{err:#}").contains(needle), "{verify:?}: {err:#}");
    }
    assert!(handle.kg.load_drawers().unwrap().is_empty());
}

/// A slot the admission gate refuses is written as an ordinary drawer, and its
/// command is dropped with the slot; an admitted slot keeps its trimmed command.
#[tokio::test]
async fn a_refused_slot_stores_no_verify_command() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());

    let refused = write(
        &handle,
        "a fact under a key with no domain",
        Some("bad"),
        CMD,
    )
    .await
    .expect("a refused slot still stores the drawer");
    let admitted = write(
        &handle,
        "PR 9390 is open on the stacked branch",
        Some(SLOT),
        &format!("  {CMD}\n"),
    )
    .await
    .expect("an admitted slot stores its command");

    let held = handle
        .kg
        .verify_with_many(vec![refused, admitted])
        .await
        .unwrap();
    assert_eq!(held.get(&refused), None, "{held:?}");
    assert_eq!(
        held.get(&admitted).map(String::as_str),
        Some(CMD),
        "{held:?}"
    );
}

/// Each write to the slot retires the prior occupant's command and records only
/// its own, so the batch read reports at most the current occupant's command.
/// A read keyed by the slot instead of the drawer id would report a retired
/// command on the newer drawer.
#[tokio::test]
async fn slot_retirement_closes_the_retirees_verify_command() {
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    let other = "gh pr view 9390 --json state";

    let first = write(&handle, "PR 9390 is open at head a38e511d", Some(SLOT), CMD)
        .await
        .unwrap();
    let second = write(
        &handle,
        "PR 9390 was rebased to head 5a6a8e36",
        Some(SLOT),
        other,
    )
    .await
    .unwrap();
    let held = handle
        .kg
        .verify_with_many(vec![first, second])
        .await
        .unwrap();
    assert_eq!(held.get(&first), None, "{held:?}");
    assert_eq!(
        held.get(&second).map(String::as_str),
        Some(other),
        "{held:?}"
    );

    let third = handle
        .remember_with_options(
            "PR 9390 merged to main as a squash commit".to_string(),
            RoomType::General,
            vec![],
            0.5,
            RememberOptions {
                fact_key: Some(SLOT.to_string()),
                ..RememberOptions::forced()
            },
        )
        .await
        .unwrap();
    let held = handle
        .kg
        .verify_with_many(vec![first, second, third])
        .await
        .unwrap();
    assert!(held.is_empty(), "{held:?}");
}

/// #9278 T3c-1: `remember`, `remember_with_options`, `forget` and `flush`
/// return `EngineError`, and a slot whose verify record failed after its
/// drawer committed is `EngineError::VerifyNotStored`, naming that drawer.
#[tokio::test]
async fn the_write_paths_return_engine_error_with_a_typed_verify_miss() {
    use crate::engine::EngineError;
    let dir = tempdir().unwrap();
    let handle = make_handle(dir.path());
    handle.fail_next_verify_record_for_test();
    let err = write(&handle, "PR 9390 head is 5a6a8e3613", Some(SLOT), CMD)
        .await
        .expect_err("a missed verify record is reported");
    let EngineError::VerifyNotStored(missed) = err else {
        panic!("expected VerifyNotStored, got {err:?}");
    };
    assert!(
        handle
            .kg
            .load_drawer_ids()
            .unwrap()
            .contains(&missed.drawer)
    );

    let id: Result<Uuid, EngineError> = handle
        .remember(
            "the typed remember path stores a plain drawer".to_string(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await;
    let gone: Result<ForgetOutcome, EngineError> = handle.forget(id.unwrap()).await;
    assert_eq!(gone.unwrap(), ForgetOutcome::Deleted);
    let flushed: Result<(), EngineError> = handle.flush();
    flushed.unwrap();
}
