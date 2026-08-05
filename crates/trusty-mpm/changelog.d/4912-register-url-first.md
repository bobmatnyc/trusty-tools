Changed

- `tm register` now takes the URL first with an optional alias — `tm register <url> [alias]` (closes [#4912](https://github.com/bobmatnyc/trusty-tools/issues/4912))
  - with no alias, one is derived from the URL as hyphen-joined `owner-repo` (`https://github.com/bobmatnyc/trusty-tools` → `bobmatnyc-trusty-tools`); the hyphen is deliberate, since an alias becomes a path segment wherever it is consumed
  - the legacy `tm register <alias> <url>` order still works: whichever positional is URL-shaped is taken as the URL, so existing invocations are not silently reinterpreted
  - a URL with no owner segment falls back to the bare repo slug; a host-only URL errors with the explicit-alias escape hatch
  - a derived alias already bound to a different URL refuses without touching the registry
