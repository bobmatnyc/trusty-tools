Changed

- `tm register` now takes the repo first with an optional alias, and `owner/repo` is the primary form (closes [#4912](https://github.com/bobmatnyc/trusty-tools/issues/4912))
  - `tm register bobmatnyc/trusty-tools` registers `https://github.com/bobmatnyc/trusty-tools` under alias `bobmatnyc-trusty-tools`. GitHub is assumed, matching the `is_github_remote` gate `tm launch` already applies
  - a full URL is the alternative form and any host works there — `https://…`, `git@host:owner/repo.git`, `.git` suffix, trailing slash, ports. Non-GitHub *shorthand* is deferred, not refused on the merits
  - with no alias, one is derived as hyphen-joined `owner-repo`; the hyphen is deliberate, since an alias becomes a path segment wherever it is consumed
  - the legacy `tm register <alias> <url>` order still works: whichever positional names a repo is taken as the repo
  - a URL with no owner segment falls back to the bare repo slug; a derived alias already bound to a different URL refuses without touching the registry
  - **behaviour change:** arguments that name no repository are now refused instead of registered. A host with no path (`https://example.com`), browser paths into a repo (`…/tree/main`, `…/pull/123`), relative paths (`./repo`), and bare words are all errors naming the accepted forms. Each previously registered silently under a wrong alias. A `?query` or `#fragment` is stripped before the URL is stored
