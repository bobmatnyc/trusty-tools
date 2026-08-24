Fixed

- The daemon's boot adoption sweep no longer adopts a tmux session named under
  the reserved test namespace `tm-xtest-` (#6116). A test that spawns a real
  tmux session leaks it whenever the test process dies without unwinding — a
  SIGKILL, a `cargo test` timeout, an aborted run — so PR #6125's kill-on-drop
  guard, which covers the panic path, cannot cover that one. Three sessions
  leaked that way on 2026-08-24, adoption turned each into a managed record, and
  the session picker grew three `(active)` ghosts that outlived the tmux
  sessions themselves. The refusal decides from the name alone, so it holds for
  every leak shape whether or not any test-side cleanup ran, and it also returns
  the leaked pane to `daemon::orphan_gc` — an untracked idle shell with no live
  child is killed there after two sweeps, whereas being adopted made it
  permanent.
- An ADOPTED reserved-namespace record is now tombstoned by boot
  reconciliation, whether its pane is live or gone, and no automatic path
  resumes one (#6116). A record adopted by a daemon build predating the refusal
  survives in the durable store, and leaving it alone had two outcomes: a gone
  pane left a `Stopped` picker row nothing could resume — the state that forced
  a manual `session_delete force` — while a LIVE one sustained a loop, since
  re-adopting it restored its orphan-GC immunity, the reaper then stamped it
  auto-resumable, the supervisor recreated the tmux session, and the next boot
  re-adopted it. The daemon log shows that cycle three times in one day. Both
  rules ask for an adopted provenance as well as a reserved name, so a session
  the daemon CREATED for a project named `xtest-…` keeps ordinary stop, resume
  and list behavior. An ordinary session's gone-tmux handling is unchanged:
  still `Stopped`, still resumable.
- The boot-reconcile summary log now counts refused and swept test sessions, so
  a boot whose only finding was one of those no longer prints nothing.
- The `#3873` dead-runtime fixture now mints its real tmux session inside that
  namespace, and the test-support guard sweeps reserved-namespace sessions older
  than 30 minutes once per test process, so the next run on a machine reaps what
  a hard-killed run left behind. Neither mechanism survives a SIGKILL on its
  own; the daemon-side refusal is what makes a leak harmless while it lasts.
