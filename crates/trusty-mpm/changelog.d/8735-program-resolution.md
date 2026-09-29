Security

- `tm hook --pm-guard` now finds the program behind wrapper and precommand
  words and their options — `timeout 5`, `nice -n 5`, `noglob`, `nocorrect`,
  `sudo -u x`, `env -i PATH=/bin`, `time -p`, `stdbuf`, `ionice`,
  `caffeinate`, `xargs` — so `timeout 5 cat "$T"` or `noglob echo "$T"` given a
  credential refuses like the bare command. A wrapper option the guard does
  not know refuses once the command carries a credential or names a delete
  verb. The universal delete floor now judges each `$(…)`, backtick, `<(…)`
  and `>(…)` body as a command of its own, eight levels deep, so
  `echo "$(rm -rf /)"` refuses; `x=$(rm -f /tmp/scratch/file)` stays allowed
  (#8735).
