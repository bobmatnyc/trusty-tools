# trusty-kb build artifacts at `c1eed98441`

The files under `kb/` were written by trusty-kb's OKG builder, not by hand.
They are the golden registry, ledger and concept files for slice S4 of
#10746 (SPEC-OKF-01 §2.7.2: "Unchanged on-disk format"; §2.9 item 10).

How they were made, on 2026-11-04:

- A throwaway test in `crates/trusty-kb/tests/`, never committed, ran
  against trusty-kb at `389121affc` (slice S3), before S4 changed anything.
- Every file that test exercised is byte-identical to trusty-tools
  `c1eed98441a51237650b0f1ea69132bffdf23720`: `git diff c1eed98441
  389121affc -- crates/trusty-kb/src/okg crates/trusty-kb/src/put.rs` is
  empty, and `slugify` and `frontmatter::render` (moved to trusty-okf in
  S2) match their `c1eed98441` bodies. The one `store.rs` change between
  the two commits replaces `summarise` with a re-export; the builder does
  not call it.
- The test registered two doc stores (one `user_authored`), two `gmail`
  rows and two `drive` rows, widened the `sent-mail` window in place,
  ingested two doc-store items, changed one, swept the other into a
  tombstone, and ingested one `gmail` item with a non-ASCII title.

Do not edit these files. They pin what an existing tree looks like when
trusty-okf first reads it. Used by `crates/trusty-okf/tests/build_criteria.rs`.
