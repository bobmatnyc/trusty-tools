Fixed

- `origin_is_loopback` and `origin_matches_self` reject two authority shapes
  that reached a loopback verdict for a URL connecting somewhere else (#6945).
  The bracketed-IPv6 branch of the shared parser split on `]` and dropped
  whatever followed, so `http://[::1].evil.com` read as host `::1`; and neither
  branch handled userinfo, so `http://127.0.0.1:80@evil.com` and
  `http://[::1]:80@evil.com` read the part before the `@` as the host. The
  parser now returns `None` for any authority containing `@`, and for a
  bracketed literal trailed by anything that is not a `:port`. No well-formed
  `Origin` header or `http_addr` discovery file carries either shape.
