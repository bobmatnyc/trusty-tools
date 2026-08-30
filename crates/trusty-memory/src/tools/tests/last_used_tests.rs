//! Dispatcher-level tests for the per-palace last-used stamp (#6424).
//!
//! Why: the throttle and the file format are unit-tested in
//! `crate::palace_last_used`. What those cannot show is that the stamp is
//! actually written when a caller uses a palace, is NOT written when something
//! merely inspects one, and is read back on the two surfaces the console reads
//! — `console_metrics` and `palace_info`. Those are dispatcher-level facts, so
//! they are pinned here, against the real handlers.
//! What: drives `dispatch_tool` against a tempdir-rooted `AppState`.
//! Test: this IS the test module.

use super::*;

/// Read the stamp file a palace should have, without going through the
/// dispatcher, so a broken read path cannot mask a broken write path.
fn stamp_on_disk(state: &AppState, palace: &str) -> Option<u64> {
    let id = trusty_common::memory_core::PalaceId::new(palace);
    let data_dir = state.registry.peek(&id)?.data_dir.clone()?;
    crate::palace_last_used::read(&data_dir)
}

/// A remember and a recall each stamp the palace they touched (#6424).
///
/// Why: this is the write half of the feature. Before it, the only recency
/// signal was `idle_evict`'s in-process counter, so the console had nothing
/// durable to show and no column to sort.
/// What: creates a palace, asserts it has no stamp, remembers into it, and
/// asserts a stamp now exists. Then clears the throttle cache — standing in for
/// a later day rather than a later second — recalls, and asserts the stamp is
/// still there.
/// Test: this function.
#[tokio::test]
async fn dispatch_remember_and_recall_stamp_last_used() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "stamped"}))
        .await
        .expect("palace_create");

    assert_eq!(
        stamp_on_disk(&state, "stamped"),
        None,
        "creating a palace is not using it"
    );

    dispatch_tool(
        &state,
        "memory_remember",
        json!({"palace": "stamped", "text": "The stamp file is written whenever a caller performs a real write against this palace", "room": "General"}),
    )
    .await
    .expect("memory_remember");

    let after_write = stamp_on_disk(&state, "stamped").expect("remember must stamp");

    // Drop the throttle's memory so the next use is eligible to write again.
    state.palace_last_used.clear();
    dispatch_tool(
        &state,
        "memory_recall",
        json!({"palace": "stamped", "query": "stamp", "top_k": 3}),
    )
    .await
    .expect("memory_recall");

    let after_read = stamp_on_disk(&state, "stamped").expect("recall must stamp");
    assert!(
        after_read >= after_write,
        "a recall is use too: {after_read} must not predate {after_write}"
    );
}

/// A recall addressed through an alias stamps the CANONICAL palace (#6424).
///
/// Why (round-2 review): `resolve_palace` hands back the caller's raw `palace`
/// argument, but `PalaceRegistry::open_palace_bounded` registers the handle
/// under the alias TARGET, and `registry.peek` resolves no aliases. Keying the
/// stamp on the raw string therefore found nothing and silently wrote nothing,
/// on every alias-addressed call — the operation succeeded and the column never
/// moved. This test failed before the alias resolution was added to
/// `stamp_palace_use`.
/// What: registers `sc` as an alias of `stamped-canonical`, recalls through the
/// alias, and asserts the stamp landed on the canonical palace's sidecar. The
/// alias has no directory of its own, so the canonical sidecar is the only
/// place it can correctly land.
/// Test: this function.
#[tokio::test]
async fn an_alias_addressed_recall_stamps_the_canonical_palace() {
    let (state, _tmp) = test_state();
    dispatch_tool(
        &state,
        "palace_create",
        json!({"name": "stamped-canonical"}),
    )
    .await
    .expect("palace_create");
    let canonical = dispatch_tool(
        &state,
        "palace_info",
        json!({"palace": "stamped-canonical"}),
    )
    .await
    .expect("palace_info")["id"]
        .as_str()
        .expect("id")
        .to_string();

    trusty_common::palace_alias::PalaceAliasStore::register_alias(
        &state.data_root,
        "sc",
        &canonical,
    )
    .expect("register_alias");

    dispatch_tool(
        &state,
        "memory_recall",
        json!({"palace": "sc", "query": "anything", "top_k": 3}),
    )
    .await
    .expect("memory_recall through the alias");

    assert!(
        stamp_on_disk(&state, &canonical).is_some(),
        "an alias-addressed recall must stamp the canonical palace it actually opened"
    );
}

/// Inspecting a palace does not count as using it (#6424).
///
/// Why: `palace_info` and `console_metrics` run on every console poll, and the
/// embed-audit sweeps open every palace on disk. If any of those stamped, every
/// palace would report the same freshness forever and the column would say
/// nothing.
/// What: creates a palace, calls `palace_info`, and asserts no stamp appeared.
/// Test: this function.
#[tokio::test]
async fn dispatch_palace_info_does_not_stamp_last_used() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "looked-at"}))
        .await
        .expect("palace_create");

    let info = dispatch_tool(&state, "palace_info", json!({"palace": "looked-at"}))
        .await
        .expect("palace_info");

    assert!(
        info["last_used_unix"].is_null(),
        "a never-used palace reports null, not a date: {info}"
    );
    assert_eq!(
        stamp_on_disk(&state, "looked-at"),
        None,
        "palace_info must not stamp the palace it reports on"
    );
}

/// `palace_info` and `console_metrics` both report a written stamp (#6424).
///
/// Why: the console reads `console_metrics`; `palace_info` is where anyone
/// asking about one palace looks. Both have to agree, and both have to read the
/// same file the write path produced.
/// What: remembers into a palace, then asserts the stamp appears on both
/// surfaces and matches what is on disk.
/// Test: this function.
#[tokio::test]
async fn last_used_is_reported_by_palace_info_and_console_metrics() {
    let (state, _tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "reported"}))
        .await
        .expect("palace_create");
    dispatch_tool(
        &state,
        "memory_remember",
        json!({"palace": "reported", "text": "A fact long enough to be worth storing, so the write path actually runs and stamps", "room": "General"}),
    )
    .await
    .expect("memory_remember");

    let on_disk = stamp_on_disk(&state, "reported").expect("stamped");

    let info = dispatch_tool(&state, "palace_info", json!({"palace": "reported"}))
        .await
        .expect("palace_info");
    assert_eq!(info["last_used_unix"].as_u64(), Some(on_disk));
    let palace_id = info["id"].as_str().expect("palace_info reports an id");

    let report = dispatch_tool(&state, "console_metrics", json!({}))
        .await
        .expect("console_metrics");
    let entry = report["metrics"]["palaces"]
        .as_array()
        .expect("palaces array")
        .iter()
        .find(|p| p["id"].as_str() == Some(palace_id))
        .expect("the palace must appear in the report");
    assert_eq!(
        entry["last_used_unix"].as_u64(),
        Some(on_disk),
        "console_metrics must report the same stamp palace_info does: {entry}"
    );
}
