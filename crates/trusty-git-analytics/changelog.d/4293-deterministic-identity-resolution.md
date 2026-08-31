Fixed

- Identity resolution no longer varies between runs on identical input: canonical members are kept sorted by `(canonical_email, canonical_name)` instead of `HashMap` iteration order, and an exact fuzzy-score tie breaks on that same key rather than on arrival order (#4293).
- An alias claimed by two canonical identities now goes to the first claimant in sorted canonical-name order and logs a warning, instead of silently going to whichever identity was written last (#4293).
