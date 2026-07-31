Fixed

- **The `memory_remember` secret scanner no longer false-positives on
  slash-separated issue/PR-number lists (issue #2800).** A checkpoint
  enumerating tickets as `#2763/#2774/#2780/#2782/#2790` was rejected as a
  "likely secret/credential token": the `/` separators set the base64-symbol
  flag and the digits set the digit flag, so the base64-blob branch of
  `looks_like_secret` fired on a token containing no letters at all. Observed
  live twice; agents worked around it by rewording or dropping detail, silently
  degrading session-checkpoint fidelity. `looks_like_secret` now allowlists
  tokens built only from `#`, `/`, and ASCII digits, alongside the existing
  git-SHA carve-out. The exemption is charset-scoped and strictly narrower than
  the SHA allowlist — a single alphabetic character, or a `+`, takes a token
  back onto the normal heuristic path, so every credential shape the module
  already blocked stays blocked.

- **`project_index_id` documentation corrected**
  ([#4269](https://github.com/bobmatnyc/trusty-tools/issues/4269), amended under
  [#4288](https://github.com/bobmatnyc/trusty-tools/issues/4288)). The module
  stated without qualification that `root` is immutable and recommended that the
  wiring/migration slice "reconcile on `root`". Four routine actions move it —
  `mv proj`, a GitHub repo rename, a repo transfer, and `git remote remove
  origin` — and reconciling on it would reintroduce the silent-orphan class the
  identity work exists to remove. The immutability claims are now qualified
  ("under ordinary git operations"), the four movers are documented, and the
  recommendation points at a git-maintained anchor instead. The CHANGELOG's
  hermeticity claim is likewise corrected: two callers CAN derive different ids
  when `HOME`/`GIT_CONFIG_GLOBAL` differ and the repo sets no local
  `user.email`. Documentation only — no code change.
