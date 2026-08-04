Fixed

- Test integrity: four test-only defects that let the suite report green
  without proving what it claims.
  - The `#2178` P0 root-hijack data-loss guard
    (`reindex_refuses_untrusted_root_move_and_preserves_corpus`) no longer
    isolates `indexes.toml` with a process-global
    `std::env::set_var("TRUSTY_DATA_DIR", …)` behind `#[serial]`. `#[serial]`
    excludes only other serial tests, so a non-serial sibling could still
    clobber the variable mid-test; `load_index_registry` then found no
    persisted entry, the gate trusted the hijacked root, and the assertion
    flipped `Failed` → `Complete`. Both tests in that file now run alone in a
    child process whose data dir is supplied at spawn time — deterministic
    isolation with the assertions unchanged
    ([#4213](https://github.com/bobmatnyc/trusty-tools/issues/4213)).
  - Unit tests no longer register index entries in the developer's real
    daemon registry. `data_dir()`'s un-overridden fallback resolves to an
    isolated per-process directory in test builds, so a test that forgets to
    set `TRUSTY_DATA_DIR` can no longer write throwaway fixtures pointing at
    `~/.trusty-search-test-roots/…` into the live `indexes.toml` — the debris
    that kept `search_health` reporting `degraded`
    ([#4094](https://github.com/bobmatnyc/trusty-tools/issues/4094)).
  - `test_trim_heap_never_increases_rss_after_bulk_free` no longer asserts an
    invariant that concurrent test execution can break. It sampled
    whole-process RSS either side of `trim_heap()`, so any sibling test
    allocating in that window failed the bound with `malloc_trim` behaving
    perfectly (observed 181 MB → 194 MB on an unrelated PR). Following the
    `#3705` precedent, the bound is now a sanity band plus a calibrated
    concurrency-noise budget, and the raw before/after MB numbers are always
    printed ([#3954](https://github.com/bobmatnyc/trusty-tools/issues/3954)).
  - The `#2847` legacy-path regression tests now force their failure at
    `index_data_dir()`'s own `create_dir_all` (`ENOTDIR`) rather than one
    layer above at `data_dir()`'s, matching the precision of their colocated
    counterpart, which blocks the immediate dispatch target
    ([#3963](https://github.com/bobmatnyc/trusty-tools/issues/3963)).
