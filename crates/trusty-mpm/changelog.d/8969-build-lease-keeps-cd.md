Fixed

- A heavy build after a `cd` no longer builds another checkout when Claude
  Code drops the `cd`. The `tm build-lease` rewrite now carries the
  directory into the lease. A `cd /abs/path && cargo …` chain with nothing
  after the build gets `--chdir <dir>`, so the build runs in the `cd`'s
  directory wherever the shell started. Any other `cd` that can be resolved,
  such as a relative one or a chain that continues after the build (`&& git
  commit`), gets `--expect-cwd <dir>`, and the lease refuses with exit 78
  when it does not stand there, so the rest of the chain stops too. A build
  after a `cd` the hook cannot resolve (`cd "$VAR"`, `cd -`, `cd ''`, a bare
  `pushd`) is refused with a message advising `tm build-lease --chdir
  <dir> -- <command>`. A quoted `"~/x"` is a literal directory, not the
  home directory.
