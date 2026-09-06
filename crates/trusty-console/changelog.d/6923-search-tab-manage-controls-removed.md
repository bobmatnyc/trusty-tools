Removed
- The Search tab's per-row index delete and its stale-index cleanup panel, with
  the nested unjudged-registration review inside it. The console displays and
  the dashboard manages, so deleting an index is reachable only from
  `/tools/search`. The `/api/console/search/prune-indexes` and
  `deregister-unjudged` routes are untouched and keep working; nothing in the
  console calls them until the dashboard carries that panel
  ([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)).
