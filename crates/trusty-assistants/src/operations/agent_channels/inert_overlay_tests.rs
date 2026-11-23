//! An overlay whose global is gone stays visible and survives a whole-list save
//! (#8187).
//!
//! Why: `load_at_with` drops such a record so the rest of the file still loads
//! (#7609). That made it invisible to `GET /api/agents/{name}/channels`, and
//! the view's revision hashes the RAW file — so the next whole-list save wrote
//! back exactly the list the client saw and deleted the hidden record, with no
//! conflict and nothing logged. `DELETE /api/channels/{id}` named it once, at
//! the moment it was orphaned, and after that the operator had no way to see
//! it at all.
//! What: this file is in its own module because `agent_channels.rs` is close to
//! the 500-SLOC production cap and an inline test module counts against it.
//! Test: this module IS the test.

use super::*;

/// An assistant directory with one resolvable binding and one orphaned overlay.
fn fixture() -> (tempfile::TempDir, Vec<PathBuf>) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("fixture.toml"), "[agent]\nname='fixture'\n").expect("manifest");
    std::fs::write(
        dir.path().join("fixture.channels.json"),
        json!([
            {"id":"team","name":"Team","provider":"slack","target":"C123456",
             "enabled":true,"send_enabled":true},
            {"id":"ghost","name":"Orphaned mail","provider":"gworkspace","target":"",
             "enabled":true,"receive_enabled":true}
        ])
        .to_string(),
    )
    .expect("bindings");
    let dirs = vec![dir.path().to_path_buf()];
    (dir, dirs)
}

/// The orphaned overlay is reported by the view and is NOT deleted by a save of
/// the list that view handed out.
///
/// Pre-change this fails at the first assertion: `inert_overlays_at` does not
/// exist, and once it does, the save still drops `ghost` from the file.
#[tokio::test]
async fn an_inert_overlay_is_reported_and_survives_a_whole_list_save() {
    let (dir, dirs) = fixture();
    let path = dir.path().join("fixture.channels.json");

    // No global declares `ghost`, so the loadable view cannot show it.
    let (_, raw, visible) = load_at_with(&dirs, "fixture", &[]).await.expect("load");
    assert_eq!(
        visible.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        vec!["team"],
        "the orphaned overlay is dropped from the loadable list"
    );
    let inert = inert_overlays_at(&dirs, "fixture", &[])
        .await
        .expect("inert overlays");
    assert_eq!(
        inert.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        vec!["ghost"],
        "and is reported instead of disappearing"
    );

    // The client saves back exactly what it could see.
    let (before, after) = write_at(
        &dirs,
        "fixture",
        Update {
            revision: revision(&raw),
            bindings: visible,
        },
    )
    .await
    .expect("whole-list save");
    assert_eq!(
        (before, after),
        (2, 2),
        "the counts audit the ON-DISK records, not the visible ones"
    );

    let saved: Vec<Binding> =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read back")).expect("parse");
    assert_eq!(
        saved.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        vec!["team", "ghost"],
        "the record the client never saw is still on disk"
    );

    // Declaring the global again makes it an ordinary, editable binding.
    let globals = vec![crate::channels::Channel {
        id: "ghost".into(),
        name: "Orphaned mail".into(),
        provider: "gmail".into(),
        scope: crate::channels::ChannelScope::Global,
        enabled: true,
        receive_enabled: true,
        ..crate::channels::Channel::default()
    }];
    let (_, _, visible) = load_at_with(&dirs, "fixture", &globals)
        .await
        .expect("reload");
    assert_eq!(visible.len(), 2, "the repair path works: {visible:?}");
    assert!(
        inert_overlays_at(&dirs, "fixture", &globals)
            .await
            .expect("inert overlays")
            .is_empty(),
        "and nothing is reported inert once the global is back"
    );
}

/// A client that reuses the inert overlay's id wins — its own record is written
/// and the hidden one is not duplicated back in.
///
/// Why: preservation must not be able to produce a file with two bindings of
/// the same id, which `write_at` refuses on the way in and would then create on
/// the way out.
///
/// Pre-change this fails to compile: `inert_overlays_at` does not exist.
#[tokio::test]
async fn a_client_record_reusing_the_inert_id_replaces_it() {
    let (dir, dirs) = fixture();
    let path = dir.path().join("fixture.channels.json");
    let (_, raw, _) = load_at_with(&dirs, "fixture", &[]).await.expect("load");
    let replacement: Binding = serde_json::from_value(json!({
        "id":"ghost","name":"Real mail","provider":"slack","target":"C999999",
        "enabled":true,"send_enabled":true
    }))
    .expect("binding");

    write_at(
        &dirs,
        "fixture",
        Update {
            revision: revision(&raw),
            bindings: vec![replacement],
        },
    )
    .await
    .expect("save");

    let saved: Vec<Binding> =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read back")).expect("parse");
    assert_eq!(
        saved.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        vec!["ghost"],
        "exactly one record carries the id, and it is the client's"
    );
    assert_eq!(saved[0].target, "C999999", "the client's record won");
}
