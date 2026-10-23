---
name: architect-drift-check
description: Architect skill. Load when comparing a project's git history with its recorded architecture baseline (modules, dependencies, ADRs, boundaries) - weekly for each active project, after a large merge, before an architectural review, when setting a baseline for a new project, or when the user asks. Classifies each change as intended or unintended and brings unintended drift to the user as a proposal. Read-only in the project repo. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, drift, architecture, git]
effort: medium
---

# architect-drift-check — Architecture Drift

<!-- rule:r-069 -->
**Drift tracking.** Keep an architecture baseline per project in
`local/projects/<name>.md`: a commit SHA plus the key modules, dependencies,
ADRs and boundaries. Compare git history since the baseline with read-only
commands. Drift that an ADR or a ruling covers is intended. Bring unintended
drift to the user as a proposal.

Every command here is read-only in the project repo: `rev-parse`,
`merge-base`, `log`, `diff`, `ls-tree`, `show`, `grep`. Never checkout,
switch, reset, stash, fetch, pull or commit there. If the local ref is stale
(`git -C <repo> ls-remote origin main` differs from `origin/main`), say so in
the result and ask the owning PM to fetch.

## Set a baseline

Record this section in `local/projects/<name>.md` (see
`local/projects/_template.md`):

```
## Architecture baseline
- Baseline: <sha> (<date>, <ref>); set by <review, ruling or provisioning>
- Modules: <top-level dirs, crates or packages, one line each with its role>
- Dependencies: <manifests; key direct dependencies with major versions>
- ADRs: <ADR ids and titles at the baseline; ADR folder path>
- Boundaries: <rules such as "api/ must not import db/", public API surface>
```

Build it from `git -C <repo> ls-tree --name-only <sha>`, the manifests at that
SHA (`git -C <repo> show <sha>:Cargo.toml`), and the ADR folder listing.

## Compare

With `B` the baseline SHA and `R` the ref (usually `origin/main`):

```sh
git -C <repo> merge-base --is-ancestor $B $R && echo in-history   # else: history rewritten, report it
git -C <repo> log --oneline --no-merges $B..$R | wc -l
git -C <repo> diff --stat $B $R | tail -1
git -C <repo> diff --dirstat=files,5 $B $R
diff <(git -C <repo> ls-tree --name-only $B) <(git -C <repo> ls-tree --name-only $R)
git -C <repo> diff --name-status $B $R -- '*Cargo.toml' '*package.json' '*pyproject.toml' '*go.mod' '*requirements*.txt'
git -C <repo> diff $B $R -- '*Cargo.toml' '*package.json' '*pyproject.toml' '*go.mod'
git -C <repo> diff --name-status $B $R -- <adr-folder>
git -C <repo> log --stat --date=short --format='%h %ad %s' $B..$R -- <boundary paths>
git -C <repo> grep -n '<forbidden import>' $R -- <path>
```

Look for new or removed top-level modules, crates or packages; added or
removed direct dependencies and major-version bumps; ADRs added, superseded
or deleted; and changes that cross a recorded boundary.

## Classify

- **Intended:** an ADR (at the baseline or added in range) or a recorded
  ruling (`local/projects/<name>.md`, the project's palace) covers it.
- **Unintended:** nothing covers it.

## Report

- Unintended drift goes to the user as a multiple-choice proposal,
  recommendation first: (A) the PM writes an ADR that ratifies it; (B) relay
  a fix to the PM; (C) offer an architectural review (`architect-review`);
  (D) accept it and move the baseline.
- When every change since the baseline is intended, move the baseline to `R`
  and record why. No user message is needed.
- Record the check (date, `R` SHA, counts, verdict) in
  `local/projects/<name>.md` and one line in `local/actions.md`.

## Related skills

- `architect-review`: the review a drift report can offer.
