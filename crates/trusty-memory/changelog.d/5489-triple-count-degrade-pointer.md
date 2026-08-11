Fixed

- The `Test:` pointer on `kg_triple_count_or_zero` cited a test that lives in `trusty-common`, which `scripts/check_test_pointers.sh` resolves only within the citing crate — so `main` was red on the doc-comment pointer lint and blocked every PR that rebased onto it ([#5489](https://github.com/bobmatnyc/trusty-tools/pull/5489))
  - The degrade rule is now its own `triple_count_or_zero(palace, read)` helper, so the error arm #5384 cares about — a failed count must become a *logged* `0`, never a silent one — is exercised by an in-crate test instead of only by the upstream test that raises the error. `KgStoreRedb::db()` is `pub(super)` to `trusty-common`, so the failure cannot otherwise be induced from this crate.
  - The upstream reference survives as prose in the doc comment; behaviour at every call site is unchanged.
