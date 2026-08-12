Fixed

- **The test suite no longer registers its fixture directories in the
  developer's live trusty-search daemon, which was writing index files back
  into the sandbox and failing unrelated `run_task` tests
  ([#3036](https://github.com/bobmatnyc/trusty-tools/issues/3036),
  [#3195](https://github.com/bobmatnyc/trusty-tools/issues/3195)).**
  `trusty_common::search_index` refuses daemon writes while
  `running_under_test_harness()` holds (#4255), but that answers for the
  running process, and `env!("CARGO_BIN_EXE_tcode")` is `target/<profile>/tcode`
  — outside `deps/` — so a spawned child looked like a real user invocation and
  warmed its `--project` into whatever daemon it discovered. The daemon then
  wrote `.gitignore` and `.trusty-search/{index.redb,hnsw.usearch,…}` into the
  test's own tempdir, at a moment nobody controlled, breaking whichever
  before/after diff assertion was open — which is why a different test failed
  each run, only on a machine with a daemon, and never under
  `--test-threads=1`. `tests/support/mod.rs` set `TRUSTY_TEST_HARNESS=1` on the
  two children it owned; 26 tests built their own `Command` and did not. All of
  them now go through the one guarded constructor `support::tcode_command()`,
  and `no_test_spawns_the_tcode_binary_unguarded` fails the build if a new test
  names the binary directly. One `cargo test -p trusty-code` used to leave 3
  new indexes behind in the operator's daemon; it now leaves none.
