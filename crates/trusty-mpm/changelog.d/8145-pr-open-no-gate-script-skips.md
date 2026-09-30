Fixed
- `tm pr open --head <branch>` no longer refuses the changelog-fragment gate in a project that has no `scripts/check_changelog_fragment.sh`. The script's absence is now checked before the `--head` check, so such a project skips the gate for any head instead of being told to pass `--docs-only`. Where the script exists, a `--head` that is not the checkout's commit is still refused (#8145).
