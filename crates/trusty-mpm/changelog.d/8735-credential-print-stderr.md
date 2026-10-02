Security

- `tm hook --pm-guard` now refuses a credential handed to `cat`, `printf` or
  zsh `print` when that program would repeat it in an error message on a
  visible stderr — `wc -c < <(cat "$T")`, `cat "$T" >/dev/null`,
  `printf '%d' "$T"` — inside `<(…)`, `>(…)`, `$(…)`, subshells and the
  wrappers the rule already follows, and through `2>&1` and `|&`. Stderr sent
  to `/dev/null` or a file stays allowed; a stderr target or `printf` format
  chosen at run time refuses. `echo`, `cat <(…)` (which names a `/dev/fd`
  path, not the value) and a `printf` format of text conversions only (`%s`,
  `%b`, `%q`, `%c`) are unchanged. zsh `print -u N` given a credential is
  routed to descriptor N, and refuses when N is chosen at run time or is the
  `-p` coprocess (#8735).
