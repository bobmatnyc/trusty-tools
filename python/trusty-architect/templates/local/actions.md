# Actions

Append-only log, newest last. One entry per pass or action, stamped with
`date -u`: the mode (targeted or full poll), what changed, the exact text
sent and where, the acknowledgement seen, and what is still pending.
