Fixed

- `kg_triple_count_or_zero`'s doc comment cited `count_active_triples_surfaces_read_failure` as a backtick span, which `check_test_pointers.sh` only resolves within the citing file's own crate — the test lives in `trusty-common`, so the lint reported it dangling. Rephrased the citation to name the crate in prose per the lint's documented convention for cross-crate references; no behavior or test change ([PR #5506](https://github.com/bobmatnyc/trusty-tools/pull/5506))
