Added

- A UDS RPC method can now answer in many frames instead of one, so a daemon
  migrating to `uds::server` keeps a token stream rather than buffering it
  (#6286). `RpcRouter::typed_stream` registers a handler that returns a
  `tokio::sync::mpsc::Receiver` of items — the shape `trusty-memory`'s chat
  handler already produces — and `uds::send_framed_stream_request` reads the
  reply frame by frame as a `FramedStream`, which yields items through
  `next_frame()` or adapts into a `futures_util::Stream`.
- Streaming is opt-in per request, through one optional `"stream": true` field
  on the existing JSON-RPC request frame. Without it the protocol is byte-for-byte
  what it was, so an old client against a new server, and a new client calling a
  method that does not stream, behave exactly as before.
- A stream terminates on a frame, never on EOF: exactly one `"stream":"end"` or
  `"stream":"error"` frame is written on every path, including a handler that
  fails mid-stream, one that fails to open, and an item too large for
  `RpcServeOptions::max_frame_bytes` (which now applies per frame). A client that
  reaches EOF without a terminal frame reports it rather than returning a
  truncated answer as a complete one.
- The two protocol mismatches fail immediately in the shape the caller reads,
  rather than hanging: a request without the flag against a streaming method gets
  one ordinary response frame carrying `CODE_STREAM_REQUIRED`, and a streaming
  request against anything that does not stream gets one terminal error frame
  carrying `CODE_STREAM_UNSUPPORTED`, naming the methods this listener does
  stream.
