Fixed

- The reverse proxy's SSRF guard parses the upstream URL's host instead of
  prefix-matching the string (#6945). `is_local_upstream` tested
  `http://127.`, `http://[::1]`, and `http://localhost`, so
  `http://127.0.0.1.evil.com`, `http://localhost.evil.com`, and
  `http://[::1].evil.com` all read as loopback and the proxy would dial them.
  It now requires the `http://` scheme and hands the host to the shared
  `origin_is_loopback` classifier, which accepts only `localhost` or a host
  that parses as a loopback `IpAddr` (`127.0.0.0/8`, `::1`), and which refuses
  userinfo (`http://127.0.0.1:80@evil.com`) outright. Same class as #3319 on
  the inbound CSRF guard, on the outbound sibling that never got the fix.
