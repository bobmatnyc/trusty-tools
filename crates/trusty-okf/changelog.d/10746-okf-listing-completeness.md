Changed
- `plan_import` reads the adapter's `complete` flag from the last page only, stops listing when the run ceiling is reached, reports the cursor of a page cut at the ceiling so a resumed run lists it again, skips the fetch of an item whose listed revision matches its ledger record, and allows tombstones for a recursive docs folder listing as well as a document tree (#10746).
- A listing whose cursor repeats, or that still returns a cursor after `MAX_LIST_PAGES` pages, ends as incomplete instead of looping (#10746).
