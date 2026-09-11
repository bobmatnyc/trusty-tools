Fixed

- `compress::filter_test_runner` keeps a failed test run readable. It was an
  allowlist — a line survived only by containing `FAILED`/`error`/`warning` or
  starting `---- ` / `failures:` — so the panic location, the `left:`/`right:`
  assertion values and every multiline context line were dropped as noise, and
  only the LAST `test result:` line was kept, which left a failing suite
  followed by a passing one compressed to output ending `test result: ok`. It
  is now a blocklist: passing and ignored per-test lines and cargo's indented
  build-progress verbs are dropped, every other line is kept in its original
  position, so every suite summary survives in order beside the diagnostics
  that explain it and an unrecognised harness's output is kept rather than
  discarded. A green run still compresses to its `running`/`Running` lines and
  summaries (#7544).
