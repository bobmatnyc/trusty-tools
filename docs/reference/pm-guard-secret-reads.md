# pm-guard: script-body secret reads (#8879)

`tm hook --pm-guard` refuses a Bash command whose script reads a credential,
the same as the inline read. Code and full residual list:
`crates/trusty-mpm/src/bin/tm/commands/pm_guard_secret_script.rs` (module doc).

## What is judged

- A script operand of a known interpreter (`bash`, `sh`, `zsh`, `dash`, `ksh`,
  `source`, `.`, `python*`, `node`, `ruby`, `perl`).
- A script run directly by path, judged by its shebang.
- Nested script calls inside a shell script, up to `MAX_SCRIPT_DEPTH`.

Each case the guard cannot read or resolve in full denies (fail closed): a
read error, a symlink, a body over 256 KiB, a non-UTF-8 body, a script named
through a variable, substitution or glob, and a missing script nothing else in
the command names.

## Accepted residuals (owner ruling, 2026-10-01)

Owner ruling 373 (2026-10-01): accept these residuals and close #8879. Each
one is allowed.

- Stdin-fed scripts: `bash -s < probe.sh`, `python3 - < x.py`,
  `cat probe.sh | bash`.
- Scripts behind reserved words or inside command substitution:
  `{ bash x; }`, the bodies of `if`, `for`, `case` and `!`, and `x=$(bash x)`.
- `python3 -m <local module>`.
- Shells with a different grammar: fish, csh, tcsh.

Other residuals (a script an earlier stage writes, `xargs`/`make`/`find -exec`,
a body changed after the check, a compiled executable) are listed in the
module doc.

### Why these are accepted

An agent with shell access can always get around such a guard. The guard
raises the cost of a credential read; it is not a boundary. The real fix is
the secrets work in [DOC-74](../specs/DOC-74-secrets-integration.md) (§15.8,
language tiers, grants and the agent flag) and the planned console secrets
service, tracked by epic [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517),
which keeps the value out of the agent's reach.

## Known defects

Tracked in [#9037](https://github.com/bobmatnyc/trusty-tools/issues/9037):

- Every symlinked script is refused. This is a false positive for
  `node_modules/.bin/*` and Homebrew shims.
- The depth bound and nested unresolvable paths fail open: past
  `MAX_SCRIPT_DEPTH`, or when a nested script path cannot be resolved, the
  guard allows instead of denying.
