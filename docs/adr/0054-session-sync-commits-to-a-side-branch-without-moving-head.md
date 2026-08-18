# 0054. Build the session-sync commit with plumbing and land it on a side branch, never moving HEAD

- **Status:** Proposed
- **Date:** 2026-08-18
- **Scope:** Workspace-wide — the `/tm-session-commit` and `/tm-session-pull`
  write path (#5954), and its relationship to `tm hook --pm-guard`'s
  main-checkout rules
- **Reversibility Cost:** Low — the commits live on one branch that nothing
  merges from, so abandoning the approach means deleting a ref
- **Decision Drivers:** the owner's two-machine workflow and the sessions
  `tm-audit` / `tm-audit-01` that were invisible from the second machine;
  ADR-0048's HEAD-move hazard; ADR-0049's staged-set commit rule, written days
  earlier and not written with a recurring machine-generated commit in mind;
  the destination repository being public
- **Supersedes / Superseded by:** —

## Context

#5954 wants two commands that carry session state between machines over git.
The commit side has to write a file into the project repository from whatever
checkout the session is standing in, which on this workspace is usually the main
checkout, shared with every other session.

ADR-0044 permits writing documents and configuration there. ADR-0048 decision 4
denied `git commit` on the verb; ADR-0049 made that conditional, allowing a
commit whose staged set is entirely non-source and whose checkout has no other
live writer. Both of those rules exist for one hazard: a commit moves HEAD, and
a main checkout's HEAD is shared, so `f1da7bce` landed on a branch belonging to
a different workstream.

ADR-0049 is days old. It was written for a human-scale docs commit — an ADR, a
release note — reached through the `Bash` tool and therefore visible to
`tm hook --pm-guard`. A session-sync commit is a different animal: machine
generated, potentially large, potentially frequent, and issued by a `tm`
subcommand rather than by `git`, which means the guard never sees it. Riding
ADR-0049 as written would mean a recurring memory dump competing for the shared
HEAD and passing through a gate that is not actually in the path.

The `.gitignore` question is settled by exclusion. `**/.trusty-mpm/*` is ignored
at every depth, and the one re-include that used to live inside it was retired
after it made `tm doctor`'s `legacy_overrides` check fail repeatedly (#4286,
#4676). So the synced files cannot live under `.trusty-mpm/`, and they get their
own top-level directory, `.trusty-memories/`.

## Decision

1. **The sync commit never runs `git commit`.** It is assembled with plumbing:
   `git hash-object -w` for each blob, `git update-index` against a temporary
   `GIT_INDEX_FILE`, `git write-tree`, `git commit-tree`, and then
   `git push origin <sha>:refs/heads/<sync-branch>`. Local HEAD does not move,
   `.git/index` is not touched, and no file is written into the working tree.
   The hazard ADR-0048 and ADR-0049 exist for is a moving HEAD; this path has
   none, so it is outside the category those rules gate rather than a carve-out
   from them. It needs no exemption and asks for none.

2. **The sync branch is fixed at `trusty/session-sync`, and the tool refuses
   any other ref.** The branch is never checked out, never merged, and never
   carries a pull request. `main` never carries these files. Hard-coding the
   destination is what keeps decision 1's safety from depending on the caller
   passing the right argument.

3. **Every path in the built tree must start with `.trusty-memories/`, and a
   tree containing anything else is refused.** This is the mechanical
   replacement for ADR-0049's staged-set classification. A prefix constraint is
   strictly stronger than `is_source_code_path` here — it admits one directory
   rather than excluding one extension list — so the two lists ADR-0049 decision
   2 warns about never come into existence.

4. **The commit does not take ADR-0049 decision 3's live-writer check.** That
   check answers "is it safe to move this checkout's HEAD right now", and this
   path moves no HEAD. Two sessions committing concurrently write disjoint
   paths (decision 6) onto a branch neither has checked out; the only shared
   resource is the remote ref, and a losing race there is a non-fast-forward
   push that retries against the new tip.

5. **Content is screened for secrets before it becomes a git object, and a hit
   blocks the commit.** Not a warning, and not a follow-up. `memory_core::filter`
   runs at palace write time only, so a credential that predates the filter or
   slipped past its heuristics is already stored and the export path copies it
   out verbatim — #5902's own review recorded this. The destination is a public
   repository. The gate is what makes the owner's choice of that destination
   safe, so it is part of the write path rather than a check layered over it.

6. **The committed path is namespaced by machine as well as by session:**
   `.trusty-memories/sessions/<machine-id>/<session-id>/`. The no-conflict-by-
   construction property rests on two machines never writing the same path, and
   session ids alone do not deliver that. `tm` mints UUIDs, but
   `session_context_pause` accepts any stable string — the pause skill names
   `$TM_SESSION_ID`, a tmux `session:window`, "or any other stable value" — and
   the sessions that motivated #5954 are named `tm-audit` and `tm-audit-01`.
   Two machines can produce `main:2`, and they can both produce `tm-audit`. The
   machine segment restores the property by construction instead of by
   convention.

7. **The pull side reads objects and never checks the branch out.**
   `git fetch origin refs/heads/trusty/session-sync`, then `git cat-file` /
   `git show <sha>:<path>` to read blobs out of the fetched tree. Fetch is
   already permitted in a main checkout (ADR-0053). Materialised files land only
   under `.trusty-mpm/sessions/`, which `.gitignore` already excludes, so a pull
   changes nothing git tracks.

8. **`tm hook --pm-guard` cannot see this path, and the safety does not depend
   on it.** The PM invokes `tm session commit`; the guard classifies `git`
   segments, so no rule of ADR-0048 or ADR-0049 is consulted. That is stated
   here rather than discovered later. What makes the path safe is the shape of
   the operations in decisions 1–3, enforced inside the tool, where the tool's
   own tests can hold them. A guard rule would be the weaker of the two anyway:
   it inspects a command line, and these constraints are about which refs and
   which paths the tool will accept.

## Consequences

**Easier.** A session's state crosses machines without a server, without an
account, and without the membership model #1683 specifies — the repository
already answers who may push and who may read. Concurrent commits from any
number of sessions and machines cannot conflict, because decision 6 makes the
paths disjoint and decision 1 keeps them off any shared ref that a checkout
holds. A `git status` in the main checkout stays clean, which is the property
that would have failed had the files been materialised in the working tree
first: `.trusty-memories/` is never on `main`, so every such file would have
shown as untracked, to every session, forever.

**Harder.** The plumbing sequence in decision 1 is more code than
`git add && git commit`, and it is code that has to be right — a mistake in the
temporary-index handling is a mistake in someone's shared checkout. It is
covered by tests against real repositories rather than by inspection.

**A branch nobody looks at accumulates history.** Each commit supersedes the
previous snapshot for its session, so the tree stays small, but the commit
history grows without bound and nothing prunes it. Deleting and re-creating the
branch is safe by construction (nothing merges from it), which is the intended
remedy, and it is not automated.

**The secret gate will produce false positives.** The detector behind decision 5
has six recorded recurrences of exactly that (#1667, #2800, #4216, #4312, #4739,
#4898), and here a false positive blocks a commit rather than a memory write.
That is the correct direction to fail for a public destination, and the deny
names the offending record so the operator can fix the memory rather than
disable the gate.

**Decision 8 is a real reduction in guard coverage, accepted knowingly.** Any
future `tm` subcommand that writes to the repository inherits the same
invisibility, and nothing mechanical stops one from being added that does move
HEAD. The mitigation available today is that the guard's rules and this tool's
rules are both in `trusty-mpm`, so a reviewer looking at one can find the other.

**Decision 6 diverges from the layout #5954 was scoped with**, which namespaced
by session id alone. The verification that produced it is decision 6's own
argument, and the cost is one extra path segment.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and the ADR corpus on 2026-08-18:

- **ADR-0044 (Main-checkout write boundary and agent worktree ownership):**
  **Consistent.** Decision 1 permits writing documents and configuration in a
  main checkout. This path writes no file into the working tree at all, so it
  sits inside that boundary with room to spare, and the pull side writes only
  under the already-ignored `.trusty-mpm/`.
- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** **Consistent, no interaction.** Decision 10's HEAD-move rule
  covers `merge` and `rebase` (and `pull`, until ADR-0053). This path runs none
  of them and moves no local ref, so decision 10 is neither invoked nor
  weakened. Decision 6's remedy-in-every-deny requirement governs the deny
  messages this tool emits.
- **ADR-0049 (Docs commits are permitted in a main checkout):** **Consistent,
  and deliberately not relied on.** ADR-0049 decides which `git commit` calls a
  main checkout may run. This path issues none, so decisions 1, 3, 5, 8 and 9 of
  that ADR do not apply. Decision 2's warning against a second file-classifying
  list is honoured by decision 3 here, which constrains by directory prefix
  rather than by extension. No decision of ADR-0049 is amended or scoped.
- **ADR-0053 (Fetch and pull are permitted in a main checkout):** **Extends, in
  use.** Decision 7's `git fetch` is exactly what ADR-0053 permits; this ADR
  adds that the fetched ref is then read with `cat-file` rather than checked out.
- **ADR-0045 (Distinguish absent from undeterminable on destructive paths):**
  **Consistent, and applied.** A sync branch that does not exist yet and a
  remote that could not be reached are different answers and get different
  handling — the first commits with no parent, the second fails.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees/`):**
  **Consistent, no interaction.** This path provisions no worktree and needs
  none, which is the point of decision 1.
- **ADR-0037 (PM placement precedence, main checkout by default):**
  **Consistent, and served.** A session sitting in the main checkout by default
  is the case this ADR makes workable.
- **ADR-0027 (Room identity as UUIDv5 over wing and label):** **Consistent, and
  depended on.** It is why #5902's records carry the room label rather than the
  room id, which is what lets a pulled memory land in the right room on a palace
  that has never seen it.
- **ADR-0051 (Palace id stays hyphen-joined; owner and project fields added):**
  **Consistent, no interaction.** Which palace a machine syncs is resolved
  locally on each side; nothing about the palace id crosses the branch.

No Accepted decision contradicts this proposal.
