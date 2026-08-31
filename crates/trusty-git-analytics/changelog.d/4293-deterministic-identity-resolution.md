Fixed

- Identity resolution no longer varies between runs on identical input: canonical members are kept sorted by `(canonical_email, canonical_name)` instead of `HashMap` iteration order, and an exact fuzzy-score tie breaks on that same key rather than on arrival order (#4293).
- An alias claimed by two canonical identities now goes to the first claimant in a stable order and logs a warning, instead of silently going to whichever identity was written last. This applies to both constructors — sorted canonical-name order for a `developer_aliases` map, sorted `(email, name)` member order for a `team:` config, where member canonical emails and member aliases both still outrank a free-form `team.aliases` entry (#4293).
