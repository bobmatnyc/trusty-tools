//! The per-file decision `seed::deploy` acts on and the launch deploy plans with (#11368).
//!
//! Why: the launch deploy keeps a copy of every file `seed::deploy` is about
//! to replace (#11368 Q1). Planned and applied from one function, the two can
//! never disagree about which files are replaced.
//! What: [`of_live`] classifies a present file's bytes against the shipped
//! bytes and the shipped history.
//! Test: `a_refresh_updates_an_unedited_earlier_copy`,
//! `an_edited_file_is_never_backed_up_or_overwritten_11368`.

use super::ShippedBefore;
use super::refresh;

/// What `seed::deploy` does with a present file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// The bytes are the shipped bytes; nothing to write.
    Same,
    /// An instance-owned file (#10608); kept as is.
    InstanceKept,
    /// An unedited earlier shipped copy (#10571); replaced.
    Replace,
    /// Someone's edit; kept, the shipped copy goes to `<file>.shipped` (#11245).
    Edited,
}

/// The [`Disposition`] of `live`, the bytes on disk at `dest`.
///
/// Test: `a_refresh_updates_an_unedited_earlier_copy`,
/// `an_upgrade_keeps_instance_files_and_refreshes_framework_files_10608`.
pub(crate) fn of_live(
    dest: &str,
    shipped: &[u8],
    live: &[u8],
    shipped_before: ShippedBefore<'_>,
) -> Disposition {
    if live == shipped {
        Disposition::Same
    } else if refresh::instance_owned(dest) {
        Disposition::InstanceKept
    } else if shipped_before(dest, live) {
        Disposition::Replace
    } else {
        Disposition::Edited
    }
}
