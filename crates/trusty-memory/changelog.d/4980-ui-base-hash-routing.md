Fixed

- **`apiBase()` returned a fragment-bearing string on a hash-routed load.** `computeBase()` in `ui/src/lib/base.js` ran its `$`-anchored strips against the raw `document.baseURI` rather than its pathname, so at `…/#/` the returned base still carried the fragment (closes [#4980](https://github.com/bobmatnyc/trusty-tools/issues/4980))
  - trusty-memory serves its SPA at the ROOT (`src/web/static_assets.rs`), not under `/ui/`, so the `ui/` strip is a no-op here and `apiUrl()` stayed correct — relative URL resolution discards the fragment. Nothing in the SPA calls `apiBase()` directly, so there was no user-visible failure in this crate
  - trusty-search and trusty-analyze mount under `/ui/` and were genuinely broken by the same code; the fix is applied identically here to honour the KEEP IN SYNC contract and to stay correct if memory ever moves to a `/ui/` mount
  - the strips now run against `new URL(document.baseURI).pathname`, re-joined to `origin`; the `window.__MEMORY_BASE__` override branch and the non-browser guard are unchanged
