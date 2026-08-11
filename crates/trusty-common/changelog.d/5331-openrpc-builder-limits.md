Fixed

- `mcp::openrpc`'s module doc no longer claims it emits "the exact document
  already produced by `trusty-gworkspace`". It does not: the builder hardcodes
  the `x-scopes` extension name and has no `info.license` slot, so a server
  whose scopes are OAuth URLs (gworkspace) cannot use it without changing what
  clients discover. The doc now records both limits, so the next migration
  attempt reads them before starting rather than measuring them again. Also
  corrects two `cargo test -p trusty-mcp-core` references left over from before
  that crate was absorbed into `trusty-common` (#5331).
