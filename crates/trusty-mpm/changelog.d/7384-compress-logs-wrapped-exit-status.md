Fixed

- `tm compress`'s stats line now carries the wrapped command's exit status as
  `exit=N`, so an empty result no longer reads the same whether the command
  found nothing, failed, or had its stdout redirected away. The `tm hook`
  PreToolUse rewrite wraps the command in a brace group whose trailing `printf`
  reports `$?`, and the filter strips that sentinel before compressing, so it
  never reaches the caller's output. A signal-killed command reports the
  shell's `128 + signal`; an unwrapped `tm compress < file` reports
  `exit=unknown`.
