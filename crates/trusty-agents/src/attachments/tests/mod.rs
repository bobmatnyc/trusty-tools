//! Tests for the chat-attachment store (#7370).

mod isolation_tests;
mod manifest_tests;
mod model_input_tests;
mod store_tests;

use super::AttachmentStore;

/// A store over a fresh temp directory, standing in for `<home>/attachments`.
fn fixture() -> (tempfile::TempDir, AttachmentStore) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("attachments");
    (temp, AttachmentStore::new(root))
}
