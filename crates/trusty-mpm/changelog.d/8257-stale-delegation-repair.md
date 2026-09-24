Fixed
- The shared-checkout dispatch deny now names each blocking delegation record — agent type, agent id (or delegation id when there is none), owning session, age — and the exact `tm repair delegation` command that clears it (#8257).
- `tm repair delegation --list [DIR]` lists the live delegation records for a directory, read-only, marking the ones that block a dispatch (#8257).
- `tm repair delegation --delegation-id <id>` reaches a record that never learned an agent id, and a record whose stop was matched by agent type, or which is past the 6 h stale threshold, is repairable while its session is still live (#8257).
- The repair now refuses while a live process stands in the agent's own tree, or while a harness lock's pid runs with the lock's recorded start time, and it refuses whenever one of those probes cannot answer (#8257).
