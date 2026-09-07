Added
- The embedded dashboard (`ui-dist/`, mirrored from
  `crates/trusty-console/ui-search`) carries a stale-registration panel at
  `#/indexes/cleanup`. It lists this daemon's own `GET /registry/orphans` census
  and prunes a confirmed batch through `DELETE /indexes/{id}`, pinning each
  delete to the root the census reported so a path wiped and recreated in between
  is refused rather than deleted. No daemon-side change: the panel moved out of
  trusty-console (#6923, DOC-73 §13) onto the API this crate already served
  ([#6941](https://github.com/bobmatnyc/trusty-tools/issues/6941)).
