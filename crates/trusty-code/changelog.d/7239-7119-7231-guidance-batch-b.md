Changed

- `code-review-standards` skill and the trusty-code `code-critic` fork add
  the enclosing function or method name as a required field beside every
  finding's file + line citation, anchoring a fix-round agent's `grep -n -A`
  instead of a whole-file read
  (refs [#7239](https://github.com/bobmatnyc/trusty-tools/issues/7239))
- `systematic-debugging` skill's Phase 1 adds a bypass-the-harness probe for
  client/server split debugging, and Phase 4 adds an exact-output assertion
  against the captured bad input before deploying a parsing/trimming fix
  (refs [#7119](https://github.com/bobmatnyc/trusty-tools/issues/7119))
- `skill-refs/git-workflow/SKILL.md` is resynced byte-identical to
  trusty-mpm's `git-workflow.md`, which gains multi-commit rebase guidance
  (refs [#7231](https://github.com/bobmatnyc/trusty-tools/issues/7231))
