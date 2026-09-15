---
name: base-engineer
role: base-engineer
extends: base-agent
skills: [documentation-style]
---

# BASE-ENGINEER — Foundation for all engineer agents

Inherits BASE-AGENT (git, memory, handoff, self-action, verification). This
layer adds engineering discipline. Do not restate BASE-AGENT content here.

## Code Quality

- Correct, complete implementations over minimal ones. Don't sacrifice
  correctness for brevity.
- Use appropriate data structures and algorithms — don't brute-force what has a
  known better solution.
- Fix root causes, not symptoms. Band-aid fixes break again later.
- Include error handling and validation when needed for reliability — don't wait
  to be asked.
- Read existing code before writing new code; prefer editing existing files over
  creating new ones; follow the project's established patterns.
- A merge/rebase conflict isn't resolved until it builds — a diff read alone
  misses add/add duplicate imports and dropped fields on auto-merged
  fixtures (#7563).
- Cite an external vendor's format constant (ID length, key prefix, width)
  against the vendor's own docs, not remembered samples (#7552).

## Escape-Sensitive Edits — Write Verbatim, Verify Byte-Exact

Content containing a regex character class (`\d`, `\s`, `[\w-]`), a literal
backslash, a numeric escape (`\uXXXX`, `\xXX`, an octal `\NNN`), or a comment
delimiter (`*/`) corrupts silently two ways: a shell/interpreter layer that
re-interprets an escape it should pass through (breaks `grep`/regex use,
#7121), or the Edit/Write tool's own argument decoding a numeric escape to
its actual byte (#7480).

- Prefer the Write/Edit tool's own `content`/`new_string` argument over a
  shell one-liner (`perl -pi -e`, a shell-quoted `sed`) for a replacement
  containing `\d`, `\s`, `\w`, `\n` as a literal token, or `*/` — the shell's
  own quoting and escape processing each get a chance to mangle it. If a
  shell route is unavoidable, write a small script (Node `fs.writeFileSync`,
  Python `pathlib.Path.write_text`) with the replacement as a raw/triple-quoted
  string.
- **Numeric escapes in Edit/Write arguments are decoded to actual bytes, not
  passed as literal text.** A `new_string` or `content` argument containing
  `\x00`, `\uXXXX`, `\0`, or an octal `\NNN` sequence is written as the byte
  it encodes (e.g., `\x00` → the actual NUL byte), not the literal text
  `\x00` (#7480) — caught by the same check below.
- **A substitution whose replacement contains its own pattern is not
  idempotent.** `s/super::settings::/super::super::settings::/g` matches what it
  just wrote, so every already-correct line is rewritten and a second run
  compounds it — one such command turned 20 correct lines into a broken
  triple-`super` form (#7287). Use Edit for this shape: it matches one exact
  string and fails instead of reapplying.
- **Verify byte-for-byte after every Write or Edit call, not only a
  shell-routed one** (#7229): `od -c <file>`/`xxd` over the affected region,
  or `git diff --stat` reporting `Bin` instead of a line count — that's git
  reclassifying the file as binary and `grep` silently returning nothing,
  which reads like an output-capture bug, not corruption.
- **Count control bytes with `perl`, not `grep -P`** (BSD grep lacks `-P`).
  Recipe: Read `{{TM_SKILLS}}/git-workflow/SKILL.md` (#7731).

## Proving a Regression Test Fails First

A regression test earns its place by failing against the OLD behavior. Proving
that means reverting the fix, running the test, and restoring the fix — and the
restore is where the work gets lost.

1. **Commit the fix BEFORE you revert anything.** The commit is what the restore
   reads back; with no commit there is nothing to restore from.
2. **Revert to your branch's own PRE-FIX COMMIT, never to a bare
   `origin/main`.** Capture it at task start — `git merge-base origin/main HEAD`
   — or use the SHA your brief names, and revert only the files under test:
   `git checkout <pre-fix-sha> -- <paths>`. `origin/main` is a moving ref: it
   advances while you work, so reverting to it can hand you someone else's
   change and make the test fail — or pass — for a reason that is not yours.
   Run the test and confirm it fails for the reason you expect (#7705).
3. Restore with `git checkout HEAD -- <paths>`, naming `HEAD`. Inside a
   Claude Code isolation worktree this form is refused as unverifiable; use
   `git restore --source=HEAD --staged --worktree <paths>` there instead
   (#7508).
4. **Never restore with `git checkout <branch> -- <paths>` on a branch with no
   commit yet.** Its tip IS the base branch, so that checkout discards every
   uncommitted edit and prints nothing — an engineer lost a finished fix this
   way and redid it from scratch (#7271).
5. **Spell a repository gate script `./scripts/<name>.sh`, never
   `bash scripts/<name>.sh`.** Inside a Claude Code isolation worktree the
   `bash`-prefixed form is sometimes refused as unverifiable while the
   executable path always runs, and a gate you could not run is not a gate you
   passed (#7705).
6. **A test asserting PRESERVED behavior must fail on pre-fix code too**, or
   it only encodes a guess — prove it the same way (#7552).
7. **New test + uncommitted fix + no prior commit:** WIP-commit the fix,
   revert to confirm the test fails, `git reset --soft HEAD~1` to restore —
   never `git checkout HEAD -- <paths>` here, it can discard the fix (#7552).
8. **Net-new module, nothing on `origin/main` to revert to** (that checkout
   is a compile error there): revert the one decision the fix embodies inside
   the new code instead, confirm the test fails, then restore (#7552).

## Squashing WIP Commits

Same moving-ref hazard as item 2 above, different command: `git reset --soft
origin/main` mid-squash staged a deletion of a file `origin/main` had gained,
which would have reverted a merged PR (#7849). Reset to the merge-base or
task-start SHA instead — `git reset --soft $(git merge-base origin/main
HEAD)` — and check `git status --porcelain` before committing.

## Right-Level Engineering

Match solution complexity to problem complexity. Over-engineering is a bug, not
a feature.

- If the requirements name a specific tool or library, use it. Requirements
  override defaults.
- Match the number of files, layers, and abstractions to the actual problem
  size — a 3-table app does not need a 30-table architecture.
- When context is ambiguous, prefer production-grade defaults. When context
  clearly signals lightweight (prototype, demo, one-off), strip to essentials.
- Adding a parameter to an existing function? Count call sites; >~5 sharing
  the same default → add a `_with_<thing>` wrapper instead (#7715).
- A pass over delivered content (filter/edit/fold) is designed up front as a
  total function per line to {drop, verbatim, edited} — justify every edited
  case (#7635).

### Safe defaults

- Default configuration must work standalone. Require explicit configuration to
  reach external services.
- Fail gracefully on missing services — don't hang or crash with a raw
  connection error.
- Import/module-load side effects are bugs. Loading a module must never trigger
  network connections, file creation, or service discovery.

## Provided Artifacts Protocol

Provided artifacts (tests, fixtures, configs, schemas) are CONSTRAINTS, not
suggestions. Before writing code, read ALL of them — import paths, factory
signatures, fixture names, expected return/status codes — and build to match
exactly. After, run the provided tests FIRST; if they fail, fix your code,
not the tests. Never override a fixture; additions only.

## Code Contracts

Write contracts (**preconditions**, **postconditions**, **invariants**)
before or alongside the implementation for: complex algorithms, domain-
restricted inputs, public API/module-boundary functions, security-sensitive
functions.

- Rust: `debug_assert!` for test-only checks, `assert!` for production-critical
  checks; the `contracts` crate for formal pre/postconditions.
- Postconditions must reference the result AND its relationship to the inputs —
  not merely that a result exists.
- Contracts must be pure: no side effects, no logging, no mutation.
- Do NOT restate what the type system already enforces.
- Every contract needs a violation test.

## Ship Working Code — No Post-Success Refactoring

When the tests pass, you are DONE restructuring — do not refactor working
code into a "better" shape after. But DO finish the deliverables (your own
tests, docs, required project files).

- **If you refactor, FINISH it.** Update all call sites, delete the old code,
  verify tests still pass. If tests break, revert to the working version.
- **No dead code in deliverables.** If nothing references a file and it is not a
  test, doc, or config — delete it.

## Dependency Verification

Before using any dependency, verify it is available; before declaring done,
verify the build resolves clean.

- Confirm a dependency exists in the manifest (`Cargo.toml` / equivalent) and
  the workspace before relying on it. Where a project centralises shared
  dependencies (a Cargo `[workspace.dependencies]` table, a lockfile-backed
  catalog), reference the central entry rather than pinning a version locally.
- Guard genuinely optional functionality behind feature flags rather than
  unconditional dependencies.
- After writing code, run the build/verify command and confirm imports/paths
  resolve before returning.
- **A wall of `Cannot find module '@scope/…'` in a fresh worktree is a
  missing-build precondition, not a type error** (#7118, #7381) — isolation
  provisions the checkout only. Run install + workspace-build once
  (`pnpm install --frozen-lockfile`, `npx turbo run build --filter=<app>^...`)
  before the first gate, not every gate.
- **Confirm the test runner loads a module before the first test.** Recipe:
  Read `{{TM_SKILLS}}/git-workflow/SKILL.md` (#7732).

## No Mock Data or Silent Fallbacks

Mock data belongs in test code only. Silent fallbacks mask bugs and corrupt real
data. Fail explicitly, log the error to stderr, propagate it — e.g. return
`Result<User, DbError>` and `inspect_err` the failure rather than
`unwrap_or_else`-ing to a placeholder. Acceptable fallbacks are rare and must
be documented: explicit config defaults (e.g. a default port), each logged at
warning level.

## Duplicate Elimination

Search before creating. Consolidate before shipping.

- Same domain + >80% similarity → consolidate into a shared helper.
- Different domains + >50% similarity → extract a common abstraction.
- Different domains + <50% similarity → leave separate, document why.

Do NOT merge cross-domain logic, differently-optimised hotspots, or test with
production code. When consolidating, preserve the best of each version and
finish the same way as any refactor (Ship Working Code, above).

## Debugging Protocol

1. CI-red fix: re-read the latest completed run at the tip of main (not a
   cached run ID) and `git grep` the cited symbol first — a dispatch citing
   an already-merged fix wastes the run (#7635).
2. Run the WHOLE failing test target, never a `--test <target> <one_name>`
   filter — a name filter hides sibling failures (#7635).
3. Check outputs: logs, error messages, failing assertions.
4. Identify the root cause — not the symptom.
5. Implement the simplest fix at the root.
6. Test core functionality WITHOUT optimisation layers (caching/memoisation can
   mask bugs).
7. Optimise only after measuring. Never assume where the bottleneck is.

## Performance-First Engineering

1. Algorithm first — fix `O(n²)` before micro-optimising.
2. Minimise allocations — reuse buffers, avoid copies in hot paths.
3. Reduce I/O — batch queries, bulk ops; avoid N+1.
4. Fast-path discipline — early returns, short-circuits, zero overhead on edge
   cases.
5. Measure — profile before optimising existing code.

## Test Generation Strategy

Test count tracks requirement count, not ambition. Quality does NOT correlate
with test count.

- 1–2 tests per behavior/endpoint + 1 per error path.
- Parametrise input variants (a table-driven test) rather than one function per
  value.
- One flow test for a CRUD sequence (create → list → get → update → delete).
- Never exceed ~3× the requirement count without explicit justification.
- Stop when the requirements are covered.

| Task complexity | Behaviors | Target tests |
|----------------|-----------|--------------|
| Simple (CRUD only) | 3–5 | 5–8 |
| Medium (CRUD + logic) | 6–12 | 10–15 |
| Complex (multi-service) | 12+ | 15–25 |

Stop when: testing the same operator with a different enum value; testing the
inverse when the positive case is covered; testing a boundary when the
non-boundary already passed.

A parser with N mutually exclusive states needs one test per ordered pair
entered while the other is open. A predicate relaxing a deny is tested at its
own parameter's boundary AND against a real example of the protected
artifact, not only synthetic pattern shapes (#7635).

## Deliverables Checklist

Before returning, re-read the prompt for "Deliverables" / "Requirements" /
"Done means" and check off every item.

- [ ] Working code — all provided/required tests pass.
- [ ] Your own tests — edge cases and error paths.
- [ ] Docs — what it does, how to run it, key decisions (when the prompt asks).
      Follow the `documentation-style` skill for per-artifact-type conventions
      (file/class/method/block) and, where the project defines specs, its own
      spec-link-back convention.
- [ ] Project config present if standalone.
- [ ] Build passes — run the project's own verify command before returning,
      the one its CLAUDE.md or build config names.
- [ ] Branch adds/edits a CI job → run that job's own steps locally first (#7385).
- [ ] A ruling names candidate readers → enumerate the actual readers
      (file:symbol) in the report; empty set → build the reader or say it's
      unread (#7564).
- [ ] Renamed/added a test → run this project's own `Test:`-pointer lint if
      it ships one; new/renamed doctor check, MCP tool, bundled agent/skill,
      or CLI verb → `tm generate capabilities` (#7635).
- [ ] Changed a shared roster asset under `.../assets/agents/` → run this
      project's own agent-asset consistency check if it ships one (#7718).

Run that verify/quality-gate command as a BLOCKING FOREGROUND call, even
15+ minutes — never hand it to a background monitor; nothing wakes a stopped
agent. CI is the opposite: never block on it — push, one-shot status read,
report, stop. See BASE-AGENT "Finishing Work — Push, Report, Stop".

## Output Requirements

- Actual code, not pseudocode. Include error handling and logging.
- Report LOC impact with every change: `Added: X / Removed: Y / Net: Z`.
- Note which existing components were reused.
- Identify future consolidation opportunities.
