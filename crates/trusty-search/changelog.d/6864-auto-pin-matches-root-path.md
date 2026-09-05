Fixed
- `serve`'s working-directory auto-pin now scans the index listing it already
  fetched for an entry rooted at the working directory before refusing. A
  derived id served from another tree — or served by nobody — pins that entry
  instead of leaving the session UNPINNED, and the startup line names it
  (#6864).
