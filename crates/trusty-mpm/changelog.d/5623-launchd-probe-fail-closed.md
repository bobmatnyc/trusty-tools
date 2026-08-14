Fixed

- The launchd probe no longer reads an unreadable `~/Library/LaunchAgents` as
  "no launchd unit is registered"
  (closes [#5623](https://github.com/bobmatnyc/trusty-tools/issues/5623)).
  `daemon_launchd_label_in` mapped every `read_dir` error to the same answer as
  an empty directory, and `tm start`, `tm restart` and the MCP stdio bridge all
  read that answer as permission to spawn — creating the unsupervised orphan
  daemon of #2486/#4230. It now answers in three states, and both spawn sites
  and `tm doctor`'s `daemon_orphan` check treat "could not determine" as
  "launchd may own this" rather than as absence. A home with no `LaunchAgents`
  directory still resolves to "no unit registered" (ADR-0045 §3).
- The `tm-issues-prune` skill no longer presents a truncated backlog as the
  whole one. Its scan and prioritize passes hardcoded
  `gh issue list --limit 500` against a repo with 745 open issues; they now
  paginate to exhaustion and report retrieved-vs-total, so a short read
  announces itself.
