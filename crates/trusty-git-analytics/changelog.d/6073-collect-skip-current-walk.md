Added

- `tga collect` records the head SHA and ref name each completed full-history walk reached, per repository, in the extract database (schema v25). A later collect skips the walk when the repository is unchanged, walks only the new commits when the head advanced, and re-walks in full — naming the reason — when the recorded commit is no longer reachable. `--force` restores the unconditional full walk. (#6073)
