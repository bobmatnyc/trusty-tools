Documentation

- `kg_triple_count_or_zero`'s `Test:` pointer now names its cross-crate coverage in prose ([#5489](https://github.com/bobmatnyc/trusty-tools/pull/5489))
  - The citation of `count_active_triples_surfaces_read_failure` sat in the leading backtick run, where `scripts/check_test_pointers.sh` resolves names only against the citing crate. The test is real and lives in `trusty-common`, so the pointer lint read it as dangling and went red on main.
