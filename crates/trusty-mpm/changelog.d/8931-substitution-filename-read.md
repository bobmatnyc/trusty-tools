Fixed

- `tm hook --pm-guard` refuses a file read whose filename comes from a
  command substitution, such as `awk … $(ls -a | grep -E '^\.env\.local$')`
  or `` cat `ls | grep local` ``, when the guard cannot work out the name.
  A substitution with fixed output (`$(echo name)`, arithmetic, `$(pwd)`,
  `$(git rev-parse --show-toplevel)`) is judged on that output. A
  substitution outside a file operand, such as a commit message or a search
  pattern, is not affected.
- `tm hook --pm-guard` no longer reads a command substitution that opens
  with a subshell, such as `$( (cmd) )` or `$((cmd) )`, as arithmetic. The
  computed-filename rule and the credential-print rule now treat `$((` as
  arithmetic only when the shell does, so such a substitution is judged as
  the command it runs.
