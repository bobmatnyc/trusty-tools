Fixed

- `tm session decommission --force` no longer deletes an untracked `CLAUDE.md`,
  `.claude/settings.json` or `.claude/settings.json.bak` by path alone. It
  excuses one only while its bytes still match what the provisioning ledger
  recorded tm writing. Notes appended to `CLAUDE.md` after launch, a missing
  or unreadable ledger, or an unreadable file keep the worktree, and the
  refusal names the file and says nothing was deleted. The plain refusal no
  longer warns that `--force` discards these files. A workspace launched
  before the ledger existed (#8663) now needs its files checked by hand
  before removal.
- A `CLAUDE.md` or settings file that is a symlink, FIFO or other non-regular
  file is never excused by `--force` and is no longer opened, so a FIFO can
  no longer hang decommission.
- Worktree removal no longer treats a gitignored `.claude/` file whose name
  merely starts with `settings.json` (for example
  `.claude/settings.json-notes.md`) as tm's own bookkeeping. Only
  `settings.json`, `settings.json.bak`, its lock file and its timestamped
  snapshots are excused; any other such file keeps the worktree.
