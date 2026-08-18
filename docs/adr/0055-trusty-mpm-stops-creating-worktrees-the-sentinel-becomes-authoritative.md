# 0055. Trusty-mpm stops creating worktrees; the sentinel becomes authoritative over every worktree it does not create

- **Status:** Accepted
- **Date:** 2026-08-18
- **Scope:** crate `trusty-mpm` — `provisioner::workspace` (`GitBackend` trait,
  `RealGitBackend`, `FakeGitBackend`), `daemon::managed_routes::lifecycle`
  (`spawn_managed_cloned`), the `session_new` MCP tool schema
  (`mcp/mod.rs`, `mcp/tools/session.rs`), and the `.trusty-mpm-worktree`
  sentinel's write coverage across `.claude/worktrees/`
- **Reversibility Cost:** Medium — restoring clone-and-worktree creation is a
  code revert with no data migration, but every worktree provisioned in the
  interval carries no path back to a cloning session; closing the sentinel
  coverage gap this ADR opens is separate, ongoing work with its own cost
- **Decision Drivers:** owner ruling that trusty-mpm's own worktree-creation
  path is removed; owner ruling that a non-local `session_new` call becomes a
  hard error naming the remedy; owner ruling that the sentinel stays and
  becomes authoritative; the creation path measured as dead on every
  registered project but one; git's inability to answer worktree provenance
  (ADR-0023)
- **Supersedes / Superseded by:** none

## Context

**The creation path this ADR removes is a second one, not the only one.**
`daemon::managed_routes::inproject.rs` already provisions local worktrees
through its own `ensure_base_clone` (:280) and `create_session_worktree`
(:462), with no `GitBackend` reference at all. Nothing in this ADR touches
that path; it is how a `session_new` call against a LOCAL `repo_url` keeps
working, and it is the escape hatch decision B below routes every rejected
call toward. `content::catalog_sync.rs` also depends on the `GitBackend`
trait — for framework-catalog cloning — but calls neither method this ADR
removes; it is unaffected.

**The path removed here is `GitBackend::ensure_base_checkout` /
`GitBackend::worktree_add`** (`crates/trusty-mpm/src/provisioner/workspace.rs:140-186`),
their `RealGitBackend` / `FakeGitBackend` implementations, and the two
`provision` / `provision_in` call sites in
`daemon::managed_routes::lifecycle::spawn_managed_cloned`
(`lifecycle.rs:676-694` and `lifecycle.rs:1199-1204`). It is the only
`GitBackend`-mediated path that clones a REMOTE `repo_url` on `session_new`'s
behalf.

**It was already not trusty-mpm's only worktree-creation authority — the
agent-dispatch path had already moved to the harness.** ADR-0044 decision 4
found no trusty-mpm production path creates an agent worktree: Claude Code
creates one through `Agent(isolation: "worktree")` or `EnterWorktree`, and
`bin/tm/commands/pm_guard_worktree_grant.rs:172` only injects
`isolation: "worktree"` into the dispatch call via
`hookSpecificOutput.updatedInput` — it does not run `git worktree add`
itself. This ADR removes a DIFFERENT path: the session-level clone-and-worktree
flow behind `session_new`, which ADR-0044 did not examine because it is not
agent dispatch.

**The registry that tracks worktree EXISTENCE is already git-native and needs
no change here.** `session_manager::worktree_registry.rs`'s own header states
the finding ADR-0023 formalized: "Git already maintains the answer.
`git worktree list --porcelain` is the registry." There is no duplicate
enumeration file this removal must also delete.

**The creation path is verifiably dead, not merely unused by convention.**
All five projects this daemon has registered — apex, hackathon, trusty-tools,
writing, cto — carry no `.base` directory, no `.worktrees/` directory, and no
session worktree on disk. The clone path writes into
`<workspace_root>/<owner>/<repo>/`, and of those five only
`bobmatnyc/trusty-tools` exists under that root at all. The other four were
never provisioned through this path — the directory was never created — not
merely cleaned up after use.

**The `.base` bare clone this path once depended on is already retired.**
Post-#4270, `workspace.rs:783-786` records that the base checkout IS the
project directory, cloned non-bare — there is no separate bare intermediate.
A stale doc comment at `core/workspace_liveness.rs:9,25` still describes the
old `.base/.worktrees/<session-id>` shape from before that change; it
describes history, not the current mechanism, and is not corrected by this
ADR.

**`session_new` currently documents a capability this ADR deletes.** The MCP
tool schema (`mcp/mod.rs:140`, mirrored in
`assets/skills/tm-capabilities/references/mcp-tools.md:87`) states: "A LOCAL
`repo_url` — an absolute path to an existing directory — runs the session on
that main checkout itself (ADR-0037); only a remote URL is cloned into a
freshly-provisioned workspace." The second half of that sentence is the
capability decision A removes. `daemon::managed_routes::lifecycle.rs:394`
shows `spawn_managed_cloned` is the unconditional fallback for any
`repo_url` that fails `is_local_workdir` — every non-local call routes there
today. Removing the path without replacing its behavior would leave that
fallback with nowhere to go.

**Git cannot answer who created a worktree, at any location, ever.**
`worktree_registry.rs:274-278` states the limit directly: "nothing in the
porcelain record, and nothing in the `worktrees/<id>/` admin directory,
records WHO ran `git worktree add`." ADR-0023 already reached this
conclusion and answered it for trusty-mpm-provisioned worktrees by retaining
the `.trusty-mpm-worktree` sentinel as the one provenance carrier layered in
front of git's existence-only view. That sentinel is written on every
worktree trusty-mpm itself creates. It is not written on a worktree the
harness creates.

**The gap the sentinel does not yet close, measured on this machine on
2026-08-18:** 57 directories exist under `.claude/worktrees/`; 6 of them carry
a `.trusty-mpm-worktree` sentinel. The other 51 are harness-created worktrees
trusty-mpm can neither attribute to a session nor safely reap. Of the 6
sentinel-bearing directories, one disagrees with its own path:
`agent-a23097670a51a0450/.trusty-mpm-worktree` records
`agent_id: a5ed76bc80fd52e41` — a mismatch whose cause is undetermined.
Separately, `git worktree list --porcelain` registers 56 entries against 57
directories — one directory git itself does not recognize as a worktree.
There are 144 `worktree-agent-*` branches; 88 have no worktree at all, and 53
are unmerged. **Branch retention here is deliberate, not drift.**
`agent_worktree_reap.rs:200` documents it directly: "The branch is NOT
deleted. It may carry the pushed commits an open PR is built on" — an
unmerged branch behind a reaped worktree is expected steady state, not a
condition decision C's authority extension is meant to close.

## Decision

**A. Trusty-mpm's own worktree-creation path for `session_new` is removed.**
`GitBackend::ensure_base_checkout` and `GitBackend::worktree_add`
(`provisioner/workspace.rs:140-186`), their `RealGitBackend` and
`FakeGitBackend` implementations, and the `provision` / `provision_in` call
sites in `spawn_managed_cloned` (`lifecycle.rs:676-694`, `:1199-1204`) are
deleted. Trusty-mpm no longer clones a remote repository or runs
`git worktree add` on `session_new`'s behalf, for any `repo_url`.
`content::catalog_sync.rs` and `daemon::managed_routes::inproject.rs` are
unaffected — neither calls either removed method.

**B. A `session_new` call whose `repo_url` fails `is_local_workdir` becomes a
hard error.** With decision A's fallback removed, `spawn_managed_cloned` has
nowhere to route such a call. It must fail loudly rather than silently
degrade, and the error must tell the operator to clone the repository first
and pass the local path — the route `inproject.rs`'s existing local path
already serves. The `session_new` MCP tool schema's "only a remote URL is
cloned into a freshly-provisioned workspace" sentence is deleted along with
the capability it describes; the documented contract becomes local-path-only.

**C. The `.trusty-mpm-worktree` sentinel stays, and becomes authoritative
over every worktree under `.claude/worktrees/`, not only the ones trusty-mpm
itself creates.** Every harness-created worktree gains a sentinel, and
existing drift — the 51 unsentineled directories and the one path/payload
mismatch measured above — is reconciled. The rejected alternative was
leaving the sentinel at its current coverage, which is correct only for
worktrees trusty-mpm creates and, after decision A, describes a strictly
smaller set of the worktrees that actually exist.

## Consequences

- **Decision A removes dead code with no user-visible loss for the local
  case.** No registered project's worktrees were reached through this path;
  the measurement in Context is the evidence, not an inference from disuse.
- **Decision B trades a silent capability loss for a loud, actionable one.**
  Before this ADR, a non-local `session_new` call succeeded by cloning. After
  it, the same call fails immediately, naming the local-clone workaround. An
  operator or agent that depended on remote-URL spawning must adopt a
  clone-then-`session_new` two-step; nothing in this ADR builds that
  automatically.
- **Decision C is a design problem, not an implementation detail, and this
  ADR deliberately does not resolve it.** Writing a sentinel on every
  harness-created worktree means hooking a creation event trusty-mpm does not
  own — Claude Code, not trusty-mpm, runs `git worktree add` for
  `Agent(isolation: "worktree")` and `EnterWorktree`. The write cannot hang
  off the creation call itself, because trusty-mpm is not the caller. It must
  hang off something trusty-mpm DOES see: a hook trusty-mpm already occupies,
  or a reconcile sweep that adopts an unattributed tree after the fact. This
  ADR names that as the design problem the implementation inherits; it does
  not choose between them.
- **Decision C changes the context of #5800, without resolving it.** #5800
  is the reap deleting a worktree the harness still considers resumable
  (`agent_worktree_reap.rs:351,398`). `rebuild_from_disk` (:398) is what
  authorized the bad reap in that report — it trusts the sentinel to recover
  a worktree path across a daemon restart. An authoritative sentinel makes
  that trust reach a larger population of worktrees, which raises the stakes
  of #5800's bug without changing its mechanism or its fix. The two remain
  separate pieces of work: this ADR does not fold #5800 in, and #5800's fix
  does not satisfy decision C.
- **The 53 unmerged, worktree-less `worktree-agent-*` branches are explicitly
  out of scope for decision C's reconciliation.** `agent_worktree_reap.rs:200`
  establishes that a reaped worktree's branch survives on purpose, to carry
  an open PR's commits. Decision C closes the sentinel-coverage gap on
  worktrees that exist; it does not touch branches whose worktree is
  already, correctly, gone.
- **The one path git does not register (56 `git worktree list` entries
  against 57 directories) is a separate defect from the sentinel gap.**
  Decision C gives every directory a sentinel; it does not make git recognize
  a directory git currently does not. That mismatch is recorded here as an
  open question for whichever future work implements the design decision C
  names, not resolved by this ADR.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and the ADRs it lists on 2026-08-18:

- **ADR-0020 (Session-owned worktrees: sentinel + owner-gated reclamation):**
  **Extends.** ADR-0020 introduced the sentinel and, in "Out of scope,"
  reserved its shape "in case a future ADR unifies ownership tracking across
  all three stores." Decision C is that unification, arriving nine ADRs later
  than ADR-0020's own follow-up list expected but answering exactly the
  question it deferred. ADR-0020's mechanism — the JSON payload,
  `SessionRecord.worktree_owner`, owner-gated decommission, owner-gated
  orphan-GC, `resolve_ownerless_with_grace` — is unchanged; this ADR widens
  WHERE the sentinel is written, not what it means once written.
- **ADR-0023 (Worktree authority split: git decides existence, a rebuildable
  index decides ownership):** **Extends.** ADR-0023 states plainly that git
  cannot answer provenance and settles, against issue #4208's proposal, that
  the sentinel stays as the one provenance carrier rather than being replaced
  by a purely git-derived proof. Decision C is that settled position applied
  to a population of worktrees ADR-0023 did not cover — harness-created ones
  — using the same sentinel mechanism ADR-0023 already chose. No change to
  the existence/ownership split itself: git remains sole authority for
  existence, the sentinel remains the provenance carrier for ownership.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees/`):**
  **Consistent, and completed on the question it left open.** ADR-0036's own
  "Neutral / Follow-up work" section named this exact question — "Whether
  ADR-0020's reserved sentinel shape should now actually be written into
  harness-created worktrees, giving them a resolvable owner instead of the
  permanent owner-unknown status they hold today" — and stated plainly that
  it "does not decide whether the harness store gains provenance, and today
  it cannot, since the harness writes no sentinel." Decision C decides that
  question in the affirmative. ADR-0036's topology (one flat directory, no
  nesting) is unchanged and is the directory decision C's coverage sweep
  operates over.
- **ADR-0044 (Main-checkout write boundary and agent-worktree ownership):**
  **Consistent, and narrower than it might read.** ADR-0044 decision 4 found
  no trusty-mpm production path creates an AGENT worktree and assigned that
  creation to the harness. This ADR's decision A removes a SESSION-level
  clone-and-worktree path ADR-0044 did not examine — `session_new`'s own
  provisioning, not agent dispatch. The two findings are independently true
  and now describe the same shape at both levels: trusty-mpm creates no
  worktree of either kind. Decision C's sentinel-writing problem is the
  mirror image of ADR-0044's own boundary — trusty-mpm must attribute
  worktrees created by a process it does not control, the same relationship
  ADR-0044 already establishes for agent dispatch.
- **ADR-0048 (Dispatched writers get a worktree; the write boundary is
  enforced):** **Consistent, no interaction.** ADR-0048 governs the harness
  GRANT mechanism (`isolation: "worktree"` injected into a dispatch) and the
  main-checkout write boundary. Neither is a worktree-CREATION path this ADR
  touches; ADR-0048 decision 2 restates ADR-0044 decision 4 ("Trusty-mpm
  still creates no worktrees... it asks; it does not own") and this ADR is
  consistent with that restatement at the session level as well.
- **ADR-0049 (Documents-only commits are permitted in a main checkout):**
  **No interaction.** Governs commit permission inside a shared main
  checkout; this ADR governs worktree creation and provenance. Neither
  makes a claim the other can contradict.

No prior Accepted decision is superseded. ADR-0036's and ADR-0020's own
follow-up sections are the ones this ADR answers, and both are recorded above
as Extends rather than Supersedes: neither ADR's Decision or Consequences
section is reversed, only its stated open question is resolved.
