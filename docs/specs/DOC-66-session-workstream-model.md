---
spec_refs:
  - id: SPEC-SHAREDWS-01~draft
    path: docs/specs/DOC-52-shared-workstream-definition.md
    anchor: SPEC-SHAREDWS-01~draft
    note: canonical glossary; this spec re-scopes its §1.5 trusty-mpm 1:1 exception
  - id: SPEC-SHAREDWS-02~draft
    path: docs/specs/DOC-52-shared-workstream-definition.md
    anchor: SPEC-SHAREDWS-02~draft
  - id: SPEC-SHAREDWS-03~draft
    path: docs/specs/DOC-52-shared-workstream-definition.md
    anchor: SPEC-SHAREDWS-03~draft
    note: resource rules; §4 below is a second, advisory cap that does not replace §5.1's blocking one
---

# DOC-66 — Session and Workstream in trusty-mpm: the 1:N model, lifecycle, and slot semantics

**Status:** Draft
**Subsystem:** crate `trusty-mpm` — session/workstream data model, launch and provisioning paths, worktree reclamation
**Owner:** Engineering (trusty-mpm) / Bob Matsuoka
**Last-updated:** 2026-08-05
**Spec ID:** `SPEC-SESSWS-01~draft` … `SPEC-SESSWS-06~draft` (DOC-66)
**Builds on:**
- [`docs/adr/0030-sessions-own-many-workstreams-from-the-main-checkout.md`](../adr/0030-sessions-own-many-workstreams-from-the-main-checkout.md) — the decision this spec encodes
- [`docs/adr/0023-worktree-authority-existence-vs-ownership.md`](../adr/0023-worktree-authority-existence-vs-ownership.md) — git owns existence; a rebuildable record owns ownership
- [`docs/adr/0020-session-owned-worktrees.md`](../adr/0020-session-owned-worktrees.md) — the `.trusty-mpm-worktree` sentinel this spec extends
- [`docs/specs/DOC-52-shared-workstream-definition.md`](./DOC-52-shared-workstream-definition.md) — the cross-product glossary

**Cross-ref (code):** `crates/trusty-mpm/src/session_manager/record.rs:190-270`; `crates/trusty-mpm/src/driver/correlation.rs:33-43`; `crates/trusty-mpm/src/core/worktree_naming.rs:33-35`; `crates/trusty-mpm/src/core/session_launch/workstream_label.rs:93-136`; `crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs:349-418`; `crates/trusty-mpm/src/daemon/managed_routes/launch_on_main.rs:93-229`; `crates/trusty-mpm/src/project/worktree_policy.rs:84-106`; `crates/trusty-mpm/src/session_manager/manager.rs:1125`; `crates/trusty-mpm/src/session_manager/worktree_reclaim.rs:91-125`

**Scope.** This spec is design only. Nothing here is implemented. It states what a workstream is, where its identity is recorded, how it moves through states, what "active" means for the slot count, what happens to the main checkout at session start, and what to do about disk. It specifies no API, no route, no CLI verb, and no migration.

---

## 1. Data model {#SPEC-SESSWS-01~draft}

**ID:** SPEC-SESSWS-01~draft
**Status:** Draft

### 1.1 What exists today

No workstream identity exists. `SessionRecord` has no workstream field. `SessionCorrelation` carries one `worktree` and one `branch`, singular. The branch name is derived from the session name (`session/{name}`), as is the `ws/<session-name>` GitHub label. Every one of those is 1:1 by construction, not by choice.

### 1.2 The workstream record (NORMATIVE)

A **workstream** gains a record with the following identity. It is the ownership record ADR-0023 requires, and it answers *who owns this worktree*, never *does this worktree exist*.

| Field | Type | Meaning |
|---|---|---|
| `workstream_id` | opaque id | Durable, stable across everything. The key for addressing (ADR-0019), labels, claims, and reclamation. |
| `session_id` | owning session | The session that created it. Many workstreams share one value. |
| `branch` | string | The one branch. Cut from `origin/<default>` (§3). |
| `worktree_path` | path | The one worktree. Existence is git's answer, not this field's (ADR-0023). |
| `pr` | `Option<u64>` | The one PR, once opened. |
| `kind` | `Conversational \| Mechanical` | Slot arithmetic only (§4.4). Defaults to `Conversational`. |
| `stack_parent` | `Option<workstream_id>` | Set when this branch stacks on another workstream's branch (§4.3). |
| `state` | see §2 | |
| `created_at` / `closed_at` | timestamps | |

**Cardinality.** Session → workstream is 1:N. Workstream → branch, worktree, and PR are each 1:1. A workstream never spans repositories.

### 1.3 Where it is recorded (NORMATIVE)

- **On disk, per worktree:** the existing `.trusty-mpm-worktree` sentinel (ADR-0020) extends its JSON payload from `{owner_session_id, created_at}` to also carry `workstream_id` and, for a child worktree, `parent_workstream_id` (§5). The sentinel stays the per-worktree provenance carrier; ADR-0023 point 3 keeps it deliberately.
- **In the index:** the aggregate workstream record. Per ADR-0023 point 4 it MUST be rebuildable from the set of on-disk sentinels plus `git worktree list --porcelain`, with no other durable input. Every field in §1.2 that cannot be recovered from those two sources must therefore be written into the sentinel, or be re-derivable from git or the GitHub API (`pr`, `state`).
- **On the session record:** the session gains the set of workstream ids it owns. This is a convenience view over the index, not a second source of truth, and its loss is repaired by rebuilding the index.

**`SessionCorrelation`'s singular `worktree` and `branch` move to the workstream.** The session's correlation, where anything still needs one, is the correlation of its currently-focused workstream — computed, not stored. A stored copy would be a second source of truth and would drift.

### 1.4 The session record

`SessionRecord.workspace_path` becomes the project's **main checkout**, and it is written once and never repointed — the same write-once behaviour as today (`manager.rs:1125`), now pointing somewhere that never needs to change.

---

## 2. Lifecycle {#SPEC-SESSWS-02~draft}

**ID:** SPEC-SESSWS-02~draft
**Status:** Draft

### 2.1 Session states

```
Starting ──► Home ──► Ended
```

`Home` means: the session's tmux pane cwd is the main checkout. There is no other session state, because the session never moves.

### 2.2 There is no return-to-main transition, and there must not be one

The verified gap in today's code — `workspace_path` written once at spawn, no caller repointing it, the pane's OS cwd set at spawn and never `cd`-ed — is not a broken path to be repaired. It is the absence of a state that this model does not need.

**NORMATIVE: the session pane's cwd is the main checkout for the session's entire life and is never `cd`-ed.** "Which workstream am I on" is a pointer in a record, not a location of a process. Finishing one workstream and starting the next changes the pointer and creates a new worktree; it moves nothing.

Two consequences implementers must honour:

- **Every dispatch carries an explicit worktree path.** An agent working in a workstream is launched with that workstream's `worktree_path` as its cwd. Inheriting the parent's cwd would land it in the main checkout. `CLAUDE.md` already requires naming the worktree path in every dispatch prompt; under this model that requirement becomes load-bearing.
- **Every command the session itself runs against a workstream is explicitly scoped** (`git -C <path>`, `cargo --manifest-path`, or an equivalent). An unscoped command runs against the main checkout, which is inspection-only. This is the conventional-not-structural weakening ADR-0030 records as its largest risk; this spec does not close it.

### 2.3 Workstream states (NORMATIVE)

```
Open ──► Merged ──► Reclaimed
  │
  └────► Abandoned ──► Reclaimed
```

| State | Entered when | Holds a slot? | Holds a worktree? |
|---|---|---|---|
| `Open` | The branch and worktree are created | yes | yes |
| `Merged` | The PR's merge is observed | **no** | yes, until reclaimed |
| `Abandoned` | Explicitly closed without merging | no | yes, until reclaimed |
| `Reclaimed` | Worktree removed, branch deleted | no | no |

**The slot is freed on entry to `Merged`, not on `Reclaimed`.** This is the whole point of splitting the two states: reclamation is disk work that can lag arbitrarily, and a slow reviewer or a slow sweep must not starve the session's attention budget.

Reclamation stays owner-gated exactly as ADR-0020 and ADR-0023 specify, now keyed on `workstream_id` rather than `session_id`.

### 2.4 What triggers reclamation

Nothing is merge-triggered today: cleanup is idle- or age-triggered (`daemon/idle_reaper.rs`) or a manual `tm prune --merged-prs` sweep (`worktree_reclaim.rs:91-125`, which already models `Merged { pr }` and `Open { pr }` dispositions). Under this model, entering `Merged` is what queues reclamation. The existing age- and idle-triggered paths remain as a backstop for workstreams that never reach `Merged`.

---

## 3. Session start and the main checkout {#SPEC-SESSWS-03~draft}

**ID:** SPEC-SESSWS-03~draft
**Status:** Draft

### 3.1 Fetch, never pull (NORMATIVE)

At session start, against the main checkout:

1. **Always `git fetch`.** Unconditional. It mutates no working tree.
2. **Fast-forward the local default branch only if** the working tree is clean **and** `HEAD` is actually on that branch. Fast-forward only — `--ff-only`.
3. **Never** merge, rebase, `stash`, `checkout`, `reset`, or otherwise touch the tree. If the tree is dirty or on another branch, skip step 2 and say so.
4. **A failed fetch is not fatal.** The session starts.

### 3.2 Workstream branches come from `origin` regardless (NORMATIVE)

**Every workstream branch is cut from `origin/<default>`, whether or not the local fast-forward in §3.1 step 2 succeeded.** This is the clause that makes a failed fetch harmless: a dirty tree, a detached HEAD, or a network failure changes what the local default branch points at, and changes nothing about where new work starts from. Falling back to the local default branch is forbidden — stale local `main` has caused lost commits in this repo before.

If the fetch itself failed, branches are cut from the last successfully fetched `origin/<default>`, and the session is told the ref is stale. Stale-but-consistent beats fresh-but-local.

Open PR [#4958](https://github.com/bobmatnyc/trusty-tools/pull/4958) already implements the branch-from-fetched-origin half of this for the current model.

---

## 4. Slot semantics {#SPEC-SESSWS-04~draft}

**ID:** SPEC-SESSWS-04~draft
**Status:** Draft

### 4.1 What the cap is for

The limit exists because each workstream's conversation bubbles up to the user, and more than about five of them exceeds what one person can attend to. It is an **attention** limit.

**It is not WIP reduction.** Prior analysis in this repo examined and refuted the throughput framing — the bottleneck measured there was signals detected then ignored, not work in flight. Any future justification of this cap as throughput control is a regression to a rejected argument.

### 4.2 "Active" is defined, so the count can go down (NORMATIVE)

> A workstream is **active** when its branch is unmerged **and** its worktree still exists.

Both clauses are load-bearing. Without the first, the count only ever ratchets upward. Without the second, a reclaimed-but-unmerged workstream holds a slot forever.

Merge frees the slot (§2.3). Reclamation does not — by then the slot is already free.

### 4.3 A stack counts as one

A stacked series is one outcome and one conversation. Workstreams linked by `stack_parent` count as **one** active workstream, attributed to the root. Counting a stack of three as three would penalize the pattern the repo's own PR workflow prefers.

### 4.4 Mechanical workstreams do not count

A workstream marked `kind: Mechanical` — dependabot bumps, changelog-only edits, CI-config changes — generates no conversation and consumes no slot. `Conversational` is the default; `Mechanical` is declared explicitly at creation. Automatic classification (by PR author, by diff shape) is **not** specified here; see §7.

### 4.5 Agent worktrees do not count

Covered in §5. They are children, not workstreams.

### 4.6 The cap is advisory (NORMATIVE)

- Exceeding the limit produces a **nudge**: the user is encouraged to start a new session. Nothing is refused, delayed, or queued.
- A hard block would strand someone mid-task, which is worse than the crowding it prevents.
- The number is **configurable**, not a constant in the source. `5` is the default.

**Relationship to DOC-52 §5.1.** DOC-52 §5.1 specifies a *different* cap: repo-size-scaled, resource-driven, and refused-by-default with a `--force` override. That cap counts open workstreams for disk and merge-ordering reasons. This one counts active workstreams for attention reasons, and never refuses. They are two limits with two rationales; this spec does not withdraw DOC-52 §5.1, and whether both should survive is §7's question.

---

## 5. Agent worktrees as children {#SPEC-SESSWS-05~draft}

**ID:** SPEC-SESSWS-05~draft
**Status:** Draft

Parallel engineers inside one workstream legitimately need their own checkouts — one observed leg ran seven. These are created today by agents running plain `git worktree add` per the `CLAUDE.md` convention, and they appear in no registry; `session_manager::worktree_nested` notices them only to avoid deleting them.

**NORMATIVE:**

- An agent worktree is a **child** of the workstream that dispatched the agent. Its sentinel carries `parent_workstream_id`.
- It is **reaped when the agent exits**. Its lifetime is the agent's, not the workstream's and not the session's.
- It **never consumes a slot** (§4.5) and never appears in the session's workstream set.
- A child whose parent workstream reaches `Reclaimed` is reclaimed with it, whether or not its agent exited cleanly.

Unreaped children are the documented source of worktree sprawl. Giving them a parent is what makes them reapable at all: today nothing owns them, so nothing may delete them (ADR-0020's fail-closed rule correctly refuses to touch an owner-unknown worktree).

---

## 6. Disk {#SPEC-SESSWS-06~draft}

**ID:** SPEC-SESSWS-06~draft
**Status:** Draft

### 6.1 The problem, stated with numbers

Each worktree is a full checkout with its own `target/`. A measured example on this machine: **300 GB across ten worktrees, one `target/` alone at 88 GB.** DOC-52 §5.1 quotes ~14 GB as the average. Five workstreams per session, across several sessions, is terabytes.

**Without a shared-build strategy, the cap of five is a disk cap, not an attention cap.** Whichever number disk permits would become the real limit, and §4.1's attention rationale would be false advertising for a resource constraint.

### 6.2 Options, with their actual costs

| Option | Saves | Costs |
|---|---|---|
| **Shared `CARGO_TARGET_DIR`** per project (e.g. `~/.trusty-mpm/target/<project-slug>`) | The whole per-worktree `target/`, which is nearly all of it | Cargo takes a file lock on the target directory, so concurrent builds across worktrees **serialize** rather than run in parallel. Correctness is unaffected; wall-clock is. |
| **sccache** | Recompilation of unchanged crates across worktrees | The `target/` directories still exist per worktree, so the disk saving is partial. No lock contention. |
| **Neither** | — | Terabytes. The cap becomes a disk cap (§6.1). |

### 6.3 What this spec commits to

- **Doing nothing is not an option.** §6.1's arithmetic is the reason.
- **sccache is the default**, because it costs no build parallelism, and the model's whole point is several workstreams progressing at once.
- **Shared `CARGO_TARGET_DIR` is an opt-in** for disk-constrained machines, where serialized builds beat running out of space.
- The serialization cost of the shared-target option **has not been measured here**, and the choice above is a reasoned default, not a measured one. Measure before mandating it for everyone.

This repo already carries a `rust-build-performance` skill covering sccache across worktrees; the mechanism is not novel, only its application to this model is.

---

## 7. Open questions

Written as questions because they are not answered, not as placeholders for answers that exist elsewhere.

1. **Existing 1:1 sessions.** A live session's pane cwd is a worktree, and no record edit moves an OS process. Such a session cannot become 1:N in place. Is the answer "finish it under the old model and never migrate", or is there a relaunch path that preserves the conversation? Not answered here.
2. **The `ws/<session-name>` label and DOC-53 claim drawers.** Both derive workstream identity from the session name. Under 1:N that derivation is invalid. Do they re-key on `workstream_id`, and what happens to labels already on merged and open PRs?
3. **Two caps or one.** Does DOC-52 §5.1's blocking, repo-size-scaled cap coexist with §4's advisory attention cap, or does one withdraw? If a shared build directory removes most of the disk pressure (§6), §5.1's rationale weakens considerably.
4. **Who declares `Mechanical`.** §4.4 requires an explicit declaration at creation. Whether that can be inferred safely — by PR author for bots, by diff shape for changelog-only changes — is unspecified, and a wrong inference silently under-counts the user's real attention load.
5. **Concurrent fast-forward.** Two sessions in the same project both run §3.1 against the same main checkout. Two concurrent `--ff-only` updates of the same ref is a race. Does the second one skip, wait, or is the refresh moved somewhere serialized?
6. **The `worktree_policy` toggle and `launch_on_main`.** With the main checkout as every session's home, does the per-project worktree toggle (`worktree_policy.rs:84-106`) and the opt-out landing path (`launch_on_main.rs:93-229`) still mean anything, or do they collapse into the default?
7. **Enforcing read-only.** ADR-0030 accepts that the main checkout's read-only-ness becomes a convention. Is there a mechanism — a git hook, a filesystem guard, a pre-command check on the session's own tool calls — that restores it as a fact? None is proposed here.

---

## 8. Corrections owed to other documents

Listed, not performed — each is a separate, reviewable edit.

| Document | What needs correcting |
|---|---|
| **DOC-52 §1.5, §3.1** | State the trusty-mpm 1:1 binding as re-scoped by ADR-0030, not as permanent and sanctioned. The instruction that "no ticket should be filed to reconcile trusty-mpm with the canonical cardinality" is now wrong. |
| **DOC-53** | Its `ws:<name>` identity is justified by the DOC-52 §1.5 exception; that justification is gone (§7 question 2). |
| **`bin/tm/commands/launch.rs:86-87`** | The doc comment states "the live checkout is NEVER touched … the tmux cwd is the managed clone (#1590)". Under ADR-0030 the tmux cwd is the main checkout, and the no-writes guarantee is carried by policy instead. |

---

## 9. Non-goals

- No API, HTTP route, MCP tool, CLI verb, or storage format is specified.
- No migration is specified (§7 question 1).
- No implementation sequencing, phasing, or effort estimate.
- Nothing about trusty-code's or trusty-agents' workstream models. DOC-52 governs the cross-product vocabulary; this spec governs trusty-mpm's own model only.
