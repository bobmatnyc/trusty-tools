Added

- **`claude_config::quarantine_path`** — computes a unique, timestamped
  quarantine name (`<path>.corrupt-<UTC stamp>`) for a corrupt config file
  ([#4206](https://github.com/bobmatnyc/trusty-tools/issues/4206)). Two
  independent trusty-mpm writers previously renamed a malformed `.claude.json`
  to the same fixed `.claude.json.corrupt`, so a second quarantine silently
  destroyed the first one's bytes. Purely additive: no existing behaviour
  changes.

- `json_rmw`: cross-process locked read-modify-write for whole-file JSON
  documents — the single implementation of the load → mutate → save critical
  section that `trusty-mpm`'s `projects.json`, `trusty-gworkspace`'s
  `tokens.json` (#3502) and the epic #4207 worktree registry all need.
  `json_rmw::update` takes an exclusive advisory lock on a `<path>.lock`
  sidecar, re-reads the document under that lock (never trusting a caller's
  stale copy), applies the mutation, and publishes atomically via a
  per-writer-unique temp file + `fsync` + `rename` + directory `fsync`. Never
  fails open: a failed lock, read, parse or write returns `Err` with the
  document byte-for-byte unchanged, and only a genuinely absent file starts
  from `Default`. Adds `fd-lock` as an unconditional dependency.

- **`project_index_id` — project-derived trusty-search index identity (#4207).**
  New `ProjectIdentity` (origin + root + operator) with a pure, deterministic
  `index_id()`, plus `derive_project_index_id()` and
  `resolve_operator_identity()`. Unlike
  the basename rule in `index_id` (which collides for unrelated checkouts sharing
  a directory name) and the session-worktree UUID (which binds service identity to
  ephemeral writer isolation), this id *partitions*: the canonical content-tree
  root is a hashed component, so sibling clones, linked worktrees, and differing
  accounts derive distinct ids by construction. Derivation only — nothing is wired
  into `ensure_project_indexed`, `trusty-search serve`, or the daemon's resolution
  path; registry reconciliation and migration of existing indexes are separate
  slices of #4207. No behaviour change for any existing caller.

  Derivation reads no environment variable of its own, but it is NOT fully
  hermetic (corrected, #4269): `resolve_operator_identity` shells out to `git
  config`, so two callers on one tree CAN derive different ids when `HOME` or
  `GIT_CONFIG_GLOBAL` differ and the repo sets no local `user.email` — see
  `project_index_id.rs`'s own note. The launchd daemon and a shell CLI are
  precisely that pair, since the daemon runs under a plist environment while CLI
  invocations inherit the shell's. Set a repo-local `user.email` to pin it. The
  `index_id()` docs enumerate exactly which inputs are mutable — `origin` moves on
  the first commit, on `git remote add origin`, and on a new root commit — each
  pinned by a test, so the migration slice inherits a true guarantee rather than an
  assumption of permanence.

---
