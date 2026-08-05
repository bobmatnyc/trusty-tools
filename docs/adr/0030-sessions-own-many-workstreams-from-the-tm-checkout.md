# 0030. A session owns many workstreams and lives in the tm checkout

- **Status:** Proposed
- **Date:** 2026-08-05
- **Scope:** crate `trusty-mpm` (session/workstream model, `session_manager`, `daemon::managed_routes` launch paths, worktree provisioning and reclamation)
- **Reversibility Cost:** High — reverses the session↔workstream cardinality that every session record, launch path, label, and reclamation gate is built on, and moves the session's own home directory
- **Decision Drivers:** owner ruling of 2026-08-05; a session pinned to one worktree cannot start a second piece of work without becoming a second session; agent-created worktrees are invisible to every registry; there is no state for "this workstream is finished, start the next one"; the tm checkout's fetch is tied to daemon lifetime, so a long-lived daemon serves stale branch start points
- **Supersedes / Superseded by:** Re-scopes **DOC-52** §1.5 / §3.1 (the "permanent, sanctioned" trusty-mpm 1:1 exception). Reverses the launch-target half of **#1590**. Builds on ADR-0020 and ADR-0023, reversing neither.

## Context

**Terminology.** The **tm checkout** is the directory tm creates at `repos_root().join(owner).join(repo)` — by default `~/trusty-mpm-projects/<owner>/<repo>` (`daemon/managed_routes/inproject.rs:134-136`, `DEFAULT_REPOS_DIR` at `:56`). It is not **the user's own clone** of the repo, wherever that lives. This ADR uses those two terms and never says "main checkout", which was read both ways and cost a round of wrong analysis.

**trusty-mpm binds one session to one workstream, by construction.** `SessionRecord` (`crates/trusty-mpm/src/session_manager/record.rs:190-270`) has no workstream field. `SessionCorrelation` (`crates/trusty-mpm/src/driver/correlation.rs:33-43`) carries a single `worktree: Option<PathBuf>` and a single `branch: Option<String>` — singular, not collections. The worktree branch is `format!("session/{name}")` (`crates/trusty-mpm/src/core/worktree_naming.rs:33-35`), and the only "workstream" artifact that exists anywhere is the GitHub label `ws/<session-name>` (`crates/trusty-mpm/src/core/session_launch/workstream_label.rs:93-136`), keyed off the session name and therefore 1:1 by construction. DOC-52 §1.5 ratifies this as a permanent exception to its own repository-wide cardinality.

**The session lands in a worktree under the tm checkout, not in the tm checkout itself.** `daemon::managed_routes::lifecycle::spawn_managed_routed` (`lifecycle.rs:349-418`) picks the path; `project::worktree_policy::worktree_enabled_in` (`worktree_policy.rs:84-106`) is the toggle, defaulting to enabled. Landing in the checkout directory itself exists only as a per-project opt-out (`launch_on_main::spawn_managed_on_main`, `launch_on_main.rs:93-229`). The design intent is stated verbatim at `crates/trusty-mpm/src/bin/tm/commands/launch.rs:86-87`: *"The live checkout is NEVER touched … the tmux cwd is the managed clone (#1590)."*

**Three consequences follow, and all three are live defects.**

1. A second piece of work needs a second session. There is nowhere to put it.
2. Agent worktrees are invisible. A live session on this machine accumulated roughly eight per-PR worktrees created by agents running plain `git worktree add` per the `CLAUDE.md` convention; none appears in any session registry, and `session_manager::worktree_nested` notices them only to avoid deleting them.
3. There is no transition back out of a workstream. `workspace_path` is written once at spawn (`session_manager/manager.rs:1125`) and no caller repoints it; decommission (`session_manager/decommission.rs:460-538`) is destructive-only. The tmux pane's OS cwd is set at spawn and nothing ever `cd`s it, so editing the record would desynchronize the record from the process rather than relocate the session.

Cleanup is idle- or age-triggered (`daemon/idle_reaper.rs`) or a manual `tm prune --merged-prs` sweep (`session_manager/worktree_reclaim.rs:91-125`). Nothing is merge-triggered.

**And the tm checkout's fetch is tied to daemon lifetime, not to branch creation.** `inproject_hygiene::run_hygiene_for_base` does fetch (`inproject_hygiene.rs:291-303`), but only from `run_hygiene_for_all_bases` (`:405-437`), whose single non-test caller is daemon startup (`daemon/mod.rs:149`). A daemon up for weeks serves branches cut from whatever the base was at boot — the reported 1.3.4 failure, where the base froze at 2026-07-18 and every session since inherited it.

## Decision

We will make a session a container of workstreams, and give it a home that is not a worktree.

**What "managed" means, since it governs the rest.** tm maintains tm-specific configuration inside the tm checkout — the relocated `CLAUDE_CONFIG_DIR`, deployed agents and skills, compiled instructions, statusline and settings wiring, `.trusty-mpm/` state. That serves one boundary in two directions: outward, tm's configuration does not leak into a repo where someone runs plain `claude`; inward, it is where the user's own tm-specific customization lives. It does **not** mean access-controlled or tm-exclusive — the directory holds the user's config, so the user has standing reason to open and edit it, and external editors may work there while a session runs. DOC-66 §0.3 and §0.4 carry the full definition and its consequences.

1. **A session owns many workstreams.** One session : N workstreams. This **reverses** the 1:1 binding DOC-52 §1.5 records as permanent and sanctioned; that section is re-scoped by this ADR, not left standing.

2. **A workstream is one worktree plus one branch, yielding one PR.** It is the unit that gains an identity, a lifecycle, and a slot. The session gains a name and a set of owned workstreams.

3. **The session's home is the tm checkout.** Its tmux pane starts there and is never `cd`-ed away for the session's whole life. Work happens in workstream worktrees, entered by the agents the session dispatches with an explicit cwd — not by relocating the session.

4. **The tm checkout is refreshed at every new session and at session restoration, by fetch and fast-forward only.** Always `git fetch`; fast-forward the default branch only when the tree is clean and actually on it; never `git pull` into a dirty tree, `reset --hard`, `clean`, merge, rebase, or stash. A user's editor may hold uncommitted work there, so anything that discards local state is a data-loss path rather than hygiene. Widening the trigger past daemon startup is the fix for the staleness defect in Context.

5. **Workstream branches are cut from `origin/<default>` explicitly.** The refresh is for the human reading the directory; the explicit start point is for correctness. Because the start point never consults the local ref, a skipped or failed fast-forward costs nothing — which is what lets point 4 be as conservative as a shared directory requires.

6. **Roughly five active workstreams per session, advisory and configurable.** The limit exists because each workstream's conversation surfaces to the user, and more than about five exceeds what one person can attend to. It is an **attention** limit. It is not WIP reduction — prior analysis in this repo refuted that framing, and re-justifying the cap as throughput control would be a regression to a rejected argument. Exceeding it nudges; it never blocks.

7. **Agent worktrees are children of a workstream, not workstreams.** They are created under the workstream that dispatched them, reaped when the agent exits, and never consume a slot.

8. **Working directly on the default branch stays available.** The `worktree: false` override already exists and is not re-designed here; only its shape changes, from a per-project config key to a per-session-overridable flag targeting the tm checkout (DOC-66 §3.5).

The data model, the state machine, the slot arithmetic, the refresh rules, and the disk strategy are specified in [DOC-66](../specs/DOC-66-session-workstream-model.md). This ADR records the decision only.

### What this does to #1590

`bin/tm/commands/launch.rs:86-87` states the design intent verbatim: *"The live checkout is NEVER touched … the tmux cwd is the managed clone (#1590)."* Reversing a named, deliberate design decision has to be said plainly, so: **this decision reverses #1590's launch target.** A session lands in the tm checkout itself rather than in a per-session worktree under it.

**It does not cross the boundary #1590 defends.** That boundary is config isolation — it runs between the tm checkout and everything outside it, keeping tm's configuration out of repos where someone runs plain `claude`. Both the old launch target and the new one sit *inside* that boundary; the decision operates entirely on its interior. Nothing about the guarantee is weakened or traded away, and the user's own clone is untouched either way.

What does need saying, on its own terms rather than as a residual of #1590: because the tm checkout is shared with the user and their editors, uncommitted and untracked content is its **expected steady state**, and git recovers none of it. That is why point 4 above is non-destructive. DOC-66 §0.4 and §3.3 specify what it requires.

## Consequences

### Positive

- Starting a second piece of work stops requiring a second session, which is the change the owner asked for.
- Agent worktrees acquire an owner and a reaper. Unreaped children are the documented source of worktree sprawl; they are currently owned by nobody.
- A slot freed at **merge** rather than at cleanup means a slow reviewer cannot starve a session. Today nothing is merge-triggered at all.
- Refreshing at every new session and restore, rather than at daemon startup, retires the staleness class behind the 1.3.4 report.
- The missing transition out of a workstream is not implemented — it is dissolved. A session that never leaves the tm checkout has nothing to return to, and the record/process desynchronization risk never arises.

### Negative / Trade-offs

- **Existing 1:1 sessions have no migration, and this is the decision's largest risk.** A live session's `workspace_path` points at a worktree, and that pane's OS cwd cannot be moved by editing a record — no amount of record rewriting relocates a running process. Such a session cannot become a 1:N session in place; it can only be finished under the old model. Unlike every other item here, this one has no clean answer, which is why DOC-66 §7 records it as an open question rather than inventing a rewrite.
- **Disk.** Each worktree is a full checkout with its own `target/`. A measured example on this machine: 300 GB across ten worktrees, one `target/` alone at 88 GB. Five workstreams per session, across several sessions, is terabytes. Without the shared-build strategy DOC-66 §6 specifies, the cap of five is a disk cap wearing an attention cap's clothes, and the attention justification becomes false advertising.
- **`ws/<session-name>` becomes the wrong key.** The label is keyed off the session name, which under 1:N no longer identifies a workstream. Existing labels on merged and open PRs are not rewritten by this ADR.
- **Ownership records must gain a workstream grain.** ADR-0020's `worktree_owner` and ADR-0023's registration index are keyed on a session id; under 1:N a session owns several worktrees and the key stops discriminating.
- **The startup hygiene `reset --hard` is a confirmed data-loss path.** `inproject_hygiene::run_hygiene_for_base` runs a safety-gated `reset --hard` against the tm checkout at daemon startup (`inproject_hygiene.rs:291-303`, swept by `run_hygiene_for_all_bases` at `:405-437`). A separate audit reproduced it: the gate's `is_dirty` check omits `--ignored`, so gitignored content is invisible to it and gets silently overwritten where it collides with a tracked path. That is precisely what decision point 4 forbids. A fix is in flight on another branch and is not specified here.

### Neutral / Follow-up work

- Whether `worktree_policy`'s per-project key survives as a default once the override becomes a per-session flag.
- Whether DOC-52 §5.1's blocking, repo-size-scaled cap on open workstreams coexists with this advisory attention cap, or one of the two is withdrawn.
- Reviewing the gate on the startup hygiene `reset --hard` before this model ships.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-08-05:

- **ADR-0020 (Session-owned worktrees):** **Extends.** Its sentinel, registry field, and owner-gated reclamation all stand. What changes is the grain: `owner_session_id` no longer discriminates between a session's several worktrees, so the sentinel payload must carry a workstream id as well. No mechanism of ADR-0020 is reversed.
- **ADR-0023 (git decides existence, a rebuildable index decides ownership):** **Consistent.** The workstream record specified in DOC-66 §1 *is* an ownership record in ADR-0023's sense: it never answers existence, and it is rebuildable from sentinels plus `git worktree list --porcelain`. ADR-0023 point 6 defers branch role as an ownership signal, gated on provisioning actually enforcing a workstream-branch convention — this decision is what would establish that convention, and the deferral stands until it ships.
- **ADR-0019 (Unified IPC messaging / never key on fragile session_id):** **Consistent, and reinforced.** Addressing keyed on the workstream rather than the session is exactly what 1:N requires.
- **ADR-0016 (Orchestration Hierarchy):** **Consistent.** A session owning several workstreams is a resource-ownership statement, not a role statement; the PM/Assistant hierarchy is unaffected.
- **ADR-0025 (Collapse agent and skill tier hierarchies) / ADR-0029 (MSRV):** **No interaction.**
- **DOC-52 (Shared Workstream Definition) §1.5, §3.1** *(spec, cited per DOC-46's ADR↔Spec cross-linking rule)*: **Conflict, resolved in favour of this ADR.** DOC-52 states trusty-mpm's session ≡ workstream 1:1 binding is "permanent, documented, sanctioned … not something scheduled to be fixed later," and instructs that no ticket be filed to reconcile it. The owner ruling of 2026-08-05 reverses that. §1.5 and §3.1 are re-scoped: trusty-mpm converges on DOC-52 §2's cardinality for the workstream↔session relation rather than remaining an exception to it. DOC-52 is not edited by this ADR; the correction is listed in DOC-66 §9.
- **DOC-53 (Workstream claim-drawer convention):** **Conflict, deferred.** Its `ws:<name>` identity is drawn from the tm-assigned session name and is valid, per its own text, *because of* the DOC-52 §1.5 exception. Removing that exception invalidates the derivation. DOC-66 §7 records the re-keying as an open question rather than deciding it here.
- **DOC-34 (Managed sessions launch with a tm-owned `CLAUDE_CONFIG_DIR`):** **Consistent, and load-bearing.** The relocated config dir is half of what "managed" means (Decision, above); this ADR changes where a session's cwd sits inside the managed tree and nothing about the config-dir relocation.
- **DOC-48 (tcode workstreams):** **No interaction.** trusty-code already implements the many-sessions-per-workstream shape; this decision concerns trusty-mpm's session→workstream fan-out, a different axis.

Summary: extends ADR-0020, consistent with ADR-0023, ADR-0019, ADR-0016, and DOC-34, and resolves a conflict with DOC-52 §1.5/§3.1 in favour of the owner's 2026-08-05 model. One conflict — DOC-53's session-derived workstream key — is recorded as open, not resolved.
