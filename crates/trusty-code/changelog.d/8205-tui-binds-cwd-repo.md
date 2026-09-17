Changed
- `tcode tui` with no `--project` now binds the repository enclosing the current directory (its git toplevel), or the directory itself outside a repository, instead of running projectless against a throwaway scratch root. `--projectless` opts back out; `--project <path>` still wins; `$HOME` and the filesystem root are never homed on. (#8205)
