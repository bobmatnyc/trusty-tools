Added
- The UDS RPC surface now serves `search.chat` (`POST /chat`) and
  `search.admin.stop` (`POST /admin/stop`), the last two routes with a named
  consumer — trusty-mpm's TUI stop key and the search UI's chat panel, which
  #6155 moves into `trusty-console`. Both run the same transport-free core their
  axum handler wraps, so the two doors answer identically (#6285).
