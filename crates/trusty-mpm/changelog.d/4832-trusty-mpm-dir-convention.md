Changed

- `.trusty-mpm/` now belongs to a project's MAIN CHECKOUT, never to a worktree (closes [#4832](https://github.com/bobmatnyc/trusty-tools/issues/4832))
  - the compiled PM prompt moved to `<project>/.trusty-mpm/sessions/<session-id>/INSTRUCTIONS-COMPILED.md`; two concurrent MANAGED sessions in one project can no longer overwrite each other's. Launches with no session identity (in-place start, standalone load, `tm connect`) share a `sessions/local/` bucket and can still collide there — narrower than the single per-project file it replaces, not collision-free
  - the project `manifest.toml` override moved to `<project>/.trusty-mpm/framework/manifest.toml`
  - the `last-instructions.md` stash and `tm project init`'s scaffolding resolve against the same root, so a session launched from a worktree writes no `.trusty-mpm/` into it
  - the root is resolved from git: a linked worktree maps to its main checkout and trusty-mpm's own bare `<project>/.base` provisioning clone maps to the project, while a submodule, a `--separate-git-dir` checkout and an ordinary repository merely *named* `.base` each own their own state
- **Removed feature (owner ruling, not a bug fix):** the user-level `~/.trusty-mpm/manifest.toml` layer is gone. Instructions and their manifest are per-project and always deployed locally, so there is no user-level manifest surface. Move any home-level overrides into `<project>/.trusty-mpm/framework/manifest.toml`.
- migration is automatic and best-effort: every launch deletes a pre-existing `<dir>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md` (in the worktree and at the harness root) and removes the `framework/` directory only when it actually migrated a file and the directory is left empty, so neither an operator's `manifest.toml` nor a `tm project init` scaffold is disturbed
- a pre-existing `<project>/.trusty-mpm/manifest.toml` is still deliberately not moved for you, but a launch that finds one with no manifest at the new path now warns and names both paths, so an override cannot go dark silently

Fixed

- `tm session start` from a directory outside any git repository is now refused with an actionable error instead of scattering `.trusty-mpm/`, a `CLAUDE.md` stub and a `.claude/` tier into whatever directory the shell happened to be in (refs [#4832](https://github.com/bobmatnyc/trusty-tools/issues/4832))
- session preparation no longer reads the retired `<framework_root>/instructions/INSTRUCTIONS.md`. Its content had no consumer, but any read error other than `NotFound` — a permissions fault, a directory at that path, invalid UTF-8 — raised the one fatal `PrepError` variant and killed the launch
- the `.gitignore` block `tm` writes into a target project now covers `.trusty-mpm/sessions/`, `.trusty-mpm/logs/` and `.trusty-mpm/last-instructions.md`. `framework/` and `config.toml` are deliberately still trackable — they are operator-authored config

Removed

- `PipelineOutput::merged` and `PipelineOutput::instructions_loaded`, and `PipelineInput::framework_instructions_path`. All three existed only to serve the retired framework-instructions read and had no production consumer
