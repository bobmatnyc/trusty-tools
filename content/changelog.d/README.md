# content/changelog.d — per-PR content changelog fragments

Every PR that changes a file under `content/instructions/`, `content/agents/`
or `content/skills/` adds ONE file here (ADR-0064, #8388). The format is the
crate fragment format, checked by the same assembler:

    content/changelog.d/<issue-or-pr-number>-<short-slug>.md

    Breaking | Added | Fixed | Performance | Changed | Removed | Security |
    Documentation                                                  <- line 1

    - one bullet per change a content consumer would notice

ONE fragment carries ONE category, and the file sits directly in this
directory. Validate one before committing:

    bash scripts/check_changelog_fragment.sh --file content/changelog.d/<n>-<slug>.md

Preview, then roll up at content release time:

    bash scripts/assemble-changelog.sh content --stdout
    bash scripts/assemble-changelog.sh content <version>

The roll-up writes `content/CONTENT-CHANGELOG.md` and deletes the consumed
fragments. This README is a tracked placeholder and is never a fragment.
