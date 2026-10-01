Fixed

- `tm hook --pm-guard` now judges a script a Bash command runs by the
  script's body: `bash <file>`, `sh`/`zsh`/`source`, `python3 <file>`,
  `node`/`ruby`/`perl <file>`, and `./<file>` (shebang scripts); scripts a
  shell script calls are followed to the depth bound. A script fed on an
  interpreter's stdin is not judged. A body that reads a Keychain entry, prints a token or names a
  secret-bearing file is refused exactly as the same read written inline
  (#8879, owner rulings 263 and 268). The guard reads at most 256 KiB of a
  body and fails closed: a script it cannot read in full (permission-denied,
  a symlink, over the bound, not UTF-8 text), an interpreter's script named
  through a variable, substitution or glob, and a missing script that nothing
  else in the command names are all refused. A compiled executable is not
  inspected. The documented residuals: a script that an earlier stage of the
  same command writes, scripts a script runs or sources through a variable,
  and a secret file a body names only through a bracket or brace shape.
- The credential-print refusal now tells the caller to stop and report to
  the Architect, names the script-body check, and no longer suggests how to
  consume the value in another command.
