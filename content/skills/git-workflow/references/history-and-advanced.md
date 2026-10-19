# git-workflow — conflicts, history, branches, tags and advanced operations

## Resolving Conflicts

```bash
# When conflicts occur during merge/rebase
# 1. Check conflicted files
git status

# 2. Edit files to resolve conflicts
# Look for conflict markers:
<<<<<<< HEAD
Your changes
=======
Their changes
>>>>>>> branch-name

# 3. Mark as resolved
git add resolved_file.py

# 4. Continue operation
git rebase --continue  # or git merge --continue
```

## Viewing History

```bash
# Compact log
git log --oneline -10

# Graphical log
git log --graph --oneline --all

# Commits by author
git log --author="John Doe"

# Commits affecting specific file
git log -- path/to/file.py

# See changes in commit
git show abc123

# Compare branches
git diff main..feature-branch
```

## Branch Management

```bash
# List branches
git branch -a  # All branches (local + remote)

# Delete local branch
git branch -d feature-branch  # Safe delete (merged only)
git branch -D feature-branch  # Force delete

# Delete remote branch
git push origin --delete feature-branch

# Rename branch
git branch -m old-name new-name

# Track remote branch
git checkout --track origin/feature-branch
```

## Tags

```bash
# Create lightweight tag
git tag v1.0.0

# Create annotated tag (recommended)
git tag -a v1.0.0 -m "Release version 1.0.0"

# Push tags to remote
git push origin v1.0.0
git push origin --tags  # Push all tags

# Checkout tag
git checkout v1.0.0

# Delete tag
git tag -d v1.0.0
git push origin --delete v1.0.0
```

## Advanced Operations

### Interactive Rebase

```bash
# Edit last 3 commits
git rebase -i HEAD~3

# Options in editor:
# pick = use commit
# reword = change commit message
# edit = stop to amend commit
# squash = combine with previous commit
# fixup = like squash but discard message
# drop = remove commit
```

### Multi-Commit Rebase — Conflicts Across Several Commits

A rebase that must resolve conflicts on more than one commit fails a second
time when the first resolution changes what the later commits expect (#7231):

- **Resolve commit 1 without relocating content.** Moving a block to a new
  location or deleting what looks like a duplicate removes the context a
  later commit's hunk is anchored to — its diff then has nothing to match
  against and conflicts again for a reason unrelated to the original
  conflict. Fix the conflict in place; save relocation for a follow-up
  commit after the rebase lands.
- **Run the cheapest module-loading gate before every `git rebase
  --continue`** — an import check, `cargo check`, a syntax parse — not the
  full test suite. A tree that cannot parse or load fails silently through
  `--continue` and only surfaces at the end, several commits later, with no
  indication which commit broke it.
- **A later commit can supply a fix an earlier one needs.** If an
  intermediate tree fails to load because a later commit imports new files
  or renames an export the earlier commit still references, pull that
  later commit's fix forward into the current commit's resolution rather
  than leaving the intermediate tree broken.
- **Tidy after, not during.** Once every commit lands, a follow-up pass can
  relocate content or drop the duplicate the first bullet deferred — as its
  own commit, with full conflict context already resolved.

### Bisect (Find Bug Introduction)

```bash
# Start bisect
git bisect start
git bisect bad  # Current version has bug
git bisect good v1.0.0  # This version was good

# Git checks out middle commit
# Test if bug exists
git bisect bad  # if bug exists
git bisect good  # if bug doesn't exist

# Git narrows down until finding first bad commit
git bisect reset  # Return to original branch
```

### Blame (Find Who Changed Line)

```bash
# See who last modified each line
git blame file.py

# Ignore whitespace changes
git blame -w file.py

# Show specific line range
git blame -L 10,20 file.py
```

## Git Hooks

```bash
# Pre-commit hook (runs before commit)
# .git/hooks/pre-commit
#!/bin/bash
npm run lint
npm test

# Pre-push hook (runs before push)
# .git/hooks/pre-push
#!/bin/bash
npm run test:integration
```
