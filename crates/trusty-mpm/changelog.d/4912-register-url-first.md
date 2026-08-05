Changed

- `tm register` now takes the URL first with an optional alias — `tm register <url> [alias]` (closes [#4912](https://github.com/bobmatnyc/trusty-tools/issues/4912))
  - with no alias, one is derived from the URL as hyphen-joined `owner-repo` (`https://github.com/bobmatnyc/trusty-tools` → `bobmatnyc-trusty-tools`); the hyphen is deliberate, since an alias becomes a path segment wherever it is consumed
  - the legacy `tm register <alias> <url>` order still works: whichever positional is URL-shaped is taken as the URL, so existing invocations are not reinterpreted
  - a URL with no owner segment falls back to the bare repo slug; a derived alias already bound to a different URL refuses without touching the registry
  - **behaviour change:** an argument that is not a repository URL is now rejected instead of being registered as one. `gh`-style `owner/repo` shorthand, a host with no path (`https://example.com`), a bare `myrepo.git`, and browser paths into a repo (`…/owner/repo/tree/main`, `…/pull/123`) all error with the full-URL form named. Each of these previously registered silently under a wrong alias — `owner/repo` derived the repo name alone, so it did not even collide with the later correct registration. A `?query` or `#fragment` is now stripped before the URL is stored.
  - `owner/repo` shorthand is rejected, **not** expanded to a GitHub URL — defaulting to GitHub is a separate product decision
