Fixed

- `tm hook --pm-guard` judges a symlinked script by the file it points to, so
  a `node_modules/.bin` or Homebrew shim to a clean script is no longer
  refused. A symlink loop still refuses.
- A script run inside another script is now resolved as strictly as one the
  command runs directly: a nested script named through a variable or a
  missing nested script refuses, and a chain of scripts past the depth bound
  refuses instead of allowing. A script that locates a helper through its own
  path (`$(dirname "${BASH_SOURCE[0]}")`, `$0`, a variable set once from
  them) has that helper judged.
