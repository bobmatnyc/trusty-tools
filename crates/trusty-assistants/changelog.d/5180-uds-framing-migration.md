Changed

- **`MessageBus::send_to` and the ctrl socket's writes now go through
  `trusty_common::uds`'s shared framing entry point** instead of open-coding
  `serde_json::to_string` + `push('\n')` + `write_all` + `flush`. ADR-0034 §4
  put the framing in one module; these two were the `trusty-agents` half of the
  four sites that never migrated (#5180)
- The bus is a **notification**, not a request: the receiving bus re-broadcasts
  the envelope to in-process subscribers and never writes a reply, so it routes
  through the new `send_framed_notification` rather than `send_framed_request`,
  which would have blocked on a response that does not exist. `#5180`'s issue
  text listed it as a `send_framed_request` candidate; it is not one (#5180)
- `send_to` gained a 10 s bound. A peer whose socket receive buffer was full —
  an accept loop that stopped draining, a process stopped under a debugger —
  used to block the sender's `write_all` indefinitely, and `send_to` is called
  from request-handling paths (#5180)
- `ctrl/socket_listener` keeps its own **read** loops, and the module header now
  says why: the connection is multi-message (one command in, N `output`
  envelopes and a terminal `done`/`error` out) and deliberately lenient about a
  line that fails to parse. Neither property fits a one-shot request/response
  helper, and a streaming variant in the shared module would have no second
  consumer. Its writes still share the framing (#5180)
