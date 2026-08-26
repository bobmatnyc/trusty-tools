Removed
- `walk_range_for` and the walk-range branch of `resolve_guard_ports`. trusty-memory was the only member that walked (`7070..=7079`) and it serves a socket now, so nothing produced a range. `decide_over_range` still iterates whatever `resolve_guard_ports` returns, which is how a future walker gets the #4470 relaxation back
