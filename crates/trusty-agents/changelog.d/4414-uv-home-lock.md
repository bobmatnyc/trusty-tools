Fixed

- `python_skill`'s two `uv`-spawning tests no longer race the 161 statements in
  this crate that reassign `$HOME` process-wide (closes [#4414](https://github.com/bobmatnyc/trusty-tools/issues/4414); refs [#4407](https://github.com/bobmatnyc/trusty-tools/issues/4407), [#3451](https://github.com/bobmatnyc/trusty-tools/issues/3451)).
  `uv` resolves its cache from `$HOME` at spawn time; under parallel load a
  sibling test's `HOME=<tempdir>` was reaped mid-flight, leaving `uv` pointing at
  a deleted tree (`No such file or directory ... /.tmpHR50aj/.cache/uv/...`).
  Both tests now hold `test_env::HOME_LOCK` for their whole body — verified
  exhaustive first: all 161 HOME-mutating statements (116 `set_var` + 45
  `remove_var`, across 72 test fns) sit inside a function holding that lock, so
  nothing can move `$HOME` while the subprocess runs. Measured on a loaded
  machine: 3 failures in 5 full-suite runs before, 0 in 5 after.
