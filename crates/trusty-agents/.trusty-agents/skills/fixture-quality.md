---
name: fixture-quality
description: Test fixtures must exactly match real command output to prevent parser bugs
tags: [testing, fixtures, quality, parsing, cli]
---

# Fixture Quality

## Why this matters

When a parser consumes external command output (git, docker, kubectl, ls, ps, etc.),
the test fixture is the *contract* the parser is written against. If the fixture does
not match real command output byte-for-byte, parser tests pass while the real-world
tool crashes on the first invocation.

This is exactly how the Level 2 bake-off (Git Log Analyzer) failed: the agent invented
a plausible-looking `git log` fixture format, wrote a parser against the invented
format, all tests passed, and the tool crashed on every real repository.

## The rule

**Test fixtures MUST exactly match real command output format. Never invent a fixture.**

## Good pattern: capture real output, use it verbatim

1. Decide on the exact command and format string your parser will invoke. For example:
   ```
   git log --format='%H%n%an%n%ae%n%ad%n%s%n---' --date=iso --numstat
   ```
2. Run that exact command once against a real repo.
3. Capture 3–5 real commits worth of output.
4. Save that output verbatim as your fixture file.
5. Write the parser against the fixture AND assert the parser invokes the command
   with the exact format string (so drift is caught).

Example fixture (real `git log --format='%H%n%an%n%ae%n%ad%n%s%n---' --numstat`):

```
8bedae8f2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f
Bob Matsuoka
bob@matsuoka.com
2026-04-22 10:15:32 -0700
feat: docs-agent + WriteFileTool + skip phase support (#82)
---
15	3	src/agents/docs.rs
42	0	src/tools/write_file.rs

fe394d2a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e
Bob Matsuoka
bob@matsuoka.com
2026-04-22 09:02:11 -0700
feat: dynamic skill loading — all agents discover and load skills at runtime (#81)
---
8	2	src/skills/mod.rs
```

Note the real-world details the parser must handle:
- Full 40-char SHAs, not abbreviated
- ISO dates with timezone offsets
- Commit messages that contain colons, parentheses, em-dashes, PR refs
- Numstat section with tab-separated `added\tdeleted\tpath`
- Blank line between commits
- Trailing newline at EOF (or not — check)

## Bad pattern: inventing a plausible-looking format

```
# FAKE — do not do this
commit abc123
Author: Test User <test@example.com>
Date: 2026-01-01

    Initial commit
```

Problems:
- `git log` default output uses `Author:` prefix but `--format=%H` does not
- Date format differs depending on `--date=` flag
- Numstat block is missing
- Real SHAs are 40 chars, not 6

A parser written against this fake fixture will break on real input.

## Checklist before committing a fixture

- [ ] Did I run the actual command to produce this fixture?
- [ ] Does the parser invoke the command with the SAME format string I captured?
- [ ] Are the SHAs / IDs / timestamps full-length and realistic?
- [ ] Do I have at least 3 entries, including edge cases (merge commits, renames,
      unicode in author names, multi-line messages)?
- [ ] Is there a test that asserts the exact command + args the parser will run?
- [ ] Did I include the trailing/leading whitespace the real command produces?

## Concrete reference: exact `git log` format used by trusty-agents-style parsers

When a task says "parse git log output", the parser almost always invokes:

```
git log --format='%H%n%an%n%ae%n%ad%n%s%n---' --numstat
```

This produces a very specific layout. Here is **exactly** what that command prints for
a multi-commit history — use this as the fixture template and verify your parser can
consume it without modification:

```
abc123def456abc123def456abc123def456abc123def456abc123def456abc12345
Alice Smith
alice@example.com
2024-01-15 10:30:00 +0000
Add user authentication module
---
15	2	src/auth/login.py
8	0	src/auth/models.py
3	1	tests/test_auth.py
abc234ef5678abc234ef5678abc234ef5678abc234ef5678abc234ef5678abc2345
Bob Jones
bob@example.com
2024-01-14 14:22:00 +0000
Fix password hashing bug
---
4	2	src/auth/login.py
```

### Structure per commit block

Each commit block is laid out in this exact order:

1. **Hash line** — full 40-char SHA (or longer if the repo uses SHA-256), on its own line
2. **Author name line** — `%an`
3. **Email line** — `%ae`
4. **Date line** — `%ad` (format depends on `--date=` flag; default is the committer's locale)
5. **Subject line** — `%s` (commit message first line)
6. **Separator line** — literally the three characters `---` on their own line
7. **Numstat lines** — one per changed file, format `<additions>\t<deletions>\t<filepath>`
   where `\t` is a **real TAB character (0x09)**, not spaces
8. **Blank line** before the next commit's hash (git inserts this between commits)

### Critical gotchas

- The separator is `---` produced by `%n---` in the format string. It is **NOT** a
  git diff separator, **NOT** `diff --git`, **NOT** a YAML frontmatter marker.
  It's just three literal dashes your format string asked git to print.
- Numstat columns are separated by **TAB characters**, not spaces. If your fixture
  uses spaces, your parser will either fail or silently produce wrong filenames
  (e.g., a path with spaces gets split).
- Binary files show `-\t-\t<path>` instead of numeric counts. If the parser uses
  `int(additions)`, it will crash on binary files unless you handle the `-` case.
- The **final commit** may or may not have a trailing blank line depending on git
  version — the parser must tolerate both.
- At least **3 commits** in the fixture is the minimum to exercise multi-record
  parsing (boundary between commit 1→2 and 2→3 catches off-by-one state-machine
  bugs that a 2-commit fixture misses).

### Verification checklist

Before declaring the fixture correct:

- [ ] Copy the fixture verbatim into your parser's test.
- [ ] Confirm the parser code invokes `git log` with the **same** `--format` string.
- [ ] Confirm numstat lines in the fixture use real `\t` characters (open in a hex
      view or `cat -A` — you should see `^I` between the numbers and path).
- [ ] Confirm the `---` separator appears on its own line, not glued to other text.
- [ ] Confirm at least 3 commits are present with varied file-change patterns
      (single file, multi-file, binary file, rename).

## For parsers of other external commands

Apply the same rule to:
- `docker ps` / `docker inspect` output
- `kubectl get` with `-o` format flags
- `ps`, `ls -la`, `df`, `du` output
- API responses (capture real JSON, don't hand-write it)
- Log files (capture from a real run, don't fabricate)

## Why "it looks close enough" is not enough

Real command output has subtle formatting quirks: trailing whitespace, locale-specific
date formatting, null bytes in `-z` modes, terminal color codes, platform line endings,
partial-line buffer flushes. A fabricated fixture will never reproduce these quirks,
so the parser develops blind spots that only appear in production.

**Capture real output. Use it verbatim. Write the parser against reality.**
