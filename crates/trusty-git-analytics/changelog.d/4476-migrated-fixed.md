Fixed

- Identity resolution no longer runs the Tier-3/4 Jaro-Winkler fuzzy fallback
  when a comprehensive `aliases_file` is configured (#4251). As the alias
  roster grew from 130 to 177 entries, the fuzzy tiers began collapsing
  distinct people onto similarly-spelled colleagues — `Cristian Dominguez` →
  `Crislaine Tripoli`, `Ravi Chandrasekaran` → `Ravi Pandey`, `Gauri Saykar` →
  `Gaurav Sharma`, `Josh Taylor` and `Joseph Ku` → `Joshua Lepage` — none of
  which had a declared alias explaining the match. An author that matches no
  declared alias is now reported under its own raw name instead of being
  guessed. Tier-1/2 exact alias resolution and the #2253 email-domain gate are
  unchanged.
- Identity resolver tests now use `tempfile::TempDir` instead of a hand-rolled
  `process::id() + SystemTime` directory name. The old scheme was not unique
  within a test binary — `process::id()` is constant across parallel tests and
  the `SystemTime` remainder is coarser than a nanosecond — so tests collided
  and each one's cleanup deleted a directory another was still using. Because
  `Config::resolved_aliases()` swallows alias-file load errors, the victim got
  a resolver with zero members, which returns every input unchanged and so
  satisfied the #4251 pass-through assertions *vacuously*. Measured at 10
  failures per 100 runs. The affected tests now also carry an explicit
  non-vacuity assertion that the roster loaded.

---
