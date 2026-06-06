---
name: git-operations
description: Git log parsing, subprocess git calls, and repository analysis patterns
tags: [git, subprocess, log, parsing, commits, authors, blame]
---

# Git Operations

## Parsing git log output

```python
import subprocess

def get_git_log(repo_path=".", since=None):
    cmd = ["git", "-C", repo_path, "log",
           "--format=%H%n%an%n%ae%n%ad%n%s%n---",
           "--date=iso", "--numstat"]
    if since:
        cmd += [f"--since={since}"]
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=repo_path)
    return result.stdout

# Parse separator: use "---" as record delimiter
```

## Common patterns

- Use `--numstat` for insertions/deletions per file
- Use `--format=%H%n%an%n%ae%n%ad%n%s` for hash, author name/email, date, subject
- Handle empty repos: check `git rev-parse HEAD` exit code first
- Always pass `-C <path>` to avoid changing the current working directory
- For large repos, use `--max-count=<n>` to bound output

## Blame output

```bash
git blame --line-porcelain <file>
```

The `--line-porcelain` form is easier to parse programmatically than the
default human-readable blame; each line starts with a stable header token.
