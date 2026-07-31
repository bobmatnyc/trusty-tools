Fixed

- four non-hermetic test fixtures no longer redden the shared gate for reasons unrelated to the change under test (closes [#4306](https://github.com/bobmatnyc/trusty-tools/issues/4306), [#4415](https://github.com/bobmatnyc/trusty-tools/issues/4415); refs [#4407](https://github.com/bobmatnyc/trusty-tools/issues/4407), [#3451](https://github.com/bobmatnyc/trusty-tools/issues/3451))
  - The dead-daemon address fixture no longer binds an ephemeral port and drops
    it — a released port returns to the OS pool, so another binder could be
    handed it and the "nothing is listening here" premise silently expired
    (measured: reassigned and accepting connections after one wrap of the
    ephemeral range). All three duplicate copies, across 13 call sites, now
    share one fixture on `127.0.0.1:1` — privileged, so unbindable, and outside
    every ephemeral range — matching the convention already used by
    trusty-review and trusty-analyze.
  - `telegram::supervisor`'s healthy-run backoff-reset test runs on tokio's
    paused clock instead of asserting a 20ms wall-clock bound against a 2ms
    policy base, which measured scheduler jitter rather than backoff state.
    The assertions are now exact equalities against literal delays (2ms, 8ms,
    32ms), which also catch a partial reset the old bound accepted.
  - The plain-mode banner tests, and `pm_guard_bash`'s TMPDIR/HOME expansion
    test, joined the default `#[serial]` group. The banner tests' file-local
    mutex serialised them against each other and nothing else, so they raced the
    other `$HOME` mutators in the same test binary — 19 HOME-mutating statements
    in 8 fns across 4 files in the `tm` bin target. One banner test also seeded
    `banner.txt` into the ambient home instead of its own sandbox, and the
    `pm_guard_bash` test left `HOME` repointed for the rest of the binary; it now
    restores both `HOME` and `TMPDIR` through an RAII guard, so an assertion
    panic cannot skip the cleanup.
  - Env-isolation audits are scoped per TEST TARGET, not per crate: `#[serial]`
    coordinates threads inside one test binary and process-global `$HOME` is per
    process, so neither can span targets. The trusty-mpm lib target's own HOME
    mutators were never in this race.
