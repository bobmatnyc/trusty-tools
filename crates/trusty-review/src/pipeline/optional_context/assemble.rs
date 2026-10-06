//! The refs corpus both review paths hand the citation gate (#9192).
//!
//! Why: the unified and map-reduce paths each built the corpus inline from the
//! same six sources; one builder keeps the two from drifting.
//! What: [`refs_for_gate`] joins the PR title, PR body, external context and
//! the three caller fields through `refs_corpus`, in that order.
//! Test: `off_refs_corpus_is_byte_identical`,
//! `off_keeps_the_raw_body_in_the_refs_corpus`.

use crate::pipeline::withheld_contract::refs_corpus;

/// The text a `[gh:]`/`[jira:]`/`[confluence:]` citation must resolve in.
///
/// `caller` is `[pr_description, pr_discussion, referenced_code]` as the
/// reviewer saw them.
pub(crate) fn refs_for_gate(
    title: &str,
    body: &str,
    external: &str,
    caller: [Option<&str>; 3],
) -> String {
    let [description, discussion, referenced] = caller;
    refs_corpus(&[
        Some(title),
        Some(body),
        Some(external),
        description,
        discussion,
        referenced,
    ])
}
