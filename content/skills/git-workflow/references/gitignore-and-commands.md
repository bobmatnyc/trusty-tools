# git-workflow — .gitignore patterns and the command reference

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
