//! The Architect's quiet-session list, `local/quiet-sessions.txt` (#10861).
//!
//! Why: the Architect manual names `local/quiet-sessions.txt`, and the pollers
//! and `tm architect quiet` read it, but `tm fleet init` never created it, so
//! a fresh Architect's manual pointed at a missing file and
//! `tm architect quiet` exited 1.
//! What: [`step`] creates the list holding only a comment header, which every
//! reader parses as "no quiet sessions". It never touches an existing list,
//! and it leaves the list absent while a pre-#10861 `quiet-sessions.txt`
//! still sits at the Architect root, because the Python poller reads that
//! legacy file only while the new one is absent.
//! Test: `quiet_list_init_creates_an_empty_list`,
//! `quiet_list_rerun_leaves_an_edited_list_unchanged`,
//! `quiet_list_legacy_file_is_skipped_not_shadowed`,
//! `quiet_list_write_failure_is_a_failed_step`.

use std::io::Write;
use std::path::Path;

use super::Step;

/// The list's path below the Architect directory.
pub(crate) const DEST: &str = "local/quiet-sessions.txt";

/// The pre-#10861 path the Python poller still falls back to.
const LEGACY: &str = "quiet-sessions.txt";

/// The bytes of a new list: comments only, so it names no session.
pub(crate) const HEADER: &str = "\
# Sessions the Architect's poller raises no idle alerts for.
# One tmux session name per line; `#` starts a comment.
# `tm architect quiet` prints this list.
";

/// Create `<dir>/local/quiet-sessions.txt` when absent; one step.
///
/// Why: see the module doc.
/// What: an existing entry is `Unchanged` and is not opened for writing; a
/// legacy `<dir>/quiet-sessions.txt` with no new list is `Skipped` with the
/// move to make; otherwise the header is written with `create_new`, so a
/// list another process created first is never truncated. A read or write
/// error is `Failed`, which makes `init` exit 1, as `il_init::steps` and
/// `notify_hook::step` do.
/// Test: see the module doc.
pub(crate) fn step(dir: &Path) -> Step {
    // #10861: init creates the list the manual and the pollers name.
    let path = dir.join(DEST);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => {
            return Step::Failed(format!(
                "{} is a directory, not the quiet-session list; remove it and run tm fleet \
                 init again",
                path.display()
            ));
        }
        Ok(_) => return Step::Unchanged(DEST.to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Step::Failed(format!("cannot read {}: {e}", path.display())),
    }
    let legacy = dir.join(LEGACY);
    if legacy.exists() {
        return Step::Skipped(format!(
            "{DEST}: not created, because the old list {} exists; move it to {} \
             (the poller reads the old path until then)",
            legacy.display(),
            path.display()
        ));
    }
    match create(&path) {
        Ok(()) => Step::Changed(format!("created {DEST} (no quiet sessions)")),
        Err(e) => Step::Failed(format!("cannot create {}: {e}", path.display())),
    }
}

/// Write [`HEADER`] to a new file at `path`; an existing file is an error.
fn create(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(HEADER.as_bytes())?;
    file.sync_all()
}
