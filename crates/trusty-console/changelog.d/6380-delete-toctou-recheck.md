Fixed

- Batch prune re-checks each registration against a fresh `search.registry.orphans` census immediately before deleting it, and pins the delete to the root path that census reported. An index id is derived from its root path, so a path wiped and recreated between the census an operator confirmed and the prune that acts on it named a live index under the same id. Every re-check failure — an unreachable daemon, a census that will not parse, an id the daemon no longer calls stale — refuses that id's delete and reports why (#6380).
