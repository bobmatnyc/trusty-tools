//! Which directories under the assistants root are instance homes (#11742).
//!
//! Why: `~/trusty-agents` also holds entries that are not instances (an
//! `OKG` tree, a knowledge-base directory, lock files, a README). A user
//! needs the real instance names to run `ta ask`, and a typo'd name must be
//! answered with them.
//! What: [`list_instances`] returns the sorted ids of the root's children
//! that are directories, carry a valid [`AssistantInstanceId`] name and hold
//! an [`INSTRUCTIONS_FILE`] — the file `ta ask` reads first, so a directory
//! without one cannot answer.
//! Test: `list_names_only_instance_homes` (`tests/instance_list.rs`).

use std::io::ErrorKind;
use std::path::Path;

use super::home::INSTRUCTIONS_FILE;
use super::instance::AssistantInstanceId;

/// The instance homes under `root`, sorted by id.
///
/// Why: see the module doc.
/// What: a missing `root` is an empty list, not an error; any other read
/// failure of `root` is returned. Entries that are not directories, whose
/// names are not valid instance ids, or that hold no regular
/// [`INSTRUCTIONS_FILE`] are skipped.
/// Test: `list_names_only_instance_homes`, `list_on_an_empty_root_says_so`
/// (`tests/instance_list.rs`).
pub fn list_instances(root: &Path) -> std::io::Result<Vec<AssistantInstanceId>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path.is_dir() || !path.join(INSTRUCTIONS_FILE).is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // `new` trims; a name that changes under the trim is not this directory.
        match AssistantInstanceId::new(name) {
            Ok(id) if id.as_str() == name => found.push(id),
            _ => {}
        }
    }
    found.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(found)
}
