Changed

- `tm-pr-workflow`'s frontmatter description now names the per-PR changelog fragment requirement, so a relevance match can load the skill instead of only an explicit pointer reaching it ([#4968](https://github.com/bobmatnyc/trusty-tools/pull/4968))
  - the description previously covered only branch protection, the trusty-review gate, squash-merge, and worktree discipline, so an agent searching for the `changelog.d` fragment format never matched the one skill that documents it
  - `tm-capabilities`' generated skill catalog is regenerated to match
