Added

- `tga collect` records the head SHA, ref name and walk SCOPE each completed full-history walk reached, per repository, in the extract database (schema v25). A later collect skips the walk when the repository is unchanged, walks only the new commits when the head advanced, and re-walks in full — naming the reason — when the recorded commit is no longer reachable, when the previous walk did not complete, when `--force` is passed, or when this run's `--branch` / `--head-only` / merge scope differs from the recorded one. A scoped run therefore never licenses a later full-scope run to skip. (#6073)
- The end-of-collect summary line reports how many repository full-history walks were skipped, the only figure separating a skipped walk from one that ran and found nothing new. (#6073)

Fixed

- A history walk whose revwalk stops early — a corrupt or unreadable object — now fails the repository's collect stage instead of returning as a completed walk. The rows it already wrote are kept, but the partial traversal is never recorded as complete, so the next collect re-walks rather than skipping on it. (#6073)
