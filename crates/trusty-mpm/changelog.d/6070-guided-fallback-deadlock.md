Fixed

- `tm launch` no longer walks the entire home directory before starting a
  session ([#5875](https://github.com/bobmatnyc/trusty-tools/issues/5875),
  [#6070](https://github.com/bobmatnyc/trusty-tools/issues/6070)). Step 8 calls
  `remove_global_trusty_mpm_hooks`, which reached `~/.claude/settings.json` and
  `~/.claude/settings.local.json` by recursing eight levels into `$HOME`. On a
  real developer machine one `opendir()` inside that walk blocked indefinitely,
  so the launch never returned; a stack sample showed 2328 of 2328 samples in a
  single `open$NOCANCEL`. The strip now names the two global files directly.
  Project settings files elsewhere under `$HOME` are no longer rewritten as a
  side effect of launching — `tm hooks clean` owns that machine-wide sweep, the
  same split `tm install` adopted in #2940.
- The eight `guided_fallback_*` tests in `--bin tm` terminate again. They drive
  `launch()` end to end, so they inherited the hang, and because they hold the
  process-wide `serial_test` lock while stuck they starved every other
  `#[serial]` test in the binary — which is why `cargo test -p trusty-mpm`
  needed `--skip guided_fallback` to complete at all. That workaround is no
  longer required.
- `remove_global_trusty_mpm_hooks_at` takes the home directory as a parameter,
  which gives the removal path its first real test coverage. Four doc comments
  cited `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`, a test
  that did not exist.
