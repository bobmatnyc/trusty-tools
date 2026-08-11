Added

- `kg_triple_count_or_zero`'s degrade now has in-crate test coverage ([#5489](https://github.com/bobmatnyc/trusty-tools/pull/5489))
  - The degrade rule moved into its own `triple_count_or_zero(palace, read)` helper that takes the already-performed read as a parameter, so `triple_count_or_zero_degrades_a_failed_read_to_zero` can pin the arm #5384 cares about — a failed count must become a *logged* `0`, never a silent one. `KgStoreRedb::db()` is `pub(super)` to `trusty-common`, so the failure cannot otherwise be induced from this crate, and the error arm was unexecuted here.
  - Behaviour at every call site is unchanged.
