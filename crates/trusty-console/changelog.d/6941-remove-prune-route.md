Removed
- `POST /api/console/search/prune-indexes` and
  `POST /api/console/search/deregister-unjudged`, with `routes::census_guard`
  and the `ActionVerdict::reason` / `::id` accessors that existed only for the
  batch's per-id rows. #6923 left both routes serving with no caller; the search
  dashboard now carries that panel and calls trusty-search's own
  `GET /registry/orphans` and `DELETE /indexes/{id}` directly, so a console
  management POST would be a second path to the same work — and console is
  display-only (DOC-73 §13). `crates/trusty-console/ui/src/cleanupFlow.js` keeps
  only the palace-compact half the Memory tab still uses
  ([#6941](https://github.com/bobmatnyc/trusty-tools/issues/6941)).
