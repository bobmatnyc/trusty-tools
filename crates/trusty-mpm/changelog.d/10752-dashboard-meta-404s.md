Fixed
- The Architect dashboard no longer logs a 404 every few seconds on a fresh store: the store seed now creates `meta/settings` as `{updated}` when it is absent (an existing one is never overwritten), and the page no longer polls `meta/gh_track`, which no shipped writer produces, so its repo-totals line stays hidden ([#10752](https://github.com/bobmatnyc/trusty-tools/issues/10752))
