Fixed

- Pinned the API server's loopback bind default with a regression test and
  collapsed the duplicated default into one place
  (follow-up to [#3329](https://github.com/bobmatnyc/trusty-tools/issues/3329)).
  The `0.0.0.0` bind itself was fixed in
  [#3341](https://github.com/bobmatnyc/trusty-tools/pull/3341); the suite
  covered only the *refusal* of a non-loopback bind without a token, so nothing
  would have caught the default itself being flipped back — verified by
  reintroducing `0.0.0.0` and watching the two new tests fail while all six
  pre-existing guard tests stayed green. `--bind` is now parsed into an
  `Option<IpAddr>` and resolved through the new `ApiConfig::with_bind`, so the
  raw-argv path in `runtime::startup` and the clap path in
  `runtime::mode_dispatch` can no longer drift apart on a security-relevant
  default.
- Corrected the `auth.rs` module and `auth_middleware` doc comments, which
  still stated as fact that the server binds `0.0.0.0` and that bearer auth is
  what keeps it off the LAN. Both have been false since #3341 — the bind is the
  control, and on the loopback default the auth middleware is not installed at
  all. This stale text was the evidence cited in the original report and caused
  the exposure to be re-investigated as if it were still live.
