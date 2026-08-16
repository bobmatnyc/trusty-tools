# 0049. A documents-only commit is permitted in a main checkout, gated on what is staged

- **Status:** Accepted
- **Date:** 2026-08-16
- **Scope:** crate `trusty-mpm` — `tm hook --pm-guard`
  (`pm_guard_bash::main_checkout`'s commit rule, `core::staged_paths`), and
  `docs/specs/DOC-66-session-workstream-model.md` §0.5
- **Reversibility Cost:** Low — the change can only turn an existing deny into
  an allow, so reverting it restores ADR-0048's behaviour exactly and strands
  no data
- **Decision Drivers:** the owner's ruling that documents can be written AND
  committed in the main checkout; ADR-0048 part B's unconditional `git commit`
  deny, which left a document writable where it could never be landed; #5781's
  finding that DOC-66 §0.5 contradicts a shipped, enforced decision
- **Supersedes / Superseded by:** Amends
  [ADR-0048](0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md)
  decision 4 and scopes
  [ADR-0030](0030-sessions-own-many-workstreams-from-the-tm-checkout.md)'s
  §0.5 position as encoded in DOC-66. ADR-0044's write boundary and ADR-0048's
  remaining decisions stay in force unchanged.

## Context

ADR-0044 decision 1 says a main-checkout session may write documents and
configuration. ADR-0048 decision 4 says `git commit` there is denied, on the
verb, with no conditions. Both shipped, and together they produce a state
neither decision intended: an agent may write `docs/adr/0048-….md` in the
checkout it is standing in, and has no way to land it from there. The
`.md` file is writable and the commit that would make it permanent is not.

The owner has ruled that this must change: documents can be written **and
committed** in the main checkout.

The hazard that made ADR-0048 deny the verb has not gone away. A commit moves
HEAD, and a main checkout's HEAD is shared by every session standing in it. It
is the same hazard ADR-0048 decision 10 identified for `pull`, `merge` and
`rebase`, and the reported incident is a commit specifically: `f1da7bce` landed
on `fix/1646-drive-query-v2-migration`, a branch belonging to a different
workstream, and left the branch it belonged to empty at `cff5bbcd`. Nothing
about that failure depended on the file extension. An unconditional docs-commit
allow would reopen it.

Separately, #5781 recorded that DOC-66 §0.5 states the tm checkout "is not
read-only, and read-only-ness is not a property this model wants, needs, or
should try to enforce for it". DOC-66 encodes ADR-0030, which is still
Proposed, so this is a shipped, mechanically enforced decision disagreeing with
a proposed one — not stale prose that a doc sync could quietly fix.

## Decision

1. **The commit gate decides on what is STAGED, not on the verb.** The verb is
   `git commit` for a safe commit and an unsafe one alike; only the content
   separates them. A staged set of documents and configuration is permitted
   (subject to decision 3); a staged set containing any source file is denied.
2. **"Source" is the same question the write boundary already asks.**
   `is_source_code_path` and its `SOURCE_CODE_EXTENSIONS` list decide it — the
   list ADR-0048 decision 5 already uses for the edit-tool rule. A second list
   would drift from the first, and the two rules would then disagree about
   whether a file could be written but not committed, which is the exact
   incoherence this ADR exists to remove.
3. **A documents-only commit takes ADR-0048 decision 10's live-writer check.**
   Permitted when the daemon's directory-keyed `live_shared_tree_writers`
   reports nobody else writing in that checkout; denied when it names another
   writer. A docs commit moves HEAD exactly as far as a source commit does, and
   which branch it lands on is decided by where HEAD already points, not by
   what is in the commit. Treating it as unconditionally safe would answer the
   content question and skip the concurrency one. The owner's ruling settles
   whether a docs commit is categorically forbidden — it is not — and decision
   10 already established that "allowed in principle, unsafe right now" is
   answered by a concurrency test rather than a verb ban. The solo session,
   which is the overwhelmingly common case, sees no friction at all.
4. **`git add` is not gated, and needs no rule.** It writes the index and moves
   no ref. Two sessions staging in one checkout contend on `.git/index`, which
   git's own `index.lock` serialises, and neither one changes what branch the
   other's work sits on. The hazard every rule in this family exists for is a
   moving HEAD, and staging does not move one. Verified against the code rather
   than assumed: `is_whole_tree_destructive` has no `add` arm, the commit rule
   matches `commit` alone, `starts_a_head_move` covers `pull`/`merge`/`rebase`
   only, and `pm_guard_allows_read_only_and_near_miss_git_in_a_main_checkout`
   already pins `git add -A` as allowed.
5. **A staged set that does not describe the commit is refused.** Three arms,
   each a deny: the index could not be read at all; nothing is staged; or the
   command carries `-a`, `--amend`, `--include`, `--only`, or a bare pathspec,
   each of which commits content the index does not hold. Recognition is by
   ALLOWLIST of commit flags, so an unrecognised token — a pathspec, a short
   cluster, a flag a future git adds — also denies.
6. **A MIXED staged set is denied as a whole, and the deny names every staged
   source path.** A commit is one object; half of it cannot be sent elsewhere.
   Per ADR-0048 decision 6 the message carries the remedy, and here the remedy
   is mechanical rather than general: `git restore --staged` the named paths and
   commit the documents, or move the whole change to a worktree.
7. **DOC-66 §0.5's claim is scoped, not deleted.** After this ADR the tm
   checkout is neither read-only nor freely writable; it is **source-restricted**.
   §0.5's first half survives: the checkout is not read-only, sessions are meant
   to live and write there, and documents and configuration — which is most of
   what a session writes there — now also commit from there. §0.5's second half
   does not: read-only-ness is not what the model enforces, but a boundary IS
   enforced, mechanically, by `tm hook --pm-guard`, and a spec that says no such
   property "should be enforced for it" describes a checkout this workspace does
   not have. DOC-66 §0.5 is updated to state the source-restricted boundary and
   to cite ADR-0044/0048/0049 for it.

## Consequences

- **This change is one-way, and that is what keeps it off ordinary work.**
  Every reachable arm of the commit rule returned a deny before it; every arm
  that is not a positively evidenced documents-only commit returns one after.
  The classifier can turn a deny into an allow and cannot turn an allow into a
  deny, so it carries none of the false-deny risk #5356 was filed for. The
  allowlist in decision 5 is what buys that property: an unrecognised flag
  yields the pre-ADR-0049 behaviour rather than a new judgement.
- **The gate now runs a git subprocess, on a path that previously ran none.**
  `pm_guard_bash::main_checkout` documented itself as filesystem-lexical with no
  subprocess, and the staged set cannot be read any other way. It is read only
  after both lexical halves already match — a `git commit` segment whose target
  directory is a main checkout — so ordinary Bash traffic never pays for it, the
  same discipline decision 10 uses before its daemon call. It runs through
  `session_manager::worktree_safety::git_command`, whose environment stripping
  is not incidental here: an inherited `GIT_INDEX_FILE` would otherwise let the
  answer come from a different index than the one `git commit` is about to read.
- **`staged_paths` returns `Option`, and `None` must never collapse into
  empty.** `None` means the staged set is unknown and denies; `Some(vec![])`
  means nothing is staged and also denies, for a different reason the message
  names. Merging them would turn every unreadable repository into a permitted
  commit. This is [ADR-0045](0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)'s
  distinction applied to a gate rather than to a destructive path.
- **Decision 3 inherits decision 10's fail-open, deliberately.** An unreachable
  daemon, a malformed answer, or a directory recorded under a third spelling all
  answer "nobody here", and the docs commit proceeds. A session merely standing
  in the checkout with nothing dispatched is invisible to the query. Both are
  the #4480 guard's own direction, inherited with the query rather than chosen
  again — and the cost of a false allow here is one docs commit on a shared
  branch, against a false deny that would block the release-note and ADR work
  main-checkout sessions exist to do.
- **A source commit's message is longer and more specific than the one it
  replaces**, because it now names files. A staged set of a hundred source paths
  produces a hundred names. That is the correct failure for the reader, who has
  to unstage them, and it is bounded by what they staged.
- **The `--git-dir=`/`--work-tree=` residual ADR-0048 records for the HEAD-move
  rule applies here unchanged.** Only `cd` and `git -C` resolve a target
  directory, so `git --git-dir=/repo/.git --work-tree=/repo commit` run from
  outside resolves the running directory. It is the same stated gap, not a new
  one, and closing it is still a change to `git_verb_target_dir` that all three
  rules would inherit.
- **ADR-0030 is not superseded.** Only §0.5's read-only-ness claim, as DOC-66
  encodes it, is scoped. ADR-0030's session-to-workstream relationship, its
  single-home model, and everything else DOC-66 states are untouched and remain
  Proposed.

## Alternatives Considered

- **Allow a docs commit unconditionally, on the plain reading of the ruling.**
  Rejected. The ruling answers whether the content class is permitted, and
  ADR-0048 decision 10 had already separated that question from whether the
  moment is safe. An unconditional allow would let two sessions commit
  documents into one HEAD — the reported incident, with `.md` files in it.
- **Gate on the verb plus a `--docs` style operator flag.** Rejected: it asks
  the caller to assert what the guard can check, and an assertion the guard
  cannot verify is not a boundary. The index is the authoritative answer and it
  is already there.
- **Classify staged files with a new documents allowlist rather than the
  existing source list.** Rejected. Two lists for one boundary drift, and the
  first divergence would be a file the write boundary permits writing and this
  rule refuses committing — the incoherence this ADR removes, rebuilt one layer
  down.
- **Split a mixed staged set: commit the documents, leave the source staged.**
  Rejected. The guard is a `PreToolUse` hook that returns allow or deny; it
  cannot rewrite the index, and a rule that silently committed a subset of what
  the caller staged would be a worse surprise than a refusal.
- **Edit DOC-66 §0.5 to match the enforced boundary and leave the ADR corpus
  alone.** Rejected, and #5781 says why: DOC-66 encodes a Proposed decision that
  genuinely disagrees with a shipped one, so an edit would erase the
  disagreement rather than resolve it. The resolution belongs in an ADR, and the
  spec then follows it.

## Related Decisions

Vetted against the ADR corpus on 2026-08-16:

- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** **Amends.** Decision 4's `git commit` deny becomes conditional on
  the staged set. Decisions 1–3 and 5–9 are unchanged. Decision 10 is unchanged
  and is extended in scope by decision 3 here: its directory-keyed writer query
  now also decides a documents-only commit, through the same
  `live_shared_tree_writers_in` call and with the same two directory keys.
  Decision 6's remedy requirement governs all three new messages.
- **ADR-0044 (Main-checkout write boundary):** **Consistent, and completed.**
  Decision 1 permits writing documents and configuration in a main checkout;
  this ADR makes that permission reachable by giving it a commit path. Decision
  2's mechanical-enforcement requirement is unchanged — the boundary is still
  enforced, on narrower grounds. No decision of ADR-0044 is altered.
- **ADR-0030 (Sessions own many workstreams, Proposed):** **Scopes.** DOC-66
  §0.5's "read-only-ness is not a property this model wants, needs, or should
  try to enforce" is the one claim resolved, per decision 7. Everything else
  ADR-0030 proposes is untouched and stays Proposed.
- **ADR-0045 (Absent vs undeterminable on destructive paths):** **Consistent,
  and applied.** `staged_paths`' `Option` keeps "the index could not be read"
  distinct from "nothing is staged", which is that ADR's distinction reached for
  on a permission gate rather than a destructive operation.
- **ADR-0037 (PM placement precedence, main checkout by default):**
  **Consistent.** Nothing here changes where a session runs. This decision makes
  ADR-0037's default placement usable for the documentation work such a session
  is expected to do.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees/`):**
  **Consistent, no interaction.** Every deny still names a worktree as the
  remedy at ADR-0036's location; no new location is introduced.

No Accepted decision contradicts this amendment. ADR-0030 is Proposed and the
one disagreement is resolved above.
