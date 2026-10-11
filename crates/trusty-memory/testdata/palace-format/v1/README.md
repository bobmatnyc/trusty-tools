# Palace format 1 fixture (ADR-0067 D4)

`personal/` is a palace written by the shipped trusty-memory 0.33.0 binary,
the last release line that writes palace format 1. Never regenerate it with
newer code; it is the canary that `format_v1_fixture` (trusty-common) matches
what a real format-1 binary wrote.

Provenance (2026-10-11, #9275 PR D):

- Binary: `<cargo-bin>/trusty-memory (0.33.0)`, `trusty-memory --version`
  = `trusty-memory 0.33.0`. Not rebuilt for this fixture.
- Isolation: `env -i` with `HOME=/tmp/tmfx/home`, `TRUSTY_SANDBOX=1` and
  `TRUSTY_DATA_DIR_OVERRIDE=/tmp/tmfx/data`; daemon (`serve --foreground`)
  and client (`serve --stdio`) both run from `/tmp/tmfx/work`. The real
  embedder (fastembed all-MiniLM-L6-v2) embedded the drawers; the daemon was
  stopped with SIGTERM once both vectors were in `index.usearch.redb`.
- Each tool call passes `cwd: /tmp/tmfx/work`, so the `creator:cwd=` tags
  carry that path. Without it the stdio client injects its own `getcwd()`,
  which macOS resolves through the `/tmp` symlink to the real host path.
- Calls: `palace_create personal` (bootstrap triples), two `memory_remember`
  drawers with `force: true`, `kg_assert alice knows bob`.
- Host check: `strings -n 4` on each file finds no user name, home or
  scratch path, or session id.
- Files kept: `palace.json` (`format_version: 1`), `kg.redb` (marker 1),
  `index.usearch.redb` (bare-uuid vector keys only). Not kept: `recall.redb`,
  `l1_cache.json`, `last_used` (derived or per-run state).

SHA-256:

```
17765e1dd21b3a05b7bc2bde70429dd5e86df72143f40d1d6c7cf9a987808f44  personal/palace.json
4b94be15a4c0bd615a98319a8cf3dc8d674262dd0cace21eefe38916d074cc99  personal/kg.redb
c7778d66497cd6c07f95f96ff47492b914ea6f83a5af993f75b224b167b16227  personal/index.usearch.redb
```

Test: `the_0_33_0_v1_palace_migrates_to_2_and_its_backup_matches_the_blob`
(`crates/trusty-memory/src/commands/doctor/palace_format_tests.rs`).
