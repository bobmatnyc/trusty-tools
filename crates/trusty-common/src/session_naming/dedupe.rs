//! Auto-suffix-on-collision name deduplication (issue #3692).
//!
//! Why: split out of the parent `session_naming` module to keep it under its
//! 500-SLOC production cap (adding this pushed it to 531 SLOC) — same
//! extraction pattern trusty-mpm's `session_manager` submodules already use
//! (`naming.rs`, `rename.rs`, `adopt.rs` are all siblings of `manager.rs` for
//! the identical reason). [`dedupe_by_ordinal`] is re-exported by the parent
//! module so every existing `session_naming::dedupe_by_ordinal` caller keeps
//! compiling unchanged.
//! What: [`dedupe_by_ordinal`] (auto-suffix on collision, never reject — the
//! owner decision behind issue #3692: two distinct managed sessions were
//! found sharing one name) and its private helper [`split_trailing_ordinal`].
//! Test: `dedupe_*`, `split_trailing_ordinal_*` below.

/// Ceiling on ordinal-suffix attempts before falling back to a timestamp
/// suffix (issue #3692).
///
/// Why: [`dedupe_by_ordinal`] must never loop forever if a caller's `is_taken`
/// predicate is pathologically "always true" (e.g. a bug elsewhere) — a large
/// but bounded ceiling keeps the function total while still comfortably
/// covering any realistic number of concurrently-named siblings.
const MAX_DEDUPE_ORDINAL: u32 = 9_999;

/// Split `name`'s trailing `-<digits>` ordinal suffix, if it has one.
///
/// Why: [`dedupe_by_ordinal`] must not double-suffix an already-numbered name
/// (`tm-trusty-tools-01` colliding must become `tm-trusty-tools-02`, never
/// `tm-trusty-tools-01-2`) — recognising and incrementing an EXISTING trailing
/// ordinal, whatever its width, is how it avoids that.
/// What: returns `(base, parsed_number, digit_width)` when `name` ends in
/// `-` followed by 1-6 ASCII digits; `None` otherwise (no trailing dash, no
/// digits, or a non-digit character in the suffix).
/// Test: `split_trailing_ordinal_*`.
fn split_trailing_ordinal(name: &str) -> Option<(&str, u32, usize)> {
    let dash = name.rfind('-')?;
    let base = &name[..dash];
    let digits = &name[dash + 1..];
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    Some((base, n, digits.len()))
}

/// Make `candidate` unique against `is_taken`, auto-suffixing on collision —
/// NEVER rejecting (owner decision, issue #3692: two distinct managed
/// sessions could hold the identical name, making name-based resume ambiguous
/// and the picker show duplicate rows; the fix is to always suffix, never
/// refuse).
///
/// Why: every name-allocation site (session create, explicit adopt, rename-to)
/// needs IDENTICAL collision-avoidance semantics — a caller-supplied or
/// derived name that's already held by a live/tracked entity must become
/// `<name>-2`, `<name>-3`, … (mirroring the same `-2`, `-3`, … convention
/// `trusty-agents`' own `TmManager::unique_tmux_name` already uses) rather
/// than erroring and forcing the caller to pick a different name by hand.
/// What: if `candidate` is free (`!is_taken(candidate)`), returns it unchanged
/// — the common case. Otherwise, finds the smallest free ordinal: starting
/// from 2 (or one past an existing trailing ordinal — see
/// [`split_trailing_ordinal`] — so `tm-tagents-2` colliding yields
/// `tm-tagents-3`, not a second `-2` suffix), it probes `<base>-<n>` upward
/// until `is_taken` returns `false`, preserving the original suffix's digit
/// width when incrementing an existing one (`-01` collides → `-02`, not
/// `-2`). Falls back to a millisecond-timestamp suffix after
/// [`MAX_DEDUPE_ORDINAL`] probes (pathological — should never trigger in
/// practice).
/// Test: `dedupe_returns_candidate_when_free`,
/// `dedupe_appends_2_on_first_collision`,
/// `dedupe_increments_past_multiple_collisions`,
/// `dedupe_increments_existing_ordinal_suffix`,
/// `dedupe_preserves_zero_padded_width`,
/// `dedupe_skips_gaps_left_by_freed_names`.
pub fn dedupe_by_ordinal(candidate: &str, is_taken: impl Fn(&str) -> bool) -> String {
    if !is_taken(candidate) {
        return candidate.to_string();
    }
    let (base, mut n, width) = match split_trailing_ordinal(candidate) {
        Some((base, n, width)) => (base.to_string(), n.saturating_add(1), width),
        None => (candidate.to_string(), 2, 0),
    };
    loop {
        let attempt = if width > 1 {
            format!("{base}-{n:0width$}")
        } else {
            format!("{base}-{n}")
        };
        if !is_taken(&attempt) {
            return attempt;
        }
        if n >= MAX_DEDUPE_ORDINAL {
            // Pathological fallback: a millisecond timestamp suffix is, for
            // all practical purposes, always free.
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default();
            return format!("{base}-{millis}");
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── dedupe_by_ordinal (issue #3692) ─────────────────────────────────────

    /// Why: the common case — no collision at all — must return the candidate
    /// completely unchanged, never probing `is_taken` for a suffixed variant.
    /// Test: itself.
    #[test]
    fn dedupe_returns_candidate_when_free() {
        let name = dedupe_by_ordinal("tm-tagents", |_| false);
        assert_eq!(name, "tm-tagents");
    }

    /// Why (owner's worked example, issue #3692): a bare, non-numbered name
    /// colliding with exactly one other must become `<name>-2` — never
    /// rejected.
    /// Test: itself.
    #[test]
    fn dedupe_appends_2_on_first_collision() {
        let taken = ["tm-tagents"];
        let name = dedupe_by_ordinal("tm-tagents", |c| taken.contains(&c));
        assert_eq!(name, "tm-tagents-2");
    }

    /// Why: a third colliding session must skip past an already-taken `-2` to
    /// `-3`, not loop back to `-2` or fail.
    /// Test: itself.
    #[test]
    fn dedupe_increments_past_multiple_collisions() {
        let taken = ["tm-tagents", "tm-tagents-2"];
        let name = dedupe_by_ordinal("tm-tagents", |c| taken.contains(&c));
        assert_eq!(name, "tm-tagents-3");
    }

    /// Why: if `-2` was freed (e.g. its session was decommissioned and the
    /// caller's `is_taken` no longer reports it), the smallest free ordinal is
    /// `-2` again — dedupe must not skip ahead of a genuinely free slot.
    /// Test: itself.
    #[test]
    fn dedupe_skips_gaps_left_by_freed_names() {
        // `-2` is free; `-3` is taken (mirrors "01,03 exist, 02 was freed").
        let taken = ["tm-tagents", "tm-tagents-3"];
        let name = dedupe_by_ordinal("tm-tagents", |c| taken.contains(&c));
        assert_eq!(name, "tm-tagents-2");
    }

    /// Why: a candidate that ALREADY carries a trailing ordinal (e.g. a rename
    /// target that happens to look like `tm-tagents-2`) must increment that
    /// existing ordinal on collision, never double-suffix into
    /// `tm-tagents-2-2`.
    /// Test: itself.
    #[test]
    fn dedupe_increments_existing_ordinal_suffix() {
        let taken = ["tm-tagents-2"];
        let name = dedupe_by_ordinal("tm-tagents-2", |c| taken.contains(&c));
        assert_eq!(name, "tm-tagents-3");
    }

    /// Why: the zero-padded `tm-<leaf>-NN` scheme (#1955) must keep its width
    /// when auto-suffixed — `tm-trusty-tools-01` colliding must become
    /// `tm-trusty-tools-02`, not the unpadded `tm-trusty-tools-2` (which would
    /// look like a foreign/different scheme next to sibling `-01`/`-03` names).
    /// Test: itself.
    #[test]
    fn dedupe_preserves_zero_padded_width() {
        let taken = ["tm-trusty-tools-01"];
        let name = dedupe_by_ordinal("tm-trusty-tools-01", |c| taken.contains(&c));
        assert_eq!(name, "tm-trusty-tools-02");
    }

    /// Why: `split_trailing_ordinal` must not mistake a non-numeric or absent
    /// suffix for an ordinal — a plain hyphenated leaf like `tm-my-project`
    /// has no trailing ordinal to increment.
    /// Test: itself.
    #[test]
    fn split_trailing_ordinal_rejects_non_numeric_suffix() {
        assert_eq!(split_trailing_ordinal("tm-my-project"), None);
        assert_eq!(split_trailing_ordinal("no-dash-at-all-x"), None);
    }

    /// Why: a genuine `-NN`-style suffix must parse to its numeric value and
    /// digit width so the caller can increment it in place.
    /// Test: itself.
    #[test]
    fn split_trailing_ordinal_parses_numeric_suffix() {
        assert_eq!(
            split_trailing_ordinal("tm-trusty-tools-01"),
            Some(("tm-trusty-tools", 1, 2))
        );
        assert_eq!(
            split_trailing_ordinal("tm-tagents-2"),
            Some(("tm-tagents", 2, 1))
        );
    }
}
