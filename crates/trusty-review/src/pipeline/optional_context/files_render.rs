//! The changed-files section and its ledger row (#9195).
//!
//! STUB: types only; the rendering lands in the next commit.

use crate::models::{ContextSourceRecord, SourceState};

use super::files_select::{NotShown, Shown};

/// The ledger row's `source`.
pub(crate) const CHANGED_FILES: &str = "changed_files";

/// The section heading.
pub(crate) const HEADING: &str = "## Changed files (full text at the PR head)";

/// The rendered changed-files section, split so a map-reduce chunk can carry
/// only its own file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileSections {
    /// The heading and the note.
    head: String,
    /// The `Not shown:` list; empty when every file is shown.
    list: String,
    /// `(path, block)` per shown file, in diff order.
    blocks: Vec<(String, String)>,
}

impl FileSections {
    /// The section the unified prompt carries; empty when there is nothing.
    pub(crate) fn unified(&self) -> String {
        String::new()
    }

    /// The section one map-reduce chunk prompt for `file` carries.
    pub(crate) fn for_unit(&self, _file: &str, _first: bool) -> String {
        String::new()
    }
}

/// Render the section for `shown` and `not_shown` at `sha`.
pub(crate) fn render(_sha: &str, _shown: &[Shown], _not_shown: &[NotShown]) -> FileSections {
    FileSections::default()
}

/// The `changed_files` ledger row.
pub(crate) fn row(_detail: &str, _shown: &[Shown], _not_shown: &[NotShown]) -> ContextSourceRecord {
    ContextSourceRecord::new(CHANGED_FILES, SourceState::Absent)
}
