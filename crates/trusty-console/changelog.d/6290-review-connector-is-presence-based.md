Changed

- `ReviewConnector` reports trusty-review by presence instead of dialling its
  socket (#6290). The review daemon is retired, so the old dial spent its full
  3-second budget on every detection pass and arrived at the same `Available`
  verdict presence gives immediately.
- `Running` is now unreachable for this member, which is correct: a
  per-invocation tool is installed or it is not. The webhook path is untouched —
  console still spawns `trusty-review webhook-listen` per delivery and meters
  the drain off the inbox backlog.
