Fixed

- a session in a worktree cut before `CLAUDE.md` was tracked no longer runs on fabricated instructions (closes [#5228](https://github.com/bobmatnyc/trusty-tools/issues/5228))
  - `instruction_pipeline::load_or_create_claude_md` read "file absent" as "new project" and wrote `CLAUDE_MD_STUB`. On a branch cut before #4660/#4661 tracked the real `CLAUDE.md`, that replaced every project instruction — and every `CLAUDE.md` named-section override — with boilerplate the session had no way to detect
  - the damage also persisted: once the stub was on disk, every later run found a file present and took the read path, so the fabrication was indistinguishable from an authored file
  - it now asks git whether an upstream ref (`@{upstream}`, then `origin/HEAD`, `origin/main`, `origin/master`) already tracks the file. When one does, no stub is written and the launch is refused — `prepare_session` maps the error onto `PrepError::Instructions`, the one condition #4752 rules must stop a session
  - the refusal names the branch, the ref that has the file, and two recovery commands (`git merge --ff-only <ref>`, or `git checkout <ref> -- CLAUDE.md`)
  - detection is local-only (no fetch) and fails open: no git, not a work tree, no upstream ref, or an upstream without the file all still seed the stub, so a genuinely new project is unaffected
  - `instructions_failure_message` no longer says "could not write" and no longer points only at permissions and free space — nothing is written on this path, and the refusal carries its own remedy
