# DOC-56 — Agent Configuration Sync: The `trusty-agents-agents` Private Monorepo

**Status:** Draft — §5.3 requires owner sign-off before implementation  
**Subsystem:** trusty-agents — agent configuration lifecycle, provisioning, multi-machine sync  
**Owner:** Engineering (trusty-agents) / Bob Matsuoka  
**Last-updated:** 2026-07-25  
**Spec ID:** `SPEC-AGENTSYNC-01~draft` … `SPEC-AGENTSYNC-07~draft` (DOC-56)  
**Builds on:** #3837 (git-backed per-agent content store — the local repo already exists); DOC-54 [Trusty Agents Product Specification](./trusty-agents-product-spec.md) §3.2/§3.3; #3816 (declarative templates)  
**Supersedes:** DOC-54 §3.3 — see §2.4  
**Subsumes:** #3844 (reprovision clobber) — see §6  
**Implements:** #3899 (tagent-native sync of agent config to the monorepo)

---

## 1. Executive Summary

Bob, 2026-07-25: *"Create a private repository `bobmatnyc/trusty-agents-agents`
where we sync the agent configuration and store into a monorepo under
`agents/<agent>`. This is a platform feature."*

Three things make this more than a backup scheme:

1. **The local half already exists.** #3837 initialized `~/.trusty-agents` as a
   git repo on `main`, with a `.gitignore` that already excludes secrets,
   backups, ephemeral runtime state, and regenerable index artifacts. Ninety
   objects, 260 KiB packed, 34 tracked files under `agents/`. This spec adds the
   *remote* half and the *merge* half; it does not re-litigate that baseline.
2. **It fixes a live bug class, not just a convenience gap.** `#3844` — the
   reprovision path clobbers hand-edits every `cargo install` — is unfixable in
   its current framing because the mechanism has **no merge base**. It compares
   two things (embedded bundle, live file) and can therefore only choose one.
   Once configuration lives in git with the bundled template as a tracked
   ancestor, the third input exists and the fix becomes an ordinary three-way
   merge. §6 makes this the design's centrepiece rather than a side effect.
3. **It contradicts a checked-in spec, deliberately.** DOC-54 §3.3 states agents
   are "never version-controlled as repository artifacts." §2.4 records the
   supersession explicitly so the two documents do not silently disagree.

**Open for owner sign-off (§5.3):** whether OKG knowledge trees sync alongside
configuration. Both options are specified with their real size and growth
numbers; the recommendation is **config-only in the monorepo, knowledge trees in
separate per-store repos**, because DOC-55 (the universal importer) makes the
corpus's growth profile unbounded while configuration's stays flat.

---

## 2. SPEC-AGENTSYNC-01 — The Repository {#SPEC-AGENTSYNC-01~draft}

### 2.1 Identity

| Property | Value | Rationale |
|---|---|---|
| Name | `bobmatnyc/trusty-agents-agents` | Owner-specified |
| Visibility | **Private**, permanently | Personas encode working style, contacts, and organizational context. Even with secrets excluded (§5.1) the content is personal. A public agent-config repo is a social-engineering corpus. |
| Default branch | `main` | Matches the #3837 local baseline |
| Shape | Monorepo, one directory per agent | Owner-specified; §3 |

### 2.2 Relationship to the #3837 local repo

`~/.trusty-agents` is already a git repo containing more than agents:
`config.toml`, `projects.json`, `user.toml`, `skills/index.json`, `events/`,
`sessions/`, `repl_history.txt`. The monorepo is **not** simply that repo pushed.

**Recommended model — the content store is the working copy; the monorepo is its
remote, filtered by `.gitignore`.** That is: add `origin` to the existing
`~/.trusty-agents` repo, extend `.gitignore` to exclude the per-machine and
ephemeral files §5.1 lists, and the pushed tree *is* the monorepo. Layout
`agents/<agent>/` then already holds.

The rejected alternative — a separate repo synchronized by a copy/export step —
was rejected because it introduces a second source of truth, a bidirectional
copier, and its own conflict surface, to solve a problem `.gitignore` already
solves. It also throws away the #3837 history.

> **State of the world, 2026-07-25.** The repo now exists and carries an
> **initial one-off manual sync** (#3899): `ctrl`, `izzie`, `cto-assistant`, and
> `assistant`, each as `agents/<agent>/{agent.toml, persona.md, events/*.md}`
> plus a `<agent>.toml.shadow` copy of the flat file where one existed, with a
> root `README.md`. That pass was a **copy/export** from `~/.trusty-agents/agents/`
> — the local #3837 repo was left untouched and nothing was pushed from it.
>
> That is the right call for a one-off (it got the content off a single machine
> today) but it is the shape this section rejects for the *platform* feature.
> Implementation should convert the remote into the working copy's origin rather
> than perpetuate the copier. Two concrete follow-ons: the `.toml.shadow` files
> are the flat/package ambiguity §3.2 requires normalizing away, not a durable
> layout; and `user.toml`, excluded from the manual pass, is proposed for
> inclusion here (§4.1) because a restore without it is not a restore — see Q7.

> **PII posture.** #3899 records that several personas deliberately embed real
> personal data (name, personal email, employer, coworkers, home region) as part
> of how those assistants are instructed to behave. That is by design for named
> per-user assistants and is the reason §2.1 fixes visibility at **private,
> permanently**. It is also why §5.1's secret gate blocks rather than redacts:
> the content is *supposed* to be personal, so the gate's job is credentials, and
> the repo's job is never being public.

**Consequence to accept explicitly:** the monorepo will contain a small amount of
non-agent platform config (`config.toml`, `user.toml`, `skills/index.json`). That
is correct — those files are part of what makes a machine's agents work, and a
restore that omits them is not a restore. `projects.json`, `events/`, and
`sessions/` are per-machine and are excluded (§5.1).

### 2.3 Why this is a platform feature, not an ops script

Stated as a normative requirement because it determines the whole design: **the
sync must be reachable from `tagent` itself** (§7), must run without the user
knowing git, and must be safe to invoke automatically. A documented
`git push` runbook is not this feature. The user-visible contract is "my agents
follow me between machines, and my edits are never lost" — a runbook delivers
neither half.

### 2.4 Supersession of DOC-54 §3.3 (normative)

DOC-54 §3.3 currently reads:

> Agent packages live in the user's local agent directory (e.g.,
> `~/.trusty-agents/agents/`), **not** in the repository. They are user-specific,
> account-specific… agents are never version-controlled as repository artifacts.

**That paragraph is superseded by this spec**, with one distinction preserved
intact: agent packages are still never committed to **`bobmatnyc/trusty-tools`**
(the product repository). The prohibition was always about not shipping a user's
personal agents inside the product source tree, and that remains absolute. What
changes is that agent packages ARE version-controlled — in the user's own private
content repository.

The `backup-on-change` dated-snapshot convention DOC-54 §3.3 describes is
likewise superseded by git-as-undo (#3837 already applied this: `backups/` is
gitignored and Concierge's persona documents the new convention). Implementation
must land a DOC-54 §3.3 amendment in the same wave, or the catalog carries two
contradictory normative statements.

---

## 3. SPEC-AGENTSYNC-02 — Layout {#SPEC-AGENTSYNC-02~draft}

### 3.1 Target layout

```
trusty-agents-agents/               (== ~/.trusty-agents, filtered)
  README.md                         # what this repo is, how to restore
  .gitignore                        # the §5.1 exclusion list
  config.toml                       # platform config (secrets excluded, §5.1)
  user.toml                         # owner identity/profile
  skills/index.json                 # skill catalog pins
  agents/
    <agent>/
      agent.toml                    # the manifest: [[stores]] / [tools] / [[listeners]]
      persona.md                    # main instructions + frontmatter
      events/                       # per-connector event instructions (DOC-54 §6.1)
        gmail.md
        calendar.md
      attachments/                  # optional, per #3837(a)
  .provisioning/
    bundle-stamp                    # which bundled-template generation main descends from (§6)
```

### 3.2 The flat/package migration (blocking prerequisite)

The live tree is **mixed**: `agents/izzie.toml` *and* `agents/izzie/agent.toml`
both exist, for `izzie`, `cto-assistant`, and `ctrl`. It also carries
provisioning debris that must not be synced: `*.lock`, `*.stale.bak`,
`*.stale.bak.lock`, `*.bak-20260723`, `*.bak-20260724-pre-smoketest-fix`.

Before the first push, the tree must be normalized to strict directory packages
(#3837(a)) with `git mv`, preserving history. Pushing the mixed shape first would
bake the ambiguity into the remote and into every clone.

Debris classes and disposition:

| Pattern | Disposition |
|---|---|
| `*.lock` | Never tracked — runtime lockfiles (already excluded by `.gitignore`'s `*.lock`) |
| `*.stale.bak`, `*.stale.bak.*` | Never tracked — already excluded; obsoleted entirely by §6 |
| `*.bak-<date>*` | Never tracked — already excluded; git is the undo mechanism |
| `agents/<name>.toml` flat shadow | Migrated into `agents/<name>/agent.toml`, flat file removed |

### 3.3 One agent per directory, no exceptions

`agents/<agent>/` is the unit of everything downstream: the unit of merge (§6),
the unit of selective sync, and the unit a future "share this agent" feature
would operate on. An agent whose config is split across a flat file and a package
directory has no well-defined merge unit, which is precisely how #3844's
regressions became hard to reason about.

---

## 4. SPEC-AGENTSYNC-03 — What Syncs {#SPEC-AGENTSYNC-03~draft}

### 4.1 Syncs

| Path | Why |
|---|---|
| `agents/<agent>/agent.toml` | The manifest — the thing being synced |
| `agents/<agent>/persona.md` | Instructions; the highest-value hand-edited artifact |
| `agents/<agent>/events/*.md` | Per-connector instructions (DOC-54 §6.1) |
| `agents/<agent>/attachments/**` | Small, agent-owned reference material (subject to a size gate, §5.2) |
| `config.toml` | Platform config, minus any secret-bearing key (§5.1) |
| `user.toml` | Owner identity/profile — small, stable, needed for a restore |
| `skills/index.json` | Skill catalog pins |
| `.provisioning/bundle-stamp` | The merge-base marker (§6) |

### 4.2 Never syncs (normative)

| Path / pattern | Class | Why |
|---|---|---|
| `.env`, `.env.*` | **Secret** | Already excluded by #3837. Non-negotiable. |
| Any token/credential store, OAuth cache, keychain export | **Secret** | Credentials are machine-bound and resolver-owned (#2643). A synced token is a synced compromise. |
| Any secret-bearing key inside `config.toml` | **Secret** | Requires a scrub gate, not just a path rule — §5.1 |
| `events/`, `events.jsonl`, listener cursors | **Event state** | Per-machine stream position. Syncing a Gmail history cursor makes two machines fight over "already processed". |
| `sessions/`, `repl_history.txt` | **Runtime** | Per-machine conversational state |
| `projects.json` | **Machine-local** | Paths to local checkouts; meaningless on another machine |
| `state/`, `logs/`, `sockets/`, `*.sock`, `*.lock` | **Ephemeral** | Regenerable or machine-bound |
| `backups/`, `*.bak*`, `*.stale.bak*`, `agents-disabled-*/` | **Superseded** | Git is the undo mechanism |
| `.trusty-search/`, `*.redb`, `*.usearch` | **Regenerable index** | Rebuildable from the corpus; large; binary; useless in diffs |
| `knowledge/sources/` | **Bulk raw** | 3.1 MB of raw source docs today; the distilled tree is the artifact, not the input |
| `demo-kit/` | **Binaries** | Not configuration |

`repl_history.txt`, `sessions/`, `projects.json`, and `events/` are currently
*tracked* in the #3837 baseline. Moving them to excluded is part of this spec's
first implementation slice and is a deliberate change, not an oversight.

### 4.3 The rule behind the table

> Sync what a human authored or a template instantiated. Never sync what a
> process derived, what a stream advanced, or what a credential store issued.

Applied to a novel file, this rule decides it without a spec amendment.

---

## 5. SPEC-AGENTSYNC-04 — Secrets, Size, and the Knowledge-Tree Question {#SPEC-AGENTSYNC-04~draft}

### 5.1 Secrets: a gate, not a path list (normative)

Path-based exclusion is necessary and insufficient. `config.toml` is synced and
is exactly the kind of file that accretes an API key. The sync path therefore
runs a **pre-push scrub gate**:

- **S1** — A deny-list of key names (`*_token`, `*_secret`, `*_key`,
  `password`, `authorization`, `api_key`) checked against every synced TOML/JSON,
  plus high-entropy-value and known-credential-prefix detection over synced text.
- **S2** — The gate **blocks the push** and names the file and key. It never
  auto-redacts: silently altering a user's config to make a push succeed is worse
  than failing.
- **S3** — Fails closed. Gate error ⇒ no push.
- **S4** — Applies to `persona.md` and `events/*.md` too. Instructions are prose,
  and prose is where a pasted token hides.
- **S5** — A committed secret is treated as compromised. The runbook is: rotate
  first, scrub history second. Documented in the repo README, because a private
  repo is not an encrypted one.

### 5.2 Size discipline

`attachments/` is user-supplied and unbounded by nature. A per-file cap (e.g.
1 MiB) and a per-agent total cap, enforced at commit time with a clear message,
keeps a clone fast and keeps a config repo from becoming an asset store.

### 5.3 OPEN QUESTION — do knowledge trees sync? {#SPEC-AGENTSYNC-04-Q1}

**This is the one decision this spec does not make. It needs owner sign-off.**

The directive says "the agent configuration **and store**". "Store" is genuinely
ambiguous between the `[[stores]]` *binding* (a few lines of `agent.toml` —
unambiguously synced) and the OKG *knowledge tree* the binding names.

Measured today:

| Artifact | Location | Size | Growth |
|---|---|---|---|
| All agent config | `~/.trusty-agents/agents/` | **516 KB** on disk, 34 tracked files, whole repo 260 KiB packed | Flat — bounded by agent count |
| `bob-kb` distilled tree | `~/trusty-agents/bob-kb` *(a third location — see §5.4)* | **2.7 MB** | **Unbounded** — grows with every ingest |
| Raw sources | `~/.trusty-agents/knowledge/sources/` | 3.1 MB | Unbounded; already excluded |
| Search index | `.trusty-search/`, `*.usearch` | large, binary | Regenerable; already excluded |

**Option A — Config only.** The monorepo holds `agents/<agent>/` plus the §4.1
platform files. Knowledge trees stay out; a new machine re-ingests from the
sources the registry names.

- **For:** Repo stays ~0.5 MB and clones instantly. Growth is bounded by agent
  count, not corpus size. No conflict semantics needed for machine-generated
  markdown. `_sources/*.jsonl` ledgers are append-only journals — close to the
  worst-case shape for git. No risk of a private corpus landing in a repo whose
  sharing model may later loosen.
- **Against:** A new machine must re-ingest, which costs API quota and wall-clock,
  and for a Gmail backfill may not be fully reproducible (deleted messages).
  Distilled entities — the ones a model wrote, which cost tokens — are lost work
  if the machine dies.

**Option B — Config plus distilled knowledge trees.** Adds
`knowledge/<tree>/` (entities + `_sources/registry.toml` + `_sources/*.jsonl`),
still excluding raw sources and indexes.

- **For:** A clone is a complete, immediately-useful assistant. Distilled entities
  are preserved. Ledgers travel, so a second machine's ingest correctly skips what
  the first already pulled — genuinely valuable, since idempotency is per-ledger.
- **Against:** DOC-55 (the universal importer) is explicitly designed to make this
  grow fast — every new extractor and connector multiplies corpus volume, and
  `xlsx`/`pdf` extraction produces large text bodies. Ledgers only ever append.
  Entity writes are whole-file overwrites (`put_entity` overwrite mode), so a
  re-ingest rewrites files wholesale rather than producing small deltas. Two
  machines ingesting the same source concurrently produce conflicting ledger
  appends with no merge strategy — a class of conflict Option A does not have.

**Recommendation: Option A for the monorepo, with knowledge trees in separate
per-store repositories** (e.g. `bobmatnyc/trusty-kb-<tree>`), opt-in per store,
sharing the same `tagent sync` mechanics. Rationale:

1. The two artifacts have opposite growth profiles and opposite conflict
   semantics. Coupling them means every config clone pays the corpus's cost
   forever, and the corpus's ledger-conflict problem blocks config sync.
2. It preserves the option: a store repo can be added later without restructuring
   the monorepo, whereas splitting a large history back out is painful.
3. The `[[stores]]` binding already gives each store an identity (`name`,
   `tree`, `index`), so "this store syncs to this remote" is a natural additional
   field, not a new concept.

**Sub-question if Option B is chosen:** ledger conflict strategy for concurrent
multi-machine ingest. `_sources/<id>.jsonl` is append-only, so a union merge is
*mechanically* correct (the ledger tolerates duplicate lines — `is_current`
reads the latest record for an id) but must be verified against
`Ledger::watermark()`, which counts. Do not assume; test it.

### 5.4 Incidental finding: KB trees live in three places

Worth recording because it affects any Option-B implementation. The `bob-kb`
tree is at `~/trusty-agents/bob-kb` (no leading dot), the OKG tools resolve to
`${KB_KNOWLEDGE_DIR:-$HOME/.trusty-agents/knowledge}/<agent>`, and
`~/.trusty-agents/knowledge/` currently contains only `sources/`. This is the
same disconnect #3892 documents from the search side. **Option B is not
implementable until the tree location is single-valued.** Option A is unaffected.

---

## 6. SPEC-AGENTSYNC-05 — Sync Direction, Conflicts, and Subsuming #3844 {#SPEC-AGENTSYNC-05~draft}

### 6.1 The problem, precisely

`reprovision_bundled_agents_locked`
(`crates/trusty-agents/src/agents/bundled/mod.rs:203`) compares exactly two
things: the on-disk file and the binary-embedded bundle. When they differ it
copies the on-disk version to `<dest>.stale.bak` (only if no backup exists) and
overwrites. This is careful — atomic writes, a pass-level lock, no clobbering a
prior backup, non-bundled files untouched — and still loses hand-edits, because
**with two inputs the only available operations are "keep" and "replace".** #3844
proposes a 3-way merge; the mechanism has nowhere to get the third input from.
`.stale.bak` is not a merge base: it is whatever the file happened to be at some
earlier overwrite, it is overwritten in some paths, and it is gitignored.

### 6.2 The design: a vendor branch supplies the merge base

Once configuration lives in git, the merge base is free.

```
  templates ──T1────────T2────────T3          (bundled templates, one commit
                \         \         \          per bundle stamp — machine-written)
                 \         \         \
  main ──────A────M1───B────M2───C────M3──▶   (user edits A,B,C; merges M1..M3)
```

- The binary's embedded bundle is committed to a **`templates` branch**, one
  commit per distinct bundle stamp, containing only bundled files.
- User edits are ordinary commits on `main`.
- Reprovision becomes `git merge templates` into `main`. Git supplies the true
  common ancestor (the previously merged template commit), so a hand-edited
  `role` and an upstream-changed `tools` list **both survive** — the outcome
  #3844 asks for, obtained from a mechanism that already exists and is trusted.
- `.provisioning/bundle-stamp` records which template generation `main` descends
  from, so the "is the bundle stale?" test that today drives the clobber becomes
  the "do I need to merge?" test.

**Genuine conflicts** (the same key edited on both sides) are a normal git
conflict. Handling, in order: leave the working tree conflicted and **refuse to
start the affected agent** with a message naming the file; surface it in the GUI
as "this agent needs attention"; offer take-mine / take-theirs / open-editor. A
conflict must never resolve silently — that is the #3844 failure in a new costume.

**#3844 disposition:** implemented-by rather than duplicated. #3844's "visible
warning on `.stale.bak`" remains worth doing as a stopgap until this lands, since
the merge design is a larger change than the bug's urgency warrants on its own.

### 6.3 Remote sync direction

Three-way is the same shape one level up. Local `main` and remote `main` diverge
when two machines edit; git's merge handles it, and the same conflict policy
(§6.2) applies. Normative rules:

- **D1 — No force-push, ever.** A config repo's history is a user's undo stack.
- **D2 — No auto-resolve on conflict.** Conflicts stop and ask.
- **D3 — Pull is a merge, never a reset.** A `git reset --hard origin/main`
  recovery path recreates the exact data-loss class this spec exists to end.
- **D4 — Push is fail-safe.** A rejected push leaves the local repo intact and
  reports; it never rewrites local history to make the push succeed.

---

## 7. SPEC-AGENTSYNC-06 — Platform Mechanics {#SPEC-AGENTSYNC-06~draft}

### 7.1 `tagent sync`

| Command | Behavior |
|---|---|
| `tagent sync init [--remote URL]` | Creates/attaches the private remote, normalizes layout (§3.2), applies the `.gitignore`, runs the secret gate, pushes the initial commit |
| `tagent sync status` | Ahead/behind, uncommitted changes, unmerged template generation, conflicts — no network writes |
| `tagent sync` | Commit pending changes → pull/merge → secret gate → push. The single verb a user needs |
| `tagent sync pull` / `push` | The halves, for scripts |
| `tagent sync resolve <agent>` | Guided conflict resolution for one agent package |

Each is a thin wrapper over the same internal API the auto-sync path (§7.3) and
the GUI use, so there is exactly one implementation of the merge policy.

### 7.2 Auto-commit on write (#3837(c))

Agent PATCH/create APIs and persona edits commit automatically. Without this,
auto-sync has nothing to sync and the user's last hour of GUI edits is invisible
to git. Commit messages are generated and specific
(`config(izzie): update tools allow-list via GUI`), because a wall of "auto
commit" defeats git-as-undo — the mechanism #3837 is explicitly buying.

### 7.3 Auto-sync cadence

Recommended default: **on-change, debounced, plus a periodic floor.** Push a few
seconds after the last write settles; pull-merge on daemon start and every N
minutes (default 15). Two constraints, both normative:

- **A1 — Never auto-merge into a running agent's live config mid-turn.** Apply
  the merge at a turn boundary or on next agent start. Swapping a persona under a
  running conversation is a debugging nightmare.
- **A2 — Auto-sync never resolves a conflict.** On conflict it stops, marks the
  agent, and waits. (Same rule as §6.2, restated because auto-paths are where
  "just pick one" gets added later.)

Cadence is configurable and fully disableable (`[sync] auto = false`); a user who
wants manual control gets it.

### 7.4 Bootstrap on a new machine

`tagent sync init --remote git@github.com:bobmatnyc/trusty-agents-agents.git`
clones into `~/.trusty-agents`, merges the current binary's `templates` commit
(so a newer binary's template changes apply cleanly on first run), and reports
what is *not* synced and therefore needs local setup: credentials, listener
cursors, project paths. That report is the feature — a restore that silently
omits credentials and appears to work is worse than one that says what is missing.

---

## 8. SPEC-AGENTSYNC-07 — Multi-Machine Story {#SPEC-AGENTSYNC-07~draft}

### 8.1 What "my agents follow me" means

Machine A and Machine B share agent definitions and diverge on everything
machine-bound. Editing Izzie's persona on A reaches B on B's next sync. Both
machines authenticate independently (credentials never travel, §4.2). Both
maintain independent listener cursors — otherwise they duplicate or drop events.

### 8.2 The machine-identity boundary

| Concern | Shared | Per-machine |
|---|---|---|
| Persona, manifest, event instructions | ✅ | |
| Platform config (`config.toml`, `user.toml`) | ✅ | |
| Credentials / OAuth tokens | | ✅ |
| Listener cursors, event stream position | | ✅ |
| Project paths (`projects.json`) | | ✅ |
| Conversation history, sessions | | ✅ |
| Search indexes | | ✅ (regenerated) |
| Knowledge trees | *pending §5.3* | *pending §5.3* |

### 8.3 The concurrent-listener hazard (must be designed for, not discovered)

If two machines run the same `gmail-personal` listener with independent cursors,
the event is delivered twice and the assistant may act twice. This is **not**
solved by config sync — it is *created* by it, because before sync a second
machine would not have had the listener configured at all.

Minimum viable handling: `tagent sync status` reports when another machine's
config declares the same listener, and the platform surfaces a warning. Proper
handling — a listener lease, or a per-listener `active_on` machine binding — is
follow-up work and should be filed as its own ticket rather than smuggled into
this spec's implementation.

### 8.4 Non-goals

- No real-time/CRDT sync. Git-granularity is sufficient; anything finer is a
  research project.
- No cross-**user** sharing. This is one user, N machines. Publishing or sharing
  an agent is a different feature with a different trust model.
- No hosted service. The remote is a git host the user already has.

---

## 9. Phased Delivery

**Phase 1 — Layout normalization.** `git mv` flat shadows into directory
packages (#3837(a)); extend `.gitignore` per §4.2; untrack `projects.json`,
`events/`, `sessions/`, `repl_history.txt`. Local only, no remote. *Blocks
everything else.*

**Phase 2 — Remote + secret gate.** Create the private repo; `tagent sync init`
/ `status` / `pull` / `push`; the §5.1 scrub gate; the bootstrap report (§7.4).
Delivers "my agents follow me" manually.

**Phase 3 — The templates branch (subsumes #3844).** Commit the embedded bundle
to `templates`; `.provisioning/bundle-stamp`; reprovision becomes a merge;
conflict policy and `tagent sync resolve`; retire `.stale.bak`.

**Phase 4 — Auto-commit + auto-sync.** #3837(c) write-path commits; debounced
auto-sync with the A1/A2 constraints; GUI conflict surfacing.

**Phase 5 — Knowledge trees, if Option B (§5.3) is chosen.** Blocked on the
§5.4 tree-location unification and on a verified ledger merge strategy.

---

## 10. Open Questions

| # | Question | Recommendation | Status |
|---|---|---|---|
| Q1 | Do knowledge trees sync? (§5.3) | Option A: config-only monorepo; per-store repos later | **Owner sign-off required** |
| Q2 | Does `config.toml` sync wholesale, or a filtered subset? | Wholesale + the §5.1 blocking gate; a filtered subset invents a second config format | Recommendation stands |
| Q3 | One repo for all machines of one user, or one per machine? | One per user — per-machine repos are backups, not sync | Recommendation stands |
| Q4 | Is the `templates`-branch merge (§6.2) too heavy vs. #3844's simpler marker? | Do #3844's visible warning now as a stopgap; the merge is the real fix | Recommendation stands |
| Q5 | Who creates the repo, and with what account? | The `bobmatnyc` account (memory: never the work identity) | Assigned in parallel to a version-control agent |
| Q6 | Listener-lease design for §8.3 | Separate ticket; do not bundle | Follow-up |
| Q7 | Does `user.toml` sync? (#3899 excluded it from the manual pass) | Yes — small, stable, and a restore without owner identity is not a restore | Recommendation stands |
| Q8 | Keep the `<agent>.toml.shadow` files the manual pass created? | No — they are the flat/package ambiguity §3.2 normalizes away | Recommendation stands |

---

## 11. References

- `crates/trusty-agents/src/agents/bundled/mod.rs:203` — `reprovision_bundled_agents_locked`, the clobber path (#3844)
- `crates/trusty-agents/src/stores/config.rs` — the `[[stores]]` binding (#3878)
- `~/.trusty-agents/.gitignore` — the #3837 baseline exclusion list
- [DOC-54 Trusty Agents Product Specification](./trusty-agents-product-spec.md) §3.2, §3.3, §6.1
- [DOC-55 Universal OKG Importer](./okg-universal-importer.md) — §5.3's growth argument
- Issues: #3837 (git-backed content store), #3844 (reprovision clobber), #3816 (templates), #3892 (tree/index disconnect), #2643 (credential resolver)

---

## 12. Change Log

| Date | Change |
|---|---|
| 2026-07-25 | Initial draft — repo identity, layout, sync boundary, templates-branch merge subsuming #3844, multi-machine model, knowledge-tree question open for sign-off |
