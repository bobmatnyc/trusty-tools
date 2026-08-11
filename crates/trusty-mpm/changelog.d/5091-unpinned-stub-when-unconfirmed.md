Fixed

- Session launch writes the UNPINNED `trusty-search` MCP stub when index
  registration was not confirmed, instead of pinning `serve --index <id>` to an
  index the daemon never created
  ([#5091](https://github.com/bobmatnyc/trusty-tools/issues/5091)). Previously
  every `search`/`grep` in such a session answered `404 unknown index` for the
  session's whole life while the `search` health check stayed green. The
  `search_index_pin` doctor check reports the unpinned stub as a warning naming
  the relaunch fix, and still fails on a pin that has since gone stale.
