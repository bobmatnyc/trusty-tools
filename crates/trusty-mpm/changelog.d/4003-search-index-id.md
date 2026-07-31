Fixed

- `tm doctor`'s `search` check no longer hardcodes a literal expected index id
  (`"trusty-mpm"`, the crate name) — it now derives the expected id from the
  project itself, the same rule `core::session_launch` and trusty-search's own
  `detect_project` use, so a repo registered under a different index id (e.g.
  this repo's `trusty-tools`) no longer permanently warns "index missing"
  against a healthy, fully-indexed project
  ([#4003](https://github.com/bobmatnyc/trusty-tools/issues/4003)).
