Changed

- **`bm25_client` and `embedder_client::uds` now send and receive through
  `uds::rpc`, the shared framing entry point, instead of their own copies.**
  ADR-0034 §4 decided every UDS client routes through one module. #5169 landed
  the dial half (`connect_hardened`); the framing half did not, so
  `send_framed_request` had exactly one consumer while both of these kept a
  private `write_all` + `BufReader::read_line` + `serde_json::from_str`
  sequence. The wire bytes are unchanged — `search_sends_one_newline_framed_jsonrpc_frame`
  and `embed_batch_sends_one_newline_framed_jsonrpc_frame` assert the raw
  request frame against a stub listener and fail if the framing drifts (#5180)
- **Both clients gained a wall-clock bound where they had none.** A daemon that
  accepted the connection and then wedged used to hold the caller open forever;
  for `bm25_client` that also stalled every concurrent writer on the palace,
  because `memory_remember` calls `index` under the per-palace write mutex.
  BM25 exchanges are bounded at 60 s, embed exchanges at 600 s. Both are far
  above anything that completes today — the point is that the wait is now
  finite, not that it is short (#5180)
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
