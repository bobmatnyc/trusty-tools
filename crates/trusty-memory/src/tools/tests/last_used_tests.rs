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
