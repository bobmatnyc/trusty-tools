Breaking
- `FactSlot` gains a public `verify` field, the command that re-reads the slot's fact (#9390). Code that builds a `FactSlot` with a struct literal must set it (`None` for a slot with no re-read command).
