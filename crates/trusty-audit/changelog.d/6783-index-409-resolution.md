Fixed

- A `409 Conflict` from `POST /indexes` no longer costs a repository its whole
  search-derived evidence tier. trusty-audit reads the daemon's registry, reuses
  the registration that already names this checkout, deregisters the stale row
  when one holds the id at another root or holds this tree under an obsolete id,
  and retries the create once. Deregistration never destroys the corpus and is
  guarded by the root it was decided on (#6783).
- Every arm that loses the search index now leads with one phrase —
  `evidence tier degraded: search index unavailable (<error>)` — and names the
  trusty-analyze pass that did not run with it, so a skipped analyze pass is the
  same headline rather than a separate silent gap (#6783).
- A run's `index.md` counts the repositories audited without search evidence and
  qualifies its coverage line, so an "M of M" run whose search tier was empty no
  longer reads as complete (#6783).
