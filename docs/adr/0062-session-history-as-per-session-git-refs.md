# 0062. Session history moves to per-session, append-only git refs

- **Status:** Accepted
- **Date:** 2026-09-13
- **Scope:** crate `trusty-mpm` — session pause/resume
  (`.trusty-mpm/sessions/`), the sessions-log event store, the main-checkout
  fast-forward watch
- **Reversibility Cost:** Low — the refs are additive. A session can stop
  writing them at any time with no migration; no branch, PR, or main-checkout
  history is touched by this decision.
- **Decision Drivers:** the owner's 2026-09-13 ruling, tracked as #7830; the
  concurrent-pause race PR #7782 recorded, where two sessions raced a
  snapshot commit into `.trusty-mpm/sessions/`; the main-checkout
  fast-forward watch blocking on a dirty sessions log; ADR-0061's own
  2026-09-13 amendment, which superseded the 2026-08-31 ruling that the store
  was tracked and made `.trusty-mpm/sessions/` gitignored — removing the race,
  but also removing the only durable, shared record of session history.
- **Supersedes / Superseded by:** Amends
  [ADR-0061](0061-commits-never-land-on-local-main.md) — adds a second,
  ref-based history for session and activity data, alongside (not replacing)
  ADR-0061's commit rules for source, docs, and session notes. ADR-0061's
  status is updated to `Amended by [0062](0062-session-history-as-per-session-git-refs.md)`,
  mirroring the correction made to ADR-0049's status in PR #7784. Consistent
  with [ADR-0053](0053-fetch-and-pull-are-permitted-in-a-main-checkout.md):
  fetching and pushing these refs is not a `git commit` and moves no local
  `main` HEAD. Supersedes nothing.

## Context

Session and activity data — pause snapshots, sessions-log events — records
work in progress that never appears in the git log. Two rulings tried to
place this data inside the normal git history, and both ran into the same
problem: a commit is the wrong unit for data that changes every few minutes.

The 2026-08-31 ruling tracked `.trusty-mpm/sessions/` in the repository.
Every pause then needed a PR and a fast-forward of the main checkout. Two
sessions pausing at the same time raced a commit into the same file; PR
#7782 recorded one such race, where a snapshot was duplicated. Separately,
the main-checkout fast-forward watch (ADR-0053, ADR-0061) blocked whenever
the sessions log had uncommitted changes, because the watch cannot tell a
dirty log from a source change in progress.

ADR-0061's 2026-09-13 amendment fixed the race and the blocked watch by
making `.trusty-mpm/sessions/` gitignored. That removed session history from
the repository, and with it the only shared, durable record of what a
session did. The owner ruled the same day, tracked as #7830, that session
history needs a second place to live — one that never contends with a
source or docs commit, never blocks the fast-forward watch, and is still
durable and shared through the same remote.

## Decision

1. **Session history lives in git refs, outside the branch namespace.** Each
   session writes to its own ref: `refs/tm/sessions/<user-id>/<session-key>`.
   The ref key is the user id plus the session key, so exactly one writer
   owns each ref. Two sessions never write the same ref, and the
   duplicate-snapshot race PR #7782 recorded cannot recur.
2. **Each ref is an orphan, append-only commit chain.** It shares no history
   with `main` and no history with any other session's ref. A pause appends
   one commit to the session's own chain; a resume reads the chain's tip. No
   session write ever lands on a code branch.
3. **A write is a lease-checked push, not a merge.** The daemon pushes with
   `--force-with-lease` against the ref's last known tip. A push whose lease
   does not match the remote tip is rejected as non-fast-forward, so a stale
   writer cannot silently overwrite a newer snapshot. No session write ever
   needs a PR.
4. **The working-tree `.trusty-mpm/sessions/` directory becomes a local
   cache.** It stays gitignored, per ADR-0061's amendment. The git ref is the
   durable copy; the working-tree files are read/write scratch space the
   daemon reconstructs from the ref on resume.
5. **tm fetches and pushes these refs on a dedicated refspec**,
   `+refs/tm/sessions/*:refs/tm/sessions/*`, separate from the refspec a
   plain `git fetch`/`git push` uses for branches. A clone or checkout that
   does not add this refspec sees no session refs at all — the correct
   default, since this is machine data, not project history.
6. **The daemon aggregates activity by reading refs, not by committing.**
   `git for-each-ref` lists every `refs/tm/sessions/**` ref; `git cat-file`
   reads a ref's tip commit. This is how the daemon reports who is doing
   what across sessions, with none of it appearing as a commit on `main`.
7. **Commit cadence inside a ref stays coarse.** A ref commit records a
   pause, a resume, or a checkpoint — not every event. Fine-grained
   sessions-log events stay in the jsonl files already inside the working
   tree; this ADR does not move them into the ref.
8. **Retention and the sharing model are explicitly out of scope, deferred
   by the owner.** Refs are kept indefinitely for now; pruning old session
   refs is a separate, later decision. Who may fetch whose refs — every
   session's, or only one's own — is also not decided here. Both stay open
   questions for a future ADR.
9. **The pre-push credential scan still runs.** Nothing about a session ref
   changes what content is safe to push. The existing secret-scan gate that
   blocks a push carrying a credential applies to this refspec the same way
   it applies to the branch refspec.
10. **Push access is the trust boundary (owner ruling 2026-09-14).**
    `refs/tm/sessions/**` carries no server-side access control: a
    collaborator with push access to `origin` can create a ref under any
    user id and any session key. That collaborator is trusted to write
    session history, and the integrity of a session ref rests on the forge's
    push permission — the same boundary that already governs who can push a
    branch. The reader hydrates only the caller's own ref
    (`<user-id>/<session-key>`) as defence in depth, not as authentication.
    Signed-commit verification and forge-side ref rules are deferred
    alongside decision 8; a repository whose push set is not trusted should
    set `[session_refs] enabled = false`.

## Consequences

- **No PR and no merge-queue slot per pause.** A session pause is one push
  to its own ref; nothing else in the delivery pipeline sees it.
- **No race between concurrent sessions.** The ref key already separates
  every session's history; `--force-with-lease` catches the one remaining
  case — a session racing itself across two processes.
- **Push access is the trust boundary (owner ruling 2026-09-14).**
  `refs/tm/sessions/**` carries no server-side access control: a collaborator
  with push access to `origin` can create a ref under any user id and any
  session key. That collaborator is trusted to write session history, and the
  integrity of a session ref rests on the forge's push permission — the same
  boundary that already governs who can push a branch. The reader hydrates
  only the caller's own ref (`<user-id>/<session-key>`) as defence in depth,
  not as authentication. Signed-commit verification and forge-side ref rules
  are deferred alongside decision 8; a repository whose push set is not
  trusted should set `[session_refs] enabled = false`.
- **These refs are invisible in the GitHub UI, in PR diffs, and to branch
  protection.** `refs/tm/sessions/**` is not `refs/heads/**`, so no PR ever
  lists a session-ref commit and no branch-protection rule ever inspects
  one. This is the intended trade for machine data that was never meant to
  be reviewed like source or docs.
- **A plain clone sees nothing.** Without the added refspec, a clone or a CI
  checkout fetches no session refs. Session data stays opt-in for the
  tooling that knows to ask for it.
- **Commit cadence in the ref stays coarse.** The ref records
  pause/resume/checkpoint boundaries, not every event; fine-grained events
  keep living in the jsonl files the working tree already carries.
- **Retention is unbounded until a follow-up ADR sets a policy.** Refs
  accumulate for every session that has ever paused. This is accepted for
  now because pruning is a distinct decision the owner deferred, not an
  oversight.
- **The sharing model is undecided.** Until a follow-up ADR states who may
  fetch whose session refs, the refspec's blanket `refs/tm/sessions/*` shape
  is the only shape defined; a later decision may narrow it per user or per
  project.

## Alternatives Considered

- **A dedicated private repository for session data.** Rejected as the
  runner-up: it needs its own provisioning and its own access grants, a
  second surface to keep in sync with the main repository's remote and
  credentials, for data the main repository's own remote can already carry
  as a ref.
- **Per-user gists.** Rejected: a gist is one file per URL, reachable
  independently of the project's own access control, and does not compose
  with `git for-each-ref` aggregation across many sessions.
- **Issue or discussion threads.** Rejected: a sessions-log event does not
  parse as markdown prose, and a snapshot posted every few minutes would
  flood the issue tracker used for actual project work.
- **The repository wiki.** Rejected: wiki tooling is thin compared to plain
  git refs, and a wiki page is public with the repository the same way an
  issue is — the property the tracked-store ruling was already moving away
  from.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-09-13:

- **ADR-0061 (Commits never land on local `main`):** **Amends.** ADR-0061's
  commit rules for source, docs, and session notes are unchanged; this ADR
  adds a second history, in refs rather than commits on any branch, for
  session and activity data specifically. ADR-0061's 2026-09-13 amendment,
  which made `.trusty-mpm/sessions/` gitignored, assumed session data had
  nowhere durable to live once it left the tracked tree. It now does: the
  ref this ADR defines.
- **ADR-0053 (`git fetch` and `git pull` are permitted in a main checkout):**
  **Consistent.** The daemon's fetch and push of `refs/tm/sessions/**` is a
  separate refspec operation, not a commit, and does not move local `main`'s
  HEAD; ADR-0053 already permits fetch and pull in a main checkout without
  qualifying which refspec.
- **ADR-0049 (Documents-only commits are permitted in a main checkout),
  amended by ADR-0061:** **Consistent, unreached.** A session ref write is
  not a `git commit` against the working checkout's index — the daemon
  builds the session ref's commit chain with plumbing, against a different
  ref entirely — so ADR-0049's staged-set classifier never runs against it.
- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** **Consistent, unreached.** Decision 10's live-writer,
  HEAD-move concern is about commits and merges that move a branch's HEAD;
  a session ref shares no history with any branch and moves no branch's
  HEAD.
- **ADR-0054 (Session sync commits to a side branch without moving HEAD,
  Proposed):** **Related, distinct mechanism.** ADR-0054 proposed one fixed
  branch, `trusty/session-sync`, receiving commits for `.trusty-memories/`
  content. This ADR gives each session its own ref instead of one shared
  branch, specifically to remove the single-writer contention a shared
  branch would still carry across concurrent sessions. ADR-0054 remains
  Proposed and unaffected; the two address different content
  (`.trusty-memories/` versus session pause/activity history) and neither
  has shipped code the other must reconcile with.

No other Accepted ADR governs session or activity data storage.
