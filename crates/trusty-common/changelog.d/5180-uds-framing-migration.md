Changed

- **`embedder_client::uds` now sends and receives through `uds::rpc`, the
  shared framing entry point, instead of its own copy.** ADR-0034 §4 decided
  every UDS client routes through one module. #5169 landed the dial half
  (`connect_hardened`); the framing half did not, so `send_framed_request` had
  exactly one consumer while this client kept a private `write_all` +
  `BufReader::read_line` + `serde_json::from_str` sequence. The wire bytes are
  unchanged — `embed_batch_sends_one_newline_framed_jsonrpc_frame` asserts the
  raw request frame against a stub listener and fails if the framing drifts
  (#5180)
- **The client gained a wall-clock bound where it had none.** A daemon that
  accepted the connection and then wedged used to hold the caller open forever.
  Embed exchanges are now bounded at 600 s — far above anything that completes
  today. The point is that the wait is finite, not that it is short (#5180)
- `uds::rpc` grew three additions the migration needed:
  `send_framed_request_capped` (the response-size budget as a caller argument),
  `send_framed_notification` (one frame out, no reply — for a peer that never
  writes back), and `write_frame` / `encode_frame` (the write half alone, for
  streaming NDJSON senders). `send_framed_request` is unchanged and still
  defaults to `MAX_FRAME_BYTES` (#5180)
- The embedder client passes a 256 MiB response budget rather than the shared
  8 MiB default. An embed reply is bulk data — roughly 12 bytes per
  JSON-encoded `f32`, so ~9.5 KB per 768-dimension vector — and the dream-dedup
  pass embeds every drawer in a palace in a single batch, crossing 8 MiB at
  around 900 drawers. The shared default would have turned a working dream
  cycle into a hard failure (#5180)
