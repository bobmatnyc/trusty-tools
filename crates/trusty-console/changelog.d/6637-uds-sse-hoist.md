Changed
- The crate-private `uds_sse` module moved to `trusty_common::uds::sse`; the
  search and memory bridges import it from there. No behaviour change — the
  `data:` encoding, the 20-second keep-alive, the cancel-safe reader task and
  the terminal error event are the same bytes on the wire
  ([#6637](https://github.com/bobmatnyc/trusty-tools/issues/6637)).
