Changed
- Ledger ids are ephemeral: `State::allocate` and an explicit `add` id now refuse only a live id, so an archived id is issued again (the wrap order is unchanged, and a full space of live ids is still `NoFreeId`); `allocate` no longer takes an `archived` slice and the writer no longer reads `archive.jsonl` for ids ([#10235](https://github.com/bobmatnyc/trusty-tools/issues/10235))
