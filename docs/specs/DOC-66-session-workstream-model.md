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
**Spec ID:** `SPEC-SESSWS-01~draft` … `SPEC-SESSWS-07~draft` (DOC-66)
**Builds on:**
- [`docs/adr/0030-sessions-own-many-workstreams-from-the-tm-checkout.md`](../adr/0030-sessions-own-many-workstreams-from-the-tm-checkout.md) — the decision this spec encodes
- [`docs/adr/0023-worktree-authority-existence-vs-ownership.md`](../adr/0023-worktree-authority-existence-vs-ownership.md) — git owns existence; a rebuildable record owns ownership
- [`docs/adr/0020-session-owned-worktrees.md`](../adr/0020-session-owned-worktrees.md) — the `.trusty-mpm-worktree` sentinel this spec extends
- [`docs/adr/0036-all-worktrees-are-siblings-under-claude-worktrees.md`](../adr/0036-all-worktrees-are-siblings-under-claude-worktrees.md) — every worktree is a flat sibling under `.claude/worktrees/`; §5's parent/child relation is a recorded id, never containment
- [`docs/specs/DOC-52-shared-workstream-definition.md`](./DOC-52-shared-workstream-definition.md) — the cross-product glossary

**Cross-ref (code):** `crates/trusty-mpm/src/session_manager/record.rs:190-270`; `crates/trusty-mpm/src/driver/correlation.rs:33-43`; `crates/trusty-mpm/src/core/worktree_naming.rs:33-35`; `crates/trusty-mpm/src/core/session_launch/workstream_label.rs:93-136`; `crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs:349-418`; `crates/trusty-mpm/src/daemon/managed_routes/launch_on_main.rs:64-69,93-229`; `crates/trusty-mpm/src/daemon/managed_routes/inproject.rs:56,134-136`; `crates/trusty-mpm/src/daemon/managed_routes/inproject_hygiene.rs:291-303,405-437`; `crates/trusty-mpm/src/daemon/mod.rs:149`; `crates/trusty-mpm/src/core/harness_root.rs:47-59`; `crates/trusty-mpm/src/provisioner/workspace.rs:13-22`; `crates/trusty-mpm/src/project/record.rs:165-177`; `crates/trusty-mpm/src/project/worktree_policy.rs:84-106`; `crates/trusty-mpm/src/session_manager/manager.rs:1125`; `crates/trusty-mpm/src/session_manager/worktree_reclaim.rs:91-125`

**Scope.** This spec is design only. Nothing here is implemented. It states what a workstream is, where its identity is recorded, how it moves through states, what "active" means for the slot count, what happens to the tm checkout at session start, and what to do about disk. It specifies no API, no route, no CLI verb, and no migration.

---

## 0. The model, and what the tm checkout is {#SPEC-SESSWS-07~draft}

**ID:** SPEC-SESSWS-07~draft
**Status:** Draft

### 0.1 The model in four statements (NORMATIVE)

1. **Launch target.** Launching tm from a repo sets the session's home to the **tm checkout** — the tm-created directory — never a per-session worktree.
2. **Update triggers.** The tm checkout is refreshed on **every new session** and on **session restoration** (exit and re-enter).
3. **Update semantics.** "Refreshed" means fetch plus fast-forward-only. Never `git pull` into a dirty tree, never `reset --hard`, never `clean` (§3.3).
4. **Work location.** By default the user works in a worktree and branch — a workstream. A flag overrides this to work directly on the default branch in the tm checkout (§3.5).

### 0.2 The tm checkout

**The tm checkout is the directory tm creates** at `repos_root().join(owner).join(repo)` — by default `~/trusty-mpm-projects/<owner>/<repo>` (`inproject.rs:134-136`, `DEFAULT_REPOS_DIR` at `:56`).

It is **not the user's own clone** of the repo, wherever that lives. This spec says "the tm checkout" for the former and "the user's own clone" for the latter, and never says "main checkout" for either — that phrase was read both ways and cost a round of wrong analysis.

Since #4270 the tm checkout *is* the base checkout, with worktrees at `<base>/.worktrees/<name>` (`harness_root.rs:47-59`, `provisioner/workspace.rs:13-22`). The pre-#4270 layout put a bare clone at `<project>/.base` and added worktrees from there; existing `.base` stores were deliberately not migrated. Retiring `.base` finishes that convergence and leaves exactly one tm checkout per project, which is what makes this spec's single-home model coherent rather than aspirational.

### 0.3 Managed, not protected (NORMATIVE)

> **Managed** means tm maintains tm-specific configuration inside the directory — the relocated `CLAUDE_CONFIG_DIR`, deployed agents and skills, compiled instructions, statusline and settings wiring, `.trusty-mpm/` state. It serves one boundary in two directions: **outward**, tm's configuration does not leak into a repo where someone runs plain `claude` (the precedent is the defect where `tm install` wired tm hooks into every project's `.claude/settings.json`); **inward**, it is where the *user's own* tm-specific customization lives. `CLAUDE_CONFIG_DIR` relocates the entire `~/.claude` tree wholesale — skills, agents, settings, plugins, session history — rather than layering over it, and that non-standard relocation is exactly what keeps tm's customization from stepping on native Claude Code's (DOC-34).

It does **not** mean access-controlled, tm-exclusive, or off-limits to the user — and the inward purpose is why. The directory holds *the user's* configuration, which gives them a standing, legitimate reason to open, read, and edit it: with an editor, with `tm`, or by hand. A directory carrying someone's own config cannot coherently be tm-exclusive. **"Managed" describes what tm maintains there, never who is allowed in.**

Inherited doc comments use the older word — `inproject.rs:5-7` describes "a durable PROTECTED base clone … owned by the daemon". Quoted accurately here; this spec does not adopt "protected" as its own term, because the access-control reading it invites is wrong.

### 0.4 Shared with the user and external tools (NORMATIVE)

A user may point Obsidian, Cursor, an IDE, or any other tool at the tm checkout and work there while a session runs. Four consequences bind every tm operation on it:

- **No exclusive ownership.** tm shares the directory with the user and with arbitrary external tools.
- **No clean-tree assumption.** Uncommitted and untracked content is the **expected steady state**, not an anomaly. A user may have an editor open there with unsaved work at the moment any tm operation runs.
- **No reclamation of unexpected files.** Files tm did not create are not garbage. Nothing there may be swept, pruned, or deleted on the grounds that tm does not recognize it.
- **Safe under concurrent external modification.** Any tm operation must be correct while a human or an editor is writing to the directory.

git recovers anything committed. It recovers **nothing** uncommitted or untracked, which is precisely the content this section exists to protect. Every requirement in §3.3 follows from here.

### 0.5 The "inspection-only" rule does not carry over (NORMATIVE)

`CLAUDE.md`'s rule that the main checkout is inspection-only was written about **the user's own clone**, under a topology where sessions lived elsewhere. It does not apply to the tm checkout, which is where sessions are now meant to live and write.

Stated so nobody re-imports the wrong constraint later: the tm checkout is not read-only, and read-only-ness is not a property this model wants, needs, or should try to enforce for it. The rule about the user's own clone is unaffected and still holds.

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
| `branch` | string | The one branch. Cut from `origin/<default>` (§3.4). |
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

`SessionRecord.workspace_path` becomes the **tm checkout**, written once and never repointed — the same write-once behaviour as today (`manager.rs:1125`), now pointing somewhere that never needs to change.

---

## 2. Lifecycle {#SPEC-SESSWS-02~draft}

**ID:** SPEC-SESSWS-02~draft
**Status:** Draft

### 2.1 Session states

```
Starting ──► Home ──► Ended
```

`Home` means: the session's tmux pane cwd is the tm checkout. There is no other session state, because the session never moves.

### 2.2 There is no return-to-the-checkout transition, and there must not be one

The verified gap in today's code — `workspace_path` written once at spawn, no caller repointing it, the pane's OS cwd set at spawn and never `cd`-ed — is not a broken path to be repaired. It is the absence of a state this model does not need.

**NORMATIVE: the session pane's cwd is the tm checkout for the session's entire life and is never `cd`-ed.** "Which workstream am I on" is a pointer in a record, not the location of a process. Finishing one workstream and starting the next changes the pointer and creates a new worktree; it moves nothing.

Two consequences implementers must honour:

- **Every dispatch carries an explicit worktree path.** An agent working in a workstream is launched with that workstream's `worktree_path` as its cwd. Inheriting the parent's cwd would land it in the tm checkout. `CLAUDE.md` already requires naming the worktree path in every dispatch prompt; under this model that requirement becomes load-bearing.
- **Every command the session itself runs against a workstream is explicitly scoped** (`git -C <path>`, `cargo --manifest-path`, or an equivalent). An unscoped command runs against the tm checkout. That is not a safety failure — the tm checkout is a legitimate place to write (§0.5) — but it is a correctness failure, because the command then reports on the wrong tree.

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

## 3. Refreshing the tm checkout {#SPEC-SESSWS-03~draft}

**ID:** SPEC-SESSWS-03~draft
**Status:** Draft

### 3.1 The defect: fetch is tied to daemon lifetime

Something *does* fetch the tm checkout today. `inproject_hygiene::run_hygiene_for_base` runs `git -C <base> fetch origin` plus a safety-gated `reset --hard` (`inproject_hygiene.rs:291-303`) — but only via `run_hygiene_for_all_bases` (`:405-437`), whose single non-test caller is daemon startup (`daemon/mod.rs:149`).

So the accurate defect is not "nothing ever fetches." It is that **fetch is tied to daemon lifetime rather than to worktree creation.** A daemon up for weeks serves branches cut from whatever the base was at boot. That is the reported 1.3.4 failure: the reporter's base froze at 2026-07-18 and every session since inherited it.

Widening the trigger (§3.2) is the fix for that class of bug.

### 3.2 Update triggers (NORMATIVE)

The tm checkout is refreshed at **every new session** and at **session restoration** — exit and re-enter. This is wider than today's daemon-startup-only sweep, deliberately.

### 3.3 Update semantics: non-destructive (NORMATIVE)

Against the tm checkout:

1. **Always `git fetch`.** Unconditional. It mutates no working tree.
2. **Fast-forward the default branch only if** the working tree is clean **and** `HEAD` is actually on that branch. Fast-forward only — `--ff-only`.
3. **Never** `git pull` into a dirty tree, `reset --hard`, `clean`, `checkout -f`, `stash`, merge, or rebase. If the tree is dirty or on another branch, skip step 2 and say so.
4. **A failed fetch is not fatal.** The session starts.

**The reason is specific, not generic caution.** A user may have Cursor or Obsidian open in the tm checkout holding uncommitted work (§0.4). Any operation that discards local state is therefore a data-loss path, not a hygiene step.

### 3.4 The start point is explicit, which is what makes §3.3 affordable (NORMATIVE)

**Every workstream branch is cut from `origin/<default>` explicitly, whether or not the fast-forward in §3.3 step 2 happened.**

This is the crux of the whole section, so it is stated as a split rather than left implicit:

> **The refresh is for the human reading the directory. The explicit start point is for correctness.**

Because the start point never consults the local ref, a skipped or failed fast-forward costs nothing. Correctness does not depend on the local ref moving, which is exactly what lets §3.3 be as conservative as a shared directory requires. Falling back to the local default branch is forbidden — stale local `main` has caused lost commits in this repo before.

If the fetch itself failed, branches are cut from the last successfully fetched `origin/<default>` and the session is told the ref is stale. Stale-but-consistent beats fresh-but-local.

Open PR [#4958](https://github.com/bobmatnyc/trusty-tools/pull/4958) moves the fetch to the moment the branch is cut, which is the correct layer for it.

### 3.5 Working on the default branch instead: the override already exists

**This is not new work.** Setting `worktree: false` for a project in `projects.json` (`project/record.rs:165-177`) routes through `worktree_policy::worktree_enabled_in` (`worktree_policy.rs:84-106`) to `spawn_managed_on_main` (`launch_on_main.rs:93-229`), which by its own doc comment "never clones a base checkout or adds a worktree; the session's cwd/workspace IS `local_path`" (`launch_on_main.rs:64-69`). It is a normal managed session in every other respect. Absent the key, `worktree_enabled()` defaults to `true`.

**The delta this model wants:**

| Today | Wanted |
|---|---|
| A per-project config key in `projects.json` | A documented flag, overridable per session |
| Reached only when the spawn is detected as a local workdir, landing at the caller-supplied `local_path` | Lands on the default branch **in the tm checkout** |

Only the second row is a behavioural change, and it is small: the target becomes the tm checkout rather than whatever path the caller passed.

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

## 5. Agent worktrees are children by record, not by location {#SPEC-SESSWS-05~draft}

**ID:** SPEC-SESSWS-05~draft
**Status:** Draft

Parallel engineers inside one workstream legitimately need their own checkouts — one observed leg ran seven. These are created today by agents running plain `git worktree add` per the `CLAUDE.md` convention, and they appear in no registry; `session_manager::worktree_nested` notices them only to avoid deleting them.

**NORMATIVE:**

- An agent worktree is a **child** of the workstream that dispatched the agent. "Child" is a **recorded parent id and nothing more**: its sentinel carries `parent_workstream_id`, naming the dispatching workstream. It is a statement about lifetime and reclamation authority, never about where the directory sits.
- **A child is not contained by its parent.** It is a flat sibling under `.claude/worktrees/`, alongside every other worktree, per [ADR-0036](../adr/0036-all-worktrees-are-siblings-under-claude-worktrees.md). A worktree nested inside another worktree's checkout is the defect ADR-0036 exists to prevent — the parent's dirty-tree report cannot see a gitignored worktree living inside it — so parentage must be resolved by reading `parent_workstream_id`, never by inspecting a path prefix.
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

### 6.2 Three strategies, with their actual costs

`target/` is essentially the entire disk cost — source checkouts are noise next to an 88 GB build directory. All three strategies below therefore aim at `target/`, and they **compose**: reaping is orthogonal to how the surviving builds are cached, so none of this is either/or.

| Option | Saves | Costs |
|---|---|---|
| **Idle `target/` reap** — delete `target/` in a worktree untouched for N days | The full mass, for every idle workstream | A cold rebuild on the next touch of a workstream nobody was using. Nothing while a workstream is active. |
| **sccache** | Recompilation of unchanged crates across worktrees | Hashing and serialization overhead on every build. The `target/` directories still exist per worktree, so the disk saving is partial. No lock contention. |
| **Shared `CARGO_TARGET_DIR`** per project (e.g. `~/.trusty-mpm/target/<project-slug>`) | The whole per-worktree `target/` | Cargo locks the target directory, so concurrent builds across worktrees **serialize**. Correctness is unaffected; wall-clock is — and that is the wrong trade under a five-workstream cap whose point is parallel progress. |
| **None of them** | — | Terabytes. The cap becomes a disk cap (§6.1). |

### 6.3 Idle `target/` reap (NORMATIVE)

Build output is disposable and fully regenerable, which is what makes this the cheapest of the three: it costs no CPU while a workstream is active and no parallelism ever.

- **Reap `target/` only.** Never source, never `.git`, never untracked non-build content. A general "unused files" sweep in a directory shared with the user and external editors (§0.4) is a data-loss path, not cleanup — the same class of mistake as the hygiene finding in §8.
- **Never reap a worktree with a build in progress.** Staleness is judged by the worktree's mtime, or by the last session activity recorded for its workstream. The check MUST be conservative: when in doubt, skip.
- **The threshold is configurable.** Reaping is reversible in the only sense that matters — the next build regenerates the directory.
- **Reaping does not touch the workstream's lifecycle.** It MUST NOT free a cap slot (§4.2), mark the workstream closed, or otherwise imply work ended. Disk reclamation and workstream lifecycle are independent axes; conflating them would silently retire live work.

### 6.4 What this spec commits to

- **Doing nothing is not an option.** §6.1's arithmetic is the reason.
- **Idle reap first.** It needs no build-system change at all, costs nothing while work is active, and targets the actual mass.
- **sccache next**, for the recompilation cost that reaping makes more frequent rather than less.
- **Shared `CARGO_TARGET_DIR` last, opt-in only**, for disk-constrained machines where serialized builds beat running out of space.
- **None of the three has been benchmarked on this repo.** The ordering above is reasoned from what each one costs, not measured. Measure before mandating any of them for everyone.

This repo already carries a `rust-build-performance` skill covering sccache across worktrees; the mechanism is not novel, only its application to this model is.

---

## 7. Open questions

Written as questions because they are not answered, not as placeholders for answers that exist elsewhere.

1. **Existing 1:1 sessions — the largest open risk in the model.** A live session's pane cwd is a worktree, and no record edit moves an OS process. Such a session cannot become 1:N in place. Is the answer "finish it under the old model and never migrate", or is there a relaunch path that preserves the conversation? Not answered here.
2. **The `ws/<session-name>` label and DOC-53 claim drawers.** Both derive workstream identity from the session name. Under 1:N that derivation is invalid. Do they re-key on `workstream_id`, and what happens to labels already on merged and open PRs?
3. **Two caps or one.** Does DOC-52 §5.1's blocking, repo-size-scaled cap coexist with §4's advisory attention cap, or does one withdraw? If a shared build directory removes most of the disk pressure (§6), §5.1's rationale weakens considerably.
4. **Who declares `Mechanical`.** §4.4 requires an explicit declaration at creation. Whether that can be inferred safely — by PR author for bots, by diff shape for changelog-only changes — is unspecified, and a wrong inference silently under-counts the user's real attention load.
5. **Concurrent refresh.** Two sessions in the same project both run §3.3 against the same tm checkout. Two concurrent `--ff-only` updates of the same ref is a race. Does the second one skip, wait, or is the refresh moved somewhere serialized? Widening the trigger to every new session and restore (§3.2) makes this more likely, not less.
6. **The `worktree_policy` toggle's future shape.** §3.5 specifies the delta from per-project key to per-session flag. Whether the per-project key remains as a default, and whether `launch_on_main`'s local-workdir entry condition survives, is not decided.

---

## 8. Risk flagged for the implementer

**The startup hygiene `reset --hard` is a confirmed data-loss path, and a fix is in flight.** `inproject_hygiene::run_hygiene_for_base` runs a safety-gated `reset --hard` against the tm checkout at daemon startup (`inproject_hygiene.rs:291-303`, swept by `run_hygiene_for_all_bases` at `:405-437`). A separate audit reproduced the failure: the gate's `is_dirty` check uses `git status --porcelain` **without `--ignored`**, so gitignored content is invisible to it and `reset --hard` silently overwrites such content where it collides with a tracked path.

This is the exact hazard §0.4 and §3.3 exist to prevent, confirmed rather than suspected. The fix is on a separate branch and is **not** specified here.

---

## 9. Corrections owed to other documents

Listed, not performed — each is a separate, reviewable edit.

| Document | What needs correcting |
|---|---|
| **DOC-52 §1.5, §3.1** | State the trusty-mpm 1:1 binding as re-scoped by ADR-0030, not as permanent and sanctioned. The instruction that "no ticket should be filed to reconcile trusty-mpm with the canonical cardinality" is now wrong. |
| **DOC-53** | Its `ws:<name>` identity is justified by the DOC-52 §1.5 exception; that justification is gone (§7 question 2). |
| **`bin/tm/commands/launch.rs:86-87`** | The doc comment states "the live checkout is NEVER touched … the tmux cwd is the managed clone (#1590)". Under ADR-0030 the tmux cwd is the tm checkout itself rather than a worktree under it. The user's-own-clone guarantee the comment describes is unchanged. |

---

## 10. Non-goals

- No API, HTTP route, MCP tool, CLI verb, or storage format is specified.
- No migration is specified (§7 question 1).
- No implementation sequencing, phasing, or effort estimate.
- No assessment or fix of the hygiene reset gate (§8).
- Nothing about trusty-code's or trusty-agents' workstream models. DOC-52 governs the cross-product vocabulary; this spec governs trusty-mpm's own model only.
