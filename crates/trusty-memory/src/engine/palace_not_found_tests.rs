//! Fixtures and assertions for the caller-facing palace-not-found contract
//! (#9751), shared by the MCP tool, wire and service surface tests.
//!
//! Why: every surface asserts the same contract — the missing id and the capped
//! list of existing palace ids, never a filesystem path — so one copy of the
//! fixture and the assertion keeps the surfaces from drifting apart.
//! Test: this IS test support; the callers are the `palace_missing` tests.

use crate::engine::palace::{Palace, PalaceId};
use crate::engine::store::PalaceStore;
use std::path::Path;

/// Write `count` palaces named `listed-00`, `listed-01`, … under `root`.
///
/// Metadata only (`palace.json`), so a fixture of 25 palaces opens no redb
/// file; the not-found listing reads nothing else. Returns the ids in order.
pub(crate) fn seed_listed_palaces(root: &Path, count: usize) -> Vec<String> {
    (0..count)
        .map(|i| {
            let id = format!("listed-{i:02}");
            PalaceStore::save_palace(&Palace {
                id: PalaceId::new(&id),
                name: id.clone(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: root.join(&id),
            })
            .expect("save a listed palace");
            id
        })
        .collect()
}

/// Write a palace whose `palace.json` does not decode, so listing the palaces
/// under `root` fails (#5543 made the walk fail rather than skip it).
pub(crate) fn seed_unlistable_palace(root: &Path) {
    let dir = root.join("undecodable");
    std::fs::create_dir_all(&dir).expect("create the undecodable palace dir");
    std::fs::write(dir.join("palace.json"), b"{ this is not json").expect("write");
}

/// Assert `message` names no filesystem path under `root`.
pub(crate) fn assert_no_path(message: &str, root: &Path) {
    let root_text = root.to_string_lossy();
    assert!(
        !message.contains(root_text.as_ref()),
        "#9751: the caller-facing message carries the palace root {root_text}: {message}"
    );
    if let Ok(canonical) = root.canonicalize() {
        let canonical = canonical.to_string_lossy();
        assert!(
            !message.contains(canonical.as_ref()),
            "#9751: the caller-facing message carries the palace root {canonical}: {message}"
        );
    }
    assert!(
        !message.contains("palace.json"),
        "#9751: the caller-facing message names a metadata file: {message}"
    );
}

/// Assert `message` reports `missing` as not found and lists the palaces
/// [`seed_listed_palaces`] wrote: the first `min(total, 20)` ids, the count
/// shown and the total, and no path under `root`.
pub(crate) fn assert_listed_absence(message: &str, root: &Path, missing: &str, total: usize) {
    assert_no_path(message, root);
    assert!(message.contains(missing), "must name {missing}: {message}");
    let shown = total.min(20);
    assert!(
        message.contains(&format!("{shown} of {total}")),
        "must say how many of how many palaces it lists: {message}"
    );
    for i in 0..total {
        let id = format!("listed-{i:02}");
        assert_eq!(
            message.contains(&id),
            i < shown,
            "{id} listed? expected {}: {message}",
            i < shown
        );
    }
}

/// Assert `message` reports `missing` as not found, carries no path under
/// `root`, and says the existing palaces could not be listed.
pub(crate) fn assert_unlisted_absence(message: &str, root: &Path, missing: &str) {
    assert_no_path(message, root);
    assert!(message.contains(missing), "must name {missing}: {message}");
    assert!(
        message.contains("could not be listed"),
        "a failed listing must say so rather than list nothing: {message}"
    );
}
