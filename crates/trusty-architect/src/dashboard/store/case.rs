//! Names that differ only in ASCII letter case.
//!
//! Why: on a case-insensitive filesystem (APFS, the Mac default)
//! `items/A01.json` and `items/a01.json` are one file, so in the `dir`
//! format `set a01` overwrote `A01` and `get a01` answered with `A01`'s body
//! (#10063 df-G01, code-critic repro). The same holds for collection
//! directories and `*.jsonl` files. A `jsonl` store keeps ids apart, but the
//! two formats must behave the same, so both refuse.
//! What: [`variant`] finds an existing name equal to a new one ignoring ASCII
//! case but not exactly; [`refusal`] is the error naming both.
//! Test: `case_variant_id_write_refused_and_writes_nothing`,
//! `case_variant_in_one_batch_refused`, `case_variant_collection_refused`,
//! `get_never_answers_under_a_case_variant_name`.

use super::super::error::DashError;

/// The first of `existing` that equals `name` ignoring ASCII case, but not
/// exactly.
pub(super) fn variant<'a>(
    name: &str,
    existing: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    existing
        .into_iter()
        .find(|e| *e != name && e.eq_ignore_ascii_case(name))
}

/// The refusal for `name` (a `coll/id` or a collection) that `other` shadows.
pub(super) fn refusal(kind: &str, name: &str, other: &str) -> DashError {
    DashError::Invalid(format!(
        "{kind} {name} refused: it differs from existing {kind} {other} only in letter case, \
         and such names share one file on a case-insensitive filesystem"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_ignores_exact_and_unrelated_names() {
        assert_eq!(variant("a01", ["A01", "B01"]), Some("A01"));
        assert_eq!(variant("A01", ["A01", "a02"]), None);
        assert_eq!(variant("Items", ["items"]), Some("items"));
        // Non-ASCII letters are not folded: only ASCII case collides here.
        assert_eq!(variant("é", ["É"]), None);
    }
}
