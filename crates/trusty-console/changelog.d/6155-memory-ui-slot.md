Added
- The trusty-memory dashboard is served from the console at `/tools/memory/`.
  Its Svelte source moved into this crate as `ui-memory/`, alongside the search
  dashboard's `ui-search/`, and build.rs builds it into the committed
  `ui-memory-dist/` bundle
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- `ANY /api/memory/{*path}` (and the deprecated `/proxy/memory/{*path}` alias)
  translate each path the dashboard calls into one `memory.*` JSON-RPC call on
  trusty-memory's Unix socket, including the `/sse` live feed, which becomes
  `memory.activity_stream` bridged back to Server-Sent Events. trusty-memory has
  had no HTTP surface since #6286, so this is the only way in
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The live-feed bridge writes its response head without waiting for a first
  event. `memory.activity_stream` sends no opener, so peeking that frame under
  the 60-second open budget held the head for up to a minute against a healthy
  daemon and the feed never connected; the peek now has its own two-second
  budget and its expiry commits to `200`
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
