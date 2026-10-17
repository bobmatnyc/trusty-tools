Added

- `tm doctor` gains an `unbound_delegations` row. It counts live delegation records of sessions with no bound `claude` that still wait for the age-based stale, because SessionEnd cannot stale them (#8980). Zero passes; a positive count warns and names `tm repair delegation`; a count the daemon cannot take, a daemon that predates it, or no daemon at all never reads as a pass (#9007).
