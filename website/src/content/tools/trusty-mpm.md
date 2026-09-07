## One binary, one daemon, many sessions

Running one coding session is easy. Running five across three repositories,
remembering which worktree each belongs to and which are still waiting on you,
is the part that goes wrong. trusty-mpm is the process that keeps that straight.

Everything ships as a single binary installed under two names — `tm` and
`trusty-mpm` — with each surface behind a subcommand rather than a separate
package to install and version-match.

- `tm daemon` — the background service everything else talks to.
- `tm tui` — a terminal dashboard across every live session.
- `tm telegram` and `tm slack` — remote control from a phone when you are not at
  the terminal.
- `tm gui` — an optional desktop shell over the same daemon.
- `tm wait` — poll a condition rather than sleep on it: `--for run` until a
  process exits, `--for file` until a sentinel appears (optionally containing a
  string), `--for check` until a pull request's checks settle. It exits 0 when
  the condition holds and 75 while it is still pending, so an agent whose turn
  has a ceiling re-runs the identical command instead of losing the wait.

## Sessions you can walk away from

`tm launch` provisions a session with everything it needs already deployed —
instructions, agent roster, skills — and starts it. `tm ls` and `tm f` find one
again by name or by prefix; `tm attach` reconnects. The daemon holds the roster,
so closing a terminal does not lose the session behind it.

Projects are registered rather than inferred, which is what makes the rest work:
the same directory resolves to the same project every time, across sessions and
across restarts.

<!-- include: docs/trusty-mpm/statusline-savings.md -->

## Hooked into the session, not beside it

`tm hook` handles Claude Code lifecycle events — before a tool runs, after it
runs, and when a session stops — and relays them to the daemon. That event feed
is what makes the dashboards live rather than a periodic poll, and it is
readable directly with `tm events`.

An MCP server exposes the same orchestration surface to a session itself, so an
agent can enumerate sessions, delegate work, and read project state through tool
calls rather than by shelling out.

## Coming from claude-mpm

trusty-mpm is not a fork or a version of the Python `claude-mpm` — unrelated
codebases that share an idea. If you already run one, the
[migration guide](/claude-mpm-migration) covers what installs, what carries
over, and what genuinely behaves differently.

## When it goes wrong

`tm doctor` runs a full diagnostic of the stack. `tm validate` checks a
workspace's deployed agents, skills, and settings against the canonical roster,
and `tm repair` recovers from a deploy state that has drifted. `tm health`
reports daemon reachability and a fleet summary in one line.
