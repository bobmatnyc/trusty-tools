//! `mask_secret`: the one-time set confirmation (DOC-74 §15.6).
//!
//! Why: after `secrets.set`, the operator needs to see that the right value
//! went in, once. Owner ruling 2026-10-01: show the first 8 characters and
//! the length; a value of 8 characters or fewer shows only its length. This
//! is deliberately a separate function from trusty-common's `redact_secret`,
//! whose 4-character format `memory_core::filter` depends on.
//! What: [`mask_secret`]. `list` never calls it; `list` reports length and
//! `updated_at` through [`crate::api::methods::KeyMeta`] and no characters.
//! #9066: in `api`, not `store`, so the console bridge, which links `api`
//! only, masks with the same function; `store` re-exports it.
//! Test: `mask_secret_table`, `mask_secret_never_shows_a_specimen_value_whole`.

/// Characters of the head `mask_secret` reveals for a long value.
pub const MASK_HEAD_CHARS: usize = 8;

/// Mask a value for the one-time `set` confirmation.
///
/// What: `[N chars]` when the value has at most [`MASK_HEAD_CHARS`]
/// characters; otherwise the first 8 characters, an ellipsis, and
/// `[N chars]`. Counts characters, not bytes, so a multi-byte value is never
/// split mid-character.
/// Test: `mask_secret_table`.
pub fn mask_secret(value: &str) -> String {
    let length = value.chars().count();
    if length <= MASK_HEAD_CHARS {
        return format!("[{length} chars]");
    }
    let head: String = value.chars().take(MASK_HEAD_CHARS).collect();
    format!("{head}… [{length} chars]")
}
