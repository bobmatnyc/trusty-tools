Fixed

- **The dashboard misrouted every API call on a reload of a hash-routed URL (`/ui/#/`).** `computeBase()` in `ui/src/lib/base.js` ran its `$`-anchored `index.html` / `ui/` strips against the raw `document.baseURI`, which includes the URL fragment, so the `ui/` mount segment survived and API paths resolved under `/ui/` — onto the SPA catch-all, which answers `200 text/html` (closes [#4980](https://github.com/bobmatnyc/trusty-tools/issues/4980))
  - trusty-analyze mounts its SPA at `/ui/` (`src/service/routes.rs`) with the JSON API as siblings at the daemon root, so it had the identical defect to trusty-search rather than a merely theoretical one
  - the strips now run against `new URL(document.baseURI).pathname`, which carries neither fragment nor query, re-joined to `origin`. The `window.__ANALYZER_BASE__` override branch and the non-browser guard are unchanged
  - same fix as trusty-search, per the KEEP IN SYNC contract on this file; the committed `ui/dist/` bundle is regenerated, since CI and release set `SKIP_UI_BUILD=1` and ship whatever is committed
