Changed
- Each Search-tab index row opens that index's management view in the
  console-served search dashboard, at `/tools/search/#/indexes/<id>/config`.
  That is the route the dashboard actually has — it is hash-routed and serves no
  `/tools/search/indexes/<id>` path — so the roster is now a grid list whose row
  is the link, the way the Services list already works
  ([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)).
- The Search tab's second stat card is labelled "Indexes Degraded" rather than
  "Warm Boot Degraded", which named the field instead of what it measures: any
  index not fully serving, whether from a failed embed stage, a boot-time TCC or
  allowlist skip, a load timeout, or a registry smaller than it was. Both stat
  cards centre their text
  ([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)).
