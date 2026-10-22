//! Names that differ only in ASCII letter case.
//!
//! Why: on a case-insensitive filesystem (APFS, the Mac default)
//! `items/A01.json` and `items/a01.json` are one file, so in the `dir`
//! format `set a01` overwrote `A01` and `get a01` answered with `A01`'s body
//! (#10063 df-G01, code-critic repro). The same holds for collection
//! directories and `*.jsonl` files, including two collections of one batch:
//! `[set Items/A01, set items/B01]` left one `Items.jsonl` holding only B01.
//! A `jsonl` store keeps ids apart, but the two formats must behave the
//! same, so both refuse.
//! What: [`variant`] finds an existing name equal to a new one ignoring ASCII
//! case but not exactly; [`refusal`] is the error naming both;
//! [`Store::refuse_case_variants`] applies them to one staged op, against
//! the disk listing and the earlier ops of its batch.
//! Test: `case_variant_id_write_refused_and_writes_nothing`,
//! `case_variant_in_one_batch_refused`, `case_variant_collection_refused`,
//! `case_variant_collections_in_one_batch_refused`,
//! `get_never_answers_under_a_case_variant_name`.

use super::super::error::DashError;
use super::Store;
use super::stage::Staged;

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

impl Store {
    /// Refuses an op on `coll/id` when `coll` is a case variant of a
    /// collection an earlier op of this batch touched, or, for a `write`,
    /// when the store or an earlier op holds a case variant of `id`. The
    /// disk's collections are [`Store::guard`]'s check.
    pub(super) fn refuse_case_variants(
        &self,
        staged: &mut Staged,
        coll: &str,
        id: &str,
        write: bool,
    ) -> Result<(), DashError> {
        // #10063 df-G01: every collection a batch touches is listed once, so
        // the listing's keys are this batch's collections so far.
        if let Some(other) = variant(coll, staged.listed.keys().map(String::as_str)) {
            return Err(refusal("collection", coll, other));
        }
        if !write {
            return Ok(());
        }
        self.list_once(staged, coll)?;
        let on_disk = staged.listed.get(coll).into_iter().flatten();
        let written = staged
            .slots
            .iter()
            .filter(|s| s.coll == coll && s.exists)
            .map(|s| &s.id);
        match variant(id, on_disk.chain(written).map(String::as_str)) {
            Some(other) => Err(refusal(
                "doc",
                &format!("{coll}/{id}"),
                &format!("{coll}/{other}"),
            )),
            None => Ok(()),
        }
    }
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
