Changed

- `RememberOptions` gains a `wing_id: Option<Uuid>` field (ADR-0027 T9, [#4809](https://github.com/bobmatnyc/trusty-tools/issues/4809))
  - `None` (the default) resolves the room in the palace's default wing, which is
    byte-identical to the previous behaviour — every in-tree call site constructs
    `RememberOptions` with `..Default::default()` and is unaffected
  - external callers that build the struct with an exhaustive literal will need to add
    the field; note this when cutting the next release of this crate
