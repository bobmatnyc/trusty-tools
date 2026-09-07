//! Where the PARENT session's model comes from, and where it is remembered (#6972).
//!
//! Why: the divert savings producer prices the tokens a diversion avoided at the
//! parent session's published input rate. Before this module it had two sources
//! and neither was populated on an ordinary machine — Claude Code exports no
//! model variable to a hook child, so `ANTHROPIC_MODEL` is absent, and a config
//! with no `[models]` section resolves to the Sonnet default. An Opus session's
//! diversions therefore priced at a fifth of what they saved, and three smoke
//! diversions wrote no row at all because the Haiku worker's own bill exceeded
//! the understated delta.
//!
//! What: the authoritative model id reaches `tm` on exactly one path — the
//! `statusLine` hook's stdin JSON, which carries `model.id` (the PreToolUse
//! payload `tm hook` receives carries no model field at all; see
//! `hook_payload::PASSTHROUGH_FIELDS`). [`record_session_model`] writes that id
//! to `<root>/usage/session-model/<session_id>` and [`read_session_model`] reads
//! it back in the separate `tm divert` process. The four `MODEL_SOURCE_*`
//! constants name which source answered, so a row priced from the last-resort
//! config chain is diagnosable from the ledger line alone.
//!
//! Two properties the statusline depends on:
//!
//! - **The write is skipped when nothing changed.** `statusLine` fires on every
//!   render cycle; the store is read and compared first, so a session that has
//!   not switched models does one small read per render and no write at all.
//! - **Every failure is silent.** A missing home directory, an unwritable root,
//!   a session id that is not a safe filename — each returns without writing.
//!   Nothing here may cost the status bar a render.
//!
//! Test: the inline suite in `session_model_tests.rs` —
//! `a_recorded_model_reads_back`, `rejects_a_path_traversal_session_id`,
//! `an_unchanged_model_leaves_the_file_untouched`,
//! `a_blank_model_is_never_recorded`, `reading_an_absent_record_is_none`.

use std::path::{Path, PathBuf};

/// `model_source` written when `ANTHROPIC_MODEL` named the parent's model.
///
/// Why (#6972): the operator pinning a model through the environment is an
/// explicit override, so it outranks everything the harness inferred.
/// Test: `env_wins_over_every_other_source`.
pub const MODEL_SOURCE_ENV: &str = "env";

/// `model_source` written when the statusline record named the parent's model.
///
/// Why (#6972): this is the only source carrying the model Claude Code is
/// actually running, so it is the one a correct price normally comes from.
/// Test: `the_statusline_record_outranks_the_config_chain`.
pub const MODEL_SOURCE_STATUSLINE: &str = "statusline";

/// `model_source` written when nothing better than the config chain answered.
///
/// Why (#6972): the config chain has a Sonnet default, so it always answers and
/// it is right only by luck. A row carrying this value states that its price is
/// a guess — which is exactly what made the original under-reporting invisible.
/// Test: `the_config_chain_is_the_last_resort`.
pub const MODEL_SOURCE_CONFIG_FALLBACK: &str = "config-fallback";

/// `model_source` written by the instruction-compression producer.
///
/// Why (#6972): that producer runs at session launch, where the config chain is
/// not a fallback at all — it is the chain that produced the session's own
/// `--model` flag, so it is authoritative there. Labelling it differently from
/// [`MODEL_SOURCE_CONFIG_FALLBACK`] keeps "the model is a guess" readable off
/// the ledger.
/// Test: `instruction_compression_row_names_its_model_source`.
pub const MODEL_SOURCE_LAUNCH_CONFIG: &str = "launch-config";

/// Whether `session_id` is safe to use as a file name.
///
/// Why: the id arrives on Claude Code's stdin JSON, so it is
/// attacker-influenceable; joining `../../.bashrc` onto the framework root would
/// be a path traversal. The rule is deliberately the same one
/// `statusline::compaction` already applies to the same value.
/// What: non-empty and entirely `[A-Za-z0-9_-]`.
/// Test: `rejects_a_path_traversal_session_id`.
fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The record's path under an explicit framework root.
///
/// Why: taking the root as an argument is what keeps the statusline writer and
/// the `tm divert` reader on the SAME root — both resolve `--root` /
/// `TRUSTY_MPM_ROOT` at their own call site and pass the result in, exactly as
/// the savings ledger beside it does.
/// What: `<root>/usage/session-model/<session_id>`; `None` when the id is not a
/// safe file name.
/// Test: `a_recorded_model_reads_back`, `rejects_a_path_traversal_session_id`.
pub fn session_model_path_in(root: &Path, session_id: &str) -> Option<PathBuf> {
    is_safe_session_id(session_id)
        .then(|| root.join("usage").join("session-model").join(session_id))
}

/// Read the model id recorded for `session_id`, if a statusline render wrote one.
///
/// Why: `tm divert` is a separate process from the statusline hook, so the only
/// thing it can share with the render is a file.
/// What: the file's trimmed contents; `None` when the id is unsafe, the file is
/// absent or unreadable, or it holds only whitespace.
/// Test: `a_recorded_model_reads_back`, `reading_an_absent_record_is_none`.
pub fn read_session_model(root: &Path, session_id: &str) -> Option<String> {
    let path = session_model_path_in(root, session_id)?;
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Remember the model `session_id` is running, writing only when it changed.
///
/// Why: this runs on Claude Code's hot render path, where an unconditional write
/// per render cycle would be pure waste — the model changes at most a handful of
/// times in a session. Reading and comparing first turns the steady state into
/// one small read.
/// What: no-ops on a blank model, an unsafe session id, or an unchanged value;
/// otherwise creates `<root>/usage/session-model/` and writes the id through a
/// temp file renamed into place, so a reader sees either the old id or the new
/// one and never a truncated file. Every error is discarded.
/// Test: `a_recorded_model_reads_back`,
/// `an_unchanged_model_leaves_the_file_untouched`,
/// `a_blank_model_is_never_recorded`.
pub fn record_session_model(root: &Path, session_id: &str, model_id: &str) {
    use std::io::Write as _;

    let model_id = model_id.trim();
    if model_id.is_empty() {
        return;
    }
    let Some(path) = session_model_path_in(root, session_id) else {
        return;
    };
    if read_session_model(root, session_id).as_deref() == Some(model_id) {
        return;
    }
    let _ = (|| -> Option<()> {
        let dir = path.parent()?;
        std::fs::create_dir_all(dir).ok()?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).ok()?;
        tmp.write_all(model_id.as_bytes()).ok()?;
        // On failure `PersistError::Drop` removes the temp file.
        let _ = tmp.persist(&path);
        Some(())
    })();
}

#[cfg(test)]
#[path = "session_model_tests.rs"]
mod tests;
