---
name: git-workflow
description: "Essential Git patterns for effective version control, eliminating redundant Git guidance per agent."
user-invocable: false
version: "1.0.0"
category: agent-reference
effort: low
---
# Git Workflow

Essential Git patterns for effective version control. Eliminates ~120-150 lines of redundant Git guidance per agent.

## Commit Best Practices

### Conventional Commits Format

```
<type>(<scope>): <subject>

<body>

<footer>
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation only
- `refactor`: Code change that neither fixes bug nor adds feature
- `perf`: Performance improvement
- `test`: Adding or updating tests
- `chore`: Build process, dependencies, tooling

**Examples:**
```bash
feat(auth): add OAuth2 authentication

Implements OAuth2 flow using Google provider.
Includes token refresh and validation.

Closes #123

fix(api): handle null response in user endpoint

Previously crashed when user not found.
Now returns 404 with error message.

perf(db): optimize user query with index

Reduces query time from 500ms to 50ms.
```

### Atomic Commits

```bash
# Good: Each commit does one thing
git commit -m "feat: add user authentication"
git commit -m "test: add auth tests"
git commit -m "docs: update API docs for auth"

# Bad: Multiple unrelated changes
git commit -m "add auth, fix bugs, update docs"
```

## Branching Strategy

### Git Flow (Feature Branches)

```bash
# Create feature branch from main
git checkout main
git pull origin main
git checkout -b feature/user-authentication

# Work on feature with regular commits
git add src/auth.py
git commit -m "feat(auth): implement login endpoint"

# Keep branch updated with main
git checkout main
git pull origin main
git checkout feature/user-authentication
git rebase main  # Or: git merge main

# Push and create PR
git push -u origin feature/user-authentication
```

### Trunk-Based Development

```bash
# Work directly on main with short-lived branches
git checkout main
git pull origin main
git checkout -b fix/null-pointer
# Make small change
git commit -m "fix: handle null in user query"
git push origin fix/null-pointer
# Merge immediately via PR
```

## Common Workflows

### Updating Branch with Latest Changes

```bash
# Option 1: Rebase (cleaner history)
git checkout feature-branch
git fetch origin
git rebase origin/main

# Resolve conflicts if any
git add resolved_file.py
git rebase --continue

# Option 2: Merge (preserves history)
git checkout feature-branch
git merge origin/main
```

### Undoing Changes

```bash
# Undo last commit (keep changes)
git reset --soft HEAD~1

# Undo last commit (discard changes)
git reset --hard HEAD~1

# Undo changes to specific file
git checkout -- file.py

# Revert a commit (creates new commit)
git revert abc123

# Amend last commit
git add forgotten_file.py
git commit --amend --no-edit
```

### Getting a Temporary Clean Tree

When you need a clean tree to run one command — a baseline check, a bisect, a
build against `origin/main` — add a throwaway worktree instead of moving the
work already in your tree:

```bash
git worktree add .claude/worktrees/baseline-$$ origin/main
# … run the check there …
git worktree remove .claude/worktrees/baseline-$$
```

Your own tree keeps its changes throughout, so a command that dies partway
through leaves nothing to restore.

Under trusty-mpm orchestration this is the PM's pattern, not a dispatched
agent's: worktree removal is PM-executed via `tm session prune-worktrees`, and
`tm hook --pm-guard` denies an agent's `git worktree remove` (#5791). An agent
that needs a clean tree asks the PM for one.

### Stashing Work

The stash stack is repo-global, not per-worktree: every worktree of a repo
shares one stack. Name each entry and restore it by ref, so you get back the
one you saved:

```bash
# Save current work under a name you can recognize
git stash push -u -m "WIP: authentication feature $(date +%s)"

# List first — confirm which ref is yours
git stash list

# Apply the ref you confirmed, not "the most recent"
git stash apply 'stash@{0}'
```

Check that the restored files are the ones you saved before dropping the entry.

### Cherry-Picking Commits

```bash
# Apply specific commit from another branch
git cherry-pick abc123

# Cherry-pick multiple commits
git cherry-pick abc123 def456

# Cherry-pick without committing
git cherry-pick -n abc123
```

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

## Best Practices

### ✅ DO

- Commit frequently with atomic changes
- Write clear, descriptive commit messages
- Pull before push to avoid conflicts
- Review changes before committing (`git diff --staged`)
- Use branches for features and fixes
- Keep commits small and focused

### ❌ DON'T

- Commit sensitive data (use `.gitignore`)
- Commit generated files (build artifacts, `node_modules`)
- Force push to shared branches (`git push --force`)
- Commit work-in-progress to main
- Include multiple unrelated changes in one commit
- Rewrite public history

## .gitignore Patterns

```gitignore
# Dependencies
node_modules/
venv/
__pycache__/

# Build outputs
dist/
build/
*.pyc
*.o
*.exe

# IDE
.vscode/
.idea/
*.swp

# Secrets
.env
*.key
*.pem
secrets.yml

# OS
.DS_Store
Thumbs.db

# Logs
*.log
logs/
```

## Quick Command Reference

```bash
# Status and diff
git status
git diff
git diff --staged

# Commit
git add .
git commit -m "message"
git push

# Branch
git branch
git checkout -b branch-name
git merge branch-name

# Update
git pull
git fetch

# Undo
git reset HEAD~1
git checkout -- file
git revert commit-hash

# History
git log
git log --oneline
git show commit-hash
```

## trusty-tools Deterministic Tools

The generic patterns above are for any project. On `trusty-tools` specifically,
run these deterministic checks yourself instead of reasoning about the
equivalent prose rule from memory:

| Step | Command | What a nonzero exit means |
|---|---|---|
| Opening every PR | `tm pr open --title <t> --body-file <path> [--issue N] [--rung 1-6] [--base main] [--docs-only]` | Exit 2 names the failed check (seven-field body, footer, changelog gate) and means `gh` was never called; `--dry-run` prints the argv instead |
| Before `gh pr create` | `bash scripts/check_changelog_fragment.sh` | Review-gate failure if crate `src/**` changed with no fragment; `tm pr open` runs this itself before spawning `gh`, so this is only for the hand-assembled fallback |
| Before `gh pr create` (a version was bumped) | `bash scripts/check-pr-version-bump.sh` | The version bump does not match what the PR's changes require |
| Before evaluating any required-context gate | `bash scripts/required-checks.sh [base]` (or `gh api repos/bobmatnyc/trusty-tools/branches/main/protection --jq '.required_status_checks.contexts'`) | N/A — always read live, never hand-copied (a stale copy cost PR #5836 a merge) |
| Pre-merge, to confirm queue ownership and status in one step | `tm pr queue-check [--base main] [<pr>]` | Exit 0 means every listed PR is clear; exit 1 names the first stop reason per PR (draft, hold label, `CHANGES_REQUESTED`, an unresolved `code-critic` BLOCK, or a missing/non-`SUCCESS` required context) — do not merge on nonzero |
| Pre-merge status read | `gh pr view <n> --json state,mergeable,statusCheckRollup` (one shot, never `--watch`) | `mergeable: false` or a red/pending required check means do not merge |
| Reporting a red gate | `bash scripts/is-branch-caused.sh <crate-dir> [--base origin/main]` | Prints PRE-EXISTING (exit 0), BRANCH-CAUSED (exit 1), or INCONCLUSIVE (exit 2) — report the verdict |
| After each PR's `state: MERGED` is confirmed | `tm session prune-worktrees --merged-prs --force` | A spared tree is reported with its reason — leave it |

## Remember

- **Commit often** - Small commits are easier to review and revert
- **Descriptive messages** - Future you will thank present you
- **Pull before push** - Stay synchronized with team
- **Use branches** - Keep main stable
- **Review before commit** - Check what is being committed
