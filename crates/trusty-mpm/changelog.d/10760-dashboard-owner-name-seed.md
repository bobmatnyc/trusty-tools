Fixed
- Architect dashboard: `/api/user` treats a seeded `the owner` (or an empty) `meta/state.owner_name` as unset and re-reads `local/architect.json` on each request, so an `owner_name` set after `tm fleet init` is shown without a restart; a name stored explicitly in `meta/state` still wins (closes [#10760](https://github.com/bobmatnyc/trusty-tools/issues/10760))
