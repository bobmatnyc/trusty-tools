Fixed

- `tm hook --pm-guard` refuses a file read whose filename comes from a
  command substitution, such as `awk … $(ls -a | grep -E '^\.env\.local$')`
  or `` cat `ls | grep local` ``, when the guard cannot work out the name.
  A substitution with fixed output (`$(echo name)`, arithmetic, `$(pwd)`,
  `$(git rev-parse --show-toplevel)`) is judged on that output. A
  substitution outside a file operand, such as a commit message or a search
  pattern, is not affected.
