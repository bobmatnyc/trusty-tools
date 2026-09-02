Changed
- Bare `tm` in a directory that is not inside a git work tree now prints the
  managed session list — the same `tm sessions ls` code path, not a second
  renderer — followed by the two explicit ways to create a session:
  `[n] tm sessions start --dir <cwd>` for an untracked session that registers
  nothing, and `[m] tm sessions new <cwd> --task ''` for a managed, tracked one.
  On an interactive terminal it reads one line and runs the choice; piped or
  scripted it prints the listing and the options and exits 0; `q`, Enter, and
  EOF quit without creating anything. Previously such an invocation ran
  `git init` in that directory and carried on as though a project had been
  asked for (#6274/#6276) — that auto-init is unchanged for every directory git
  already knows, and a pane that belongs to a managed session still takes the
  in-place relaunch path. The advertised command and the executed one are built
  from one function and the argv is parsed back through the real CLI, so they
  cannot drift. `tm --help` states the behaviour (#6666).
