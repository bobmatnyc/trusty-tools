Changed

- The embedded `ticketing` and `BASE-AGENT` assets track their trusty-mpm originals byte-for-byte after the #5202 workflow/ticketing consolidation ([#5202](https://github.com/bobmatnyc/trusty-tools/issues/5202)). `ticketing` now owns the Issue end to end and no Pull Request operation at all — including the PR title and body, which previously read as its "bookkeeping" — and its deduplication step produces one of four named dispositions (`COMMENT`, `REOPEN`, `NEW REGRESSION`, `NO TICKET`) instead of reopening unconditionally on any recurrence.
