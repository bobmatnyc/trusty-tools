//! `mask_secret`: the one-time set confirmation (DOC-74 §15.6).
//!
//! Why: after `secrets.set`, the operator needs to see that the right value
//! went in, once, without the confirmation exposing the value. #10394: owner
//! ruling 2026-10-28 replaces the 2026-10-01 head-8 format, which printed the
//! first 8 characters of a stored value. This is deliberately a separate
//! function from trusty-common's `redact_secret`, whose 4-character format
//! `memory_core::filter` depends on.
//! What: [`mask_secret`]. `list` never calls it; `list` reports length and
//! `updated_at` through [`crate::api::methods::KeyMeta`] and no characters.
//! #9066: in `api`, not `store`, so the console bridge, which links `api`
//! only, masks with the same function; `store` re-exports it.
//! Test: `mask_secret_table`, `mask_secret_never_shows_a_specimen_value_whole`.

/// Characters of the head `mask_secret` reveals, for a value of at least
/// [`MASK_HEAD_MIN_CHARS`] characters.
pub const MASK_HEAD_CHARS: usize = 4;

/// Characters of the tail `mask_secret` reveals, for a value of at least
/// [`MASK_TAIL_MIN_CHARS`] characters.
pub const MASK_TAIL_CHARS: usize = 5;

/// Shortest value whose head `mask_secret` reveals.
pub const MASK_HEAD_MIN_CHARS: usize = 16;

/// Shortest value whose tail `mask_secret` reveals; anything shorter shows
/// only its length.
pub const MASK_TAIL_MIN_CHARS: usize = 10;

/// Mask a value for the one-time `set` confirmation.
///
/// Why: #10394 — the confirmation printed the first 8 characters of a value.
/// What: by character count `n` (owner ruling 2026-10-28):
/// `n < 10` gives `[n chars]`; `10 <= n < 16` gives `…<last 5> [n chars]`;
/// `n >= 16` gives `<first 4>…<last 5> [n chars]`. Counts characters, not
/// bytes, so a multi-byte value is never split mid-character. At most 9
/// characters of a value are ever shown, and never more than `n - 5`.
/// Test: `mask_secret_table`, `mask_secret_never_shows_a_specimen_value_whole`.
pub fn mask_secret(value: &str) -> String {
    let length = value.chars().count();
    if length < MASK_TAIL_MIN_CHARS {
        return format!("[{length} chars]");
    }
    let head: String = if length >= MASK_HEAD_MIN_CHARS {
        value.chars().take(MASK_HEAD_CHARS).collect()
    } else {
        String::new()
    };
    // `length >= MASK_TAIL_MIN_CHARS > MASK_TAIL_CHARS`, so no underflow.
    let tail: String = value.chars().skip(length - MASK_TAIL_CHARS).collect();
    format!("{head}…{tail} [{length} chars]")
}

/// The `set` confirmation for a caller under Claude Code: `[n chars]` only.
///
/// Why: #10459 — an agent's confirmation lands in its transcript, so it
/// shows no character of the value, not even `mask_secret`'s head or tail.
/// What: the character count `n`, formatted `[n chars]`.
/// Test: `mask_length_only_shows_no_value_character`,
/// `set_by_agent_confirmation_shows_length_only`.
pub fn mask_length_only(value: &str) -> String {
    format!("[{} chars]", value.chars().count())
}
