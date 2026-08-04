Fixed

- MCP and HTTP now agree on which room a `room` argument names. The MCP tools
  carried their own exact-case, alias-free parser while HTTP used
  `RoomType::parse`, so `room="backend"` stored into `Backend` over HTTP and
  into `Custom("backend")` over MCP — two different rooms with different ids,
  each invisible to the other's filter. There is now one parser
  (`RoomType::parse`), per the common-entry-point rule (ADR-0027 T3).
  - **Behaviour change:** a *new* MCP write with `room="backend"` now resolves
    to `Backend` rather than `Custom("backend")`, and custom room names are
    lower-cased the way HTTP has always lower-cased them. Existing drawers are
    untouched, so a palace can legitimately hold both a legacy
    `Custom("backend")` room and `Backend`; both are now enumerable, and room
    merging is designed but deferred (ADR-0027 D5). Where the legacy room was
    backfilled, the shared canonical key means the new write lands on the
    legacy room's id rather than creating a second one.
