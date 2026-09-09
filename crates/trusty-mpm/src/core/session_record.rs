//! One small per-session fact, written by the statusline and read by another
//! `tm` process (#6972 for the model, #7074 for the transcript path).
//!
//! Why: Claude Code's `statusLine` payload is the only place several session
//! facts ever reach `tm` — the authoritative model id, and the path to the
//! session's own transcript. A separate `tm` process (`tm divert`, `tm
//! commit-trailers`) needs them and shares nothing with the render but the
//! filesystem. #6972 solved that for the model; #7074 needed the identical
//! store for a second fact, and two copies of "write only when it changed,
//! atomically, never failing the render" would drift. This module is the one
//! copy; [`crate::core::session_model`] is a named facade over it.
//!
//! What: `<root>/usage/<kind>/<session_id>` holds one trimmed string.
//! [`record_session_value`] writes it, [`read_session_record`] reads it back.
//!
//! Two properties every caller depends on:
//!
//! - **The write is skipped when nothing changed.** `statusLine` fires on every
//!   render cycle; the value is read and compared first, so a steady session
//!   does one small read per render and no write at all.
//! - **Every failure is silent.** A missing home directory, an unwritable root,
//!   a session id that is not a safe filename — each returns without writing.
//!   Nothing here may cost the status bar a render.
//!
//! Both halves of a transcript record are screened, because both come from the
//! same attacker-influenceable payload: [`is_safe_session_id`] the file NAME,
//! [`contained_transcript_path`] the value (#7250).
//!
//! Test: the inline suite in `session_record_tests.rs` —
//! `a_recorded_value_reads_back`, `rejects_a_path_traversal_session_id`,
//! `an_unchanged_value_leaves_the_file_untouched`,
//! `a_blank_value_is_never_recorded`, `two_kinds_do_not_collide`,
//! `rejects_a_transcript_path_outside_the_config_dir`,
//! `rejects_a_traversing_transcript_path`,
//! `rejects_a_symlink_under_the_config_dir_aimed_outside_it`,
//! `accepts_a_transcript_path_under_the_config_dir`.

use std::path::{Component, Path, PathBuf};

/// Record kind holding the session's harness model id (#6972).
pub const KIND_MODEL: &str = "session-model";

/// Record kind holding the path to the session's Claude Code transcript
/// (#7074).
///
/// Why: the transcript is the only source of a session's output-token count —
/// the `statusLine` payload carries a dollar cost and a context-window size,
/// never tokens out — and `tm commit-trailers` runs in a different process from
/// the render that learns the path.
/// Test: `two_kinds_do_not_collide`.
pub const KIND_TRANSCRIPT: &str = "session-transcript";

/// Is `session_id` a safe single path segment?
///
/// Why: `session_id` arrives from Claude Code's stdin JSON, so it is
/// attacker-influenceable; an unchecked value would let `../../.bashrc` name
/// the file this module writes.
/// What: non-empty, and only `[A-Za-z0-9_-]`.
/// Test: `rejects_a_path_traversal_session_id`.
fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Screen a payload's `transcript_path`, returning the file it may record.
///
/// Why (#7250): `transcript_path` reaches `tm` from Claude Code's `statusLine`
/// stdin JSON on the same footing as `session_id`, which [`is_safe_session_id`]
/// already screens — and a LATER `tm` process opens whatever this store holds.
/// Unscreened, a crafted payload aims that read at any file on the machine.
/// What: the path must be absolute, carry no `..` component, and canonicalize
/// under the canonicalized `claude_config_dir`, the directory Claude Code writes
/// transcripts beneath (`<config>/projects/<slug>/<session_id>.jsonl`).
/// Canonicalization rather than lexical resolution, on BOTH sides: the
/// transcript exists by the time a render reports its path, and resolving
/// symlinks is what stops a link planted under the config directory from
/// aiming the read outside it. A path that cannot be canonicalized is rejected,
/// which costs nothing — `statusLine` fires again seconds later and records it
/// then. The returned path is the CANONICAL one, so the reader opens the file
/// that was checked rather than re-resolving the payload's spelling.
/// Test: `rejects_a_transcript_path_outside_the_config_dir`,
/// `rejects_a_traversing_transcript_path`,
/// `rejects_a_symlink_under_the_config_dir_aimed_outside_it`,
/// `accepts_a_transcript_path_under_the_config_dir`.
pub fn contained_transcript_path(
    claude_config_dir: &Path,
    transcript_path: &str,
) -> Option<PathBuf> {
    let raw = transcript_path.trim();
    if raw.is_empty() {
        return None;
    }
    let raw = Path::new(raw);
    if !raw.is_absolute() || raw.components().any(|c| c == Component::ParentDir) {
        tracing::debug!(
            path = %raw.display(),
            "statusline transcript_path is relative or traverses; not recorded"
        );
        return None;
    }
    let Ok(root) = claude_config_dir.canonicalize() else {
        tracing::debug!(
            dir = %claude_config_dir.display(),
            "Claude config directory does not resolve; transcript_path not recorded"
        );
        return None;
    };
    let Ok(canonical) = raw.canonicalize() else {
        tracing::debug!(
            path = %raw.display(),
            "statusline transcript_path does not resolve; not recorded"
        );
        return None;
    };
    if !canonical.starts_with(&root) {
        tracing::debug!(
            path = %canonical.display(),
            dir = %root.display(),
            "statusline transcript_path resolves outside the Claude config directory; not recorded"
        );
        return None;
    }
    Some(canonical)
}

/// The record's path under an explicit framework root.
///
/// Why: taking the root as an argument is what keeps the statusline writer and
/// every reader on the SAME root — each resolves `--root` / `TRUSTY_MPM_ROOT`
/// at its own call site and passes the result in, exactly as the savings ledger
/// beside it does.
/// What: `<root>/usage/<kind>/<session_id>`; `None` when the id is not a safe
/// file name.
/// Test: `a_recorded_value_reads_back`, `rejects_a_path_traversal_session_id`.
pub fn session_record_path_in(root: &Path, kind: &str, session_id: &str) -> Option<PathBuf> {
    is_safe_session_id(session_id).then(|| root.join("usage").join(kind).join(session_id))
}

/// Read the value recorded for `session_id` under `kind`.
///
/// What: the file's trimmed contents; `None` when the id is unsafe, the file is
/// absent or unreadable, or it holds only whitespace.
/// Test: `a_recorded_value_reads_back`, `reading_an_absent_record_is_none`.
pub fn read_session_record(root: &Path, kind: &str, session_id: &str) -> Option<String> {
    let path = session_record_path_in(root, kind, session_id)?;
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Remember `value` for `session_id`, writing only when it changed.
///
/// Why/What: see the module doc. No-ops on a blank value, an unsafe session id,
/// or an unchanged value; otherwise creates `<root>/usage/<kind>/` and writes
/// through a temp file renamed into place, so a reader sees either the old
/// value or the new one and never a truncated file. Every error is discarded.
/// Test: `a_recorded_value_reads_back`,
/// `an_unchanged_value_leaves_the_file_untouched`,
/// `a_blank_value_is_never_recorded`.
pub fn record_session_value(root: &Path, kind: &str, session_id: &str, value: &str) {
    use std::io::Write as _;

    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let Some(path) = session_record_path_in(root, kind, session_id) else {
        return;
    };
    if read_session_record(root, kind, session_id).as_deref() == Some(value) {
        return;
    }
    let _ = (|| -> Option<()> {
        let dir = path.parent()?;
        std::fs::create_dir_all(dir).ok()?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).ok()?;
        tmp.write_all(value.as_bytes()).ok()?;
        // On failure `PersistError::Drop` removes the temp file.
        let _ = tmp.persist(&path);
        Some(())
    })();
}

#[cfg(test)]
#[path = "session_record_tests.rs"]
mod tests;
