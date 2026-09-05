Added

- The daemon's two hook-event SSE routes now have Unix-socket equivalents: `mpm.events.stream` for `GET /events` and `mpm.sessions.events_stream` for `GET /sessions/{id}/events`. Both are streaming JSON-RPC methods, so a caller sets `"stream": true` and reads one frame per event, each carrying the same JSON object the SSE `data:` line carries. A lagged subscriber loses frames and keeps streaming, matching the SSE routes exactly, and a client that disconnects releases the broadcast receiver its stream held. The HTTP routes are unchanged and still serve every one of these events (#6288).
