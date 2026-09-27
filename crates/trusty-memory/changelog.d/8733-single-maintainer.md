Fixed
- A writer (`serve --foreground`, `kg-rebuild`) runs dream and TTL-purge maintenance only while it holds its data root's maintenance lease; a second writer on the same root keeps serving reads and writes but deletes nothing, and a manual dream run there answers `Conflict`. The holder's pid is logged at warn on acquisition and kept in `maintenance.lock` (#8733).
