Fixed

- The authorship report applies confirmed identity merges recorded in `authors.aliases` before computing bus factor and top-author share, so a merge an operator accepted no longer comes apart when a later collect re-observes the source email. (#6142)
- The authorship summary carries an `identity_merge_risk` flag when a high-confidence but unconfirmed alias suggestion touches a top-ranked author, naming the affected metrics, how many identities are involved, and `tga aliases suggest`. Suggestions are still never merged automatically. (#6142)
