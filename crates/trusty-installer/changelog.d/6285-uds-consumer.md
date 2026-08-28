Changed

- `tctl`'s health probe now asks trusty-search for `search.health` on its Unix
  socket, the transport #6285 is moving the daemon onto, instead of reading
  `GET /health` alone. Both legs are read while trusty-search serves both: no
  published trusty-search binds a socket yet, so a socket-only probe would
  report every installed daemon as `Refused` — the verdict that arms `launchctl
  kickstart -k` — and hard-restart a healthy search mid-index-flush on every
  `tctl install`. `probe_http::dual_transport` marks that window, and its arm is
  deleted when the axum surface is (#6285)
