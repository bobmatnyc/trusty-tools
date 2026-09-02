Removed
- `TRIPLES_BY_PREDICATE` index maintenance (#6652). No reader anywhere in the workspace consumed it, so every assert and retract was paying for an index nothing queried. An at-open, fail-open migration drops the table; the compaction reclaims its bytes.
