Fixed

- `assistants::tests::home_tests` guarded its `$HOME` mutations with
  `#[serial]` while the rest of the crate guards the same global with
  `test_env::HOME_LOCK` — two disjoint locks, so the two groups interleaved and
  `okg_store_path_matches_the_owners_spelling` compared paths under two
  different tempdirs (#6089). Those tests now take `HOME_LOCK` as well, and a
  new `home_lock_discipline` test fails any source file that mutates `$HOME`
  without acquiring it, so the point fix cannot silently lapse again as #3952's
  did.
