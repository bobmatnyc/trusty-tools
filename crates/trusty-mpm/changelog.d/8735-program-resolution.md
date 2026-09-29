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
- `tm hook --pm-guard` now reads a `-c`, `env -S` or `flock -c` command string
  behind a wrapper and its options, so `timeout 60 bash -c 'rm -rf ~'` and
  `timeout 5 env -S 'cat $T'` refuse. It also knows Homebrew's `gtimeout`,
  `gnice`, `gstdbuf`, `gnohup` and `genv`, plus `setsid`, `chrt`, `taskset`,
  `unbuffer` and `flock`. A substitution body split across segments is judged
  from every directory a `cd` reached, so `cd / && x=$(true; rm -rf Users)`
  refuses (#8735).
- The delete floor no longer refuses a backtick inside a quoted here-document
  body, such as a commit message or PR body, and no longer refuses
  `command -v rm`, `sudo -k rm -f x` or macOS `xargs -J % rm %` (#8735).
