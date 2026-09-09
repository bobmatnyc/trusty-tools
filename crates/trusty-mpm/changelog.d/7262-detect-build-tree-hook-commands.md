Fixed

- A hook command whose executable lives in a Cargo build tree is now recognised
  as tm's own contamination whatever the binary is named, so `tm doctor` reports
  it, `tm hooks clean` removes it, and the hooks writer REPLACES it instead of
  adding a correct group beside it. Both name-based branches of
  `is_mpm_hook_command` asked what the binary was called, so
  `<repo>/target-7247/debug/deps/test_session_lifecycle-<hash> hook --pm-guard` —
  what #7244 wrote into a real project's `settings.json` seven times in one day —
  was invisible to every one of the three. The new branch asks WHERE the
  executable lives instead, and covers the `--pm-guard` and `--divert-check`
  argv shapes the old ` hook` suffix check could not reach. A build-tree path
  alone is not enough: the argv must also be one tm itself writes, so a
  project's own hook that happens to live under `target/debug` is left alone
  (#7262).
