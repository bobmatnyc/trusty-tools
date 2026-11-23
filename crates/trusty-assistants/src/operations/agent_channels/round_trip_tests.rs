//! An existing `*.channels.json` survives #7609's new field untouched.
//!
//! Why: `Binding` gained `event_types` so a binding migrated out of
//! `agent.toml` has somewhere to put it. That field is only safe to add if a
//! channels file written BEFORE it existed reads back and writes out as the
//! same bytes — otherwise every assistant's file is rewritten by the first
//! unrelated save, and a review has no way to tell an intended change from
//! this one.

use super::*;

/// The live cto-assistant binding, in the exact bytes `write_at` produces:
/// `serde_json::to_vec_pretty` over `Binding`'s declared field order, with
/// `event_types` absent because #7609 omits it when empty.
const EXISTING_FILE: &str = r#"[
  {
    "id": "cto-assistant",
    "name": "CTO Assistant",
    "provider": "slack",
    "target": "D0AM8GWJLFR",
    "enabled": true,
    "send_enabled": true,
    "receive_enabled": true,
    "filter": {
      "include_labels": [],
      "subject_contains": [],
      "snippet_contains": [],
      "from": [],
      "exclude_labels": []
    },
    "instructions": "",
    "credential_ref": "slack"
  }
]"#;

#[tokio::test]
async fn agent_channels_round_trips_an_existing_file_byte_for_byte() {
    let dir = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("fixture.toml"), "[agent]\nname='fixture'")
        .await
        .expect("manifest");
    let dirs = [dir.path().to_path_buf()];
    let (path, _, _) = load_at(&dirs, "fixture").await.expect("resolve");
    tokio::fs::write(&path, EXISTING_FILE)
        .await
        .expect("seed the pre-#7609 file");

    let (_, raw, bindings) = load_at(&dirs, "fixture").await.expect("load");
    assert_eq!(bindings.len(), 1);
    assert!(
        bindings[0].event_types.is_empty(),
        "an absent key must read as no event types, not fail the load"
    );
    assert_eq!(bindings[0].credential_ref.as_deref(), Some("slack"));
    assert_eq!(bindings[0].target, "D0AM8GWJLFR");

    write_at(
        &dirs,
        "fixture",
        Update {
            revision: revision(&raw),
            bindings,
        },
    )
    .await
    .expect("write back unchanged");

    assert_eq!(
        tokio::fs::read_to_string(&path).await.expect("read back"),
        EXISTING_FILE,
        "a save that changed nothing must not change the bytes"
    );
}
