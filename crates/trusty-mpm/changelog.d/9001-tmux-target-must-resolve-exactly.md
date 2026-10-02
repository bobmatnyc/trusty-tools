Fixed

- `tm hook --pm-guard` refuses a tmux `send-keys`, `kill-*`, `respawn-*` or other
  pane-changing command whose written `-t`/`-s` target does not resolve exactly
  to an existing session, window or pane. tmux matches an unknown session name
  by prefix, so `-t nosuch:0` could reach a live `nosuch-…` session. A
  prefix-only name, a missing target, a target the shell expands, and a server
  the guard cannot list (no tmux, a query error, an unreadable socket, a
  listing that takes over 2 seconds) are refused, naming the target.
  `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS` do not lift this.
  Read verbs such as `capture-pane`, `has-session` and `list-panes` are not
  affected. A command with no `-t` is left to the Architect pane floor.
- The same floor refuses a tmux command it cannot read, naming the token or
  the reason: a `TMUX`/`TMUX_TMPDIR` change, `sudo`, `env -i`, `exec -c`, a
  relative `-S` socket, an option it does not know, a word the shell expands
  before `--` ends the options (`$F -t x`, `"$@"`), and a program name the
  shell expands (`$T`, `${T}ux`, `$a$b`) when the command names tmux or a
  deny verb, or when an argument in a verb position is a deny verb, its alias
  or a unique prefix of it. tmux behind `xargs` or `find -exec … +`,
  and a shell that reads program text on stdin (`bash <<<`, `| sh`) in a
  command naming tmux, are refused too.
- A tmux command after shell grammar is now found: `{ …; }`, `( … )`, `!`,
  `if`/`then`/`else`, `for`/`while`/`until … do`, `case` arms including
  `(pat)`, function bodies (`f() {`, `f(){`, `f (){`), `time { …; }`, and
  `coproc`. `kill-session -a -t X` now checks `X`. A pane-position word
  (`top`, `bottom-left`, …) is no exact target for a pane verb.
- Prose in a here-document data body (`cat <<'EOF' > notes.md`) and a program
  path such as `~/.cargo/bin/tm` are no longer refused by this floor.
- Not covered: a fully dynamic program word with only dynamic arguments and
  no tmux text (`$P "$A"`), tmux reached through a runner the guard does not
  unwrap (`watch`, `script`, `parallel`), a script file, or an interpreter
  (`python3 -c`).
