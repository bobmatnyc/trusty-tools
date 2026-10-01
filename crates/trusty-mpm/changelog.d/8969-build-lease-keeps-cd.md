Fixed

- A heavy build after a `cd` no longer builds another checkout when Claude
  Code drops the `cd`. The `tm build-lease` rewrite now carries the
  directory into the lease. A pure `cd /abs/path && cargo …` chain gets
  `--chdir <dir>`, so the build runs in the `cd`'s directory wherever the
  shell started. Any other `cd` that can be resolved, such as a relative
  one, gets `--expect-cwd <dir>`, and the lease refuses with exit 78 when
  it does not stand there. A build after a `cd` the hook cannot resolve
  (`cd "$VAR"`, `cd -`, a bare `pushd`) is refused with a message naming
  the fix. It is never left to run without its directory.
