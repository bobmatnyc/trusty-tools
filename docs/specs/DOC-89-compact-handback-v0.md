# DOC-89 — Compact handback v0 and structured IL block v1

**Status:** Draft
**Subsystem:** Agent handbacks (BASE-AGENT completion format, Intent Ledger handback tail)
**Owner:** tm-dogfood (schema); decisions marked OQ belong to the Architect
**Last-updated:** 2026-11-12
**Spec ID:** `SPEC-HANDBACK-01~draft` … `SPEC-HANDBACK-05~draft` (DOC-89, v0); `SPEC-HANDBACK-06~draft` … `SPEC-HANDBACK-13~draft` (DOC-89, v1)
**DOC-N claim:** `DOC-89`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). Highest on `origin/main` at 2026-11-11 was `DOC-88`.

**Revision choice (2026-11-12).** v1 is an in-place revision of this file, not a superseding file. The Architect ruled at 17:05Z to merge into DOC-89 and not create a second spec. The file has no frontmatter and every section is still `~draft`, so nothing was accepted and nothing needs a `Superseded` marker; DOC-38 section 4.4 reserves `~v1` for acceptance, so the sections keep `~draft`. v1 adds sections 6 to 13. Sections 1 to 5 stay as the v0 record and the phase-1 fixtures' second input format. The filename keeps `-v0` so existing links hold. Ticket: [#11133](https://github.com/bobmatnyc/trusty-tools/issues/11133) (Architect task ar-T2f). Owner instruction: 2026-11-12 17:01Z and 17:03Z (Architect brief `2026-11-12-structured-response-instruction.md`).

Source of v0: agent-comm POC Track 1, plan step 3 (`/Users/masa/trusty-mpm-projects/reviews/agent-comm-poc/plan.md`), owner ruling D0e. This spec proposes a schema for the POC replay. It changes no existing file and is not adopted until the POC verdict is GO.

## 1. Scope {#SPEC-HANDBACK-01~draft}

**ID:** SPEC-HANDBACK-01~draft

Compact handback v0 is a block of at most 10 lines that a sender puts at the top of a handback. It carries the facts the receiver needs for its next action. It is parseable by regex, so a deterministic extractor can build it from a free-prose handback (plan arm C).

It does not replace the evidence. Long evidence stays in a file that `refs` names.

## 2. Fields {#SPEC-HANDBACK-02~draft}

**ID:** SPEC-HANDBACK-02~draft

One field per line. A line is `<field>: <text>` (field name, colon, one space). Fields `need`, `open`, `decided` and `refs` may repeat. The block has at most 10 lines and each line at most 240 characters. Order: `need`, `delta`, `open`, `decided`, `refs`, `next`.

| Field | Status | Syntax of `<text>` |
|---|---|---|
| `need` | Required when the dispatch carried `need:` keys; else omitted | `<key> => <answer>`. One line per key, `<key>` echoed verbatim from the dispatch. An unanswerable key reads `<key> => UNKNOWN: <reason>`. |
| `delta` | Required, exactly once | `[VERIFIED]` or `[NEEDS-ATTENTION]`, then what changed since the receiver's last known state, then the one-line gate verdicts, each `<command> => <verbatim final line>` (for example `cargo test -p x => test result: ok. 68 passed; 0 failed; 0 ignored; EXIT=0`). |
| `open` | Required, at least once | `none`, or `<question> \| owner=<who>`. One line per question. |
| `decided` | Optional, repeatable | `<YYYY-MM-DD> <attributor>: "<verbatim text>" @ <cite>`. See section 4. |
| `refs` | Required when `delta` names a file, commit, run or log | `head=<branch-or-pr>@<sha>` once, then any of `<path>@<sha>:<start>-<end>`, `run:<id>`, `log:<absolute path>`. Binding rules in section 4. |
| `next` | Required, exactly once | One imperative action for the receiver, or `none`. |

A line that cannot fit in 240 characters moves its detail to a file named by a `refs` `log:` line; the field keeps a one-line summary.

## 3. Worked example {#SPEC-HANDBACK-03~draft}

Built from a real handback: the body of PR [#11007](https://github.com/bobmatnyc/trusty-tools/pull/11007) (head `fbdc0206da8c9bae55f08dc7499bea988654f7a4`, squash commit `81afaac86c`, merged 2026-11-11T06:31:30Z), read with `gh pr view 11007 --json body` and `git show 81afaac86c`. The original is four prose paragraphs, about 900 characters, with no dispatch `need:` keys, so `need` is omitted. Line numbers come from the squash commit's hunks (`doctor.md` line 63, `doctor.rs` line 289).

```
delta: [VERIFIED] egress_guard doctor row now describes the claude.ai connector exemption; source edited in doctor.rs so the drift check passes; 2 fragments added (4 files, +8 -2). Rung 1 for doctor.md, rung 3 for doctor.rs. check_changelog_fragment.sh => OK; check_content.py tree => OK (bundle 2.4.0); check_sld.sh => 0 errors; credential scan => CLEAN
open: none
decided: 2026-11-11 Architect: "Split out of the trusty-mpm 1.12.2 release PR on the Architect's order (option A)" @ pr#11007
refs: head=pr#11007@fbdc0206da8c
refs: content/skills/tm-capabilities/references/doctor.md@fbdc0206da8c:63-63
refs: crates/trusty-mpm/src/bin/tm/generate/doctor.rs@fbdc0206da8c:289-289
next: re-assemble content 2.4.0 on top of main in the 1.12.2 release PR
```

That is 6 lines. Two weaknesses the example shows. The source prose gives the gate verdicts without exit codes, so `delta` cannot carry `EXIT=`. The Architect's order is quoted from the PR body, not from the Architect's original message, so the attribution is second-hand.

## 4. Reconciliation {#SPEC-HANDBACK-04~draft}

**ID:** SPEC-HANDBACK-04~draft

Existing formats: BASE-AGENT "Required completion format" (`content/agents/BASE-AGENT.md`, section "Verification Before Completion"); SPEC-IL-01 (`/Users/masa/trusty-mpm-projects/architect/docs/specs/SPEC-IL-01.md`, sections 2.2, 11.1, 15) with the IL handback tail.

**Placement.** v0 wraps. For an agent handback the order is: the v0 block, then the BASE-AGENT sections (`## Verification Results`) as the long form, then the IL tail. The PM reads the block and opens the long form only when `open` or `delta` says `[NEEDS-ATTENTION]`. For the POC, arm P keeps today's handback unchanged and arm C replaces the long form with the block. Nothing is adopted before GO (OQ1).

| # | Conflict | Resolution |
|---|---|---|
| C1 | BASE-AGENT requires `### Status: VERIFIED WORKING / NEEDS ATTENTION`; v0 has no status field. | Status is the first token of `delta`: `[VERIFIED]` or `[NEEDS-ATTENTION]`. The long form keeps its `### Status` line. |
| C2 | BASE-AGENT requires `[command]: [actual output]` and "show raw output"; v0 caps at 10 lines. | The block carries only the verbatim final result line and exit status per gate. The raw output goes to a file named by `refs` `log:`. Failing gates also quote the first failing test name in `delta`. The rule "never summarise results in your own words" stays satisfied: the final line is verbatim. OQ2 asks whether the owner accepts file-held raw output. |
| C3 | BASE-AGENT "What changed" lists `file:line — change`; v0 `refs` lists `path@sha:lines` without the change text. | `delta` holds the change in words; `refs` holds locations. The long form keeps the per-line list. |
| C4 | IL tail `Task: <id>@MMDD \| done \| est \| remaining \| pct` and `CTX-USE:` are not in v0. | Both stay unchanged, after the block and outside the 10-line cap. The extractor must not drop them. |
| C5 | IL ids (`df-T22`, `D0e`) are ephemeral; v0 `decided` must be readable later. | `decided` cites `pr#N`, `issue#N`, `commit:<sha12>` or `comment:<url>` and never an IL id alone. An IL id may follow the cite as a hint, `(il:<S id>:<id>)`, qualified per SPEC-IL-01 section 15 (for example `il:S14:Ddb`). A decision with no PR, issue, commit or comment yet uses `@ none` and the receiver treats it as unreferenced. |
| C6 | The brief handback shape has `Next: <action>`; v0 has `next`. | `next` is the sender's action for the receiver, inside the block. The IL `Next:` line is the PM's session next. Both may appear; neither replaces the other. |
| C7 | `Goals:` and `Open D/A:` lines (PM handbacks) overlap `open`. | `open` lists questions with owners. `Open D/A` lists Architect/owner decision ids and stays outside the block. |
| C8 | Plan says `decided` is "verbatim, dated, attributed" so the IL "reads the same". SPEC-IL-01 `owed` entries are `check`/`expect`/`ref` (80 characters each), not prose. | `decided` is not an `owed` entry. A check the receiver owes is a `next` or an `open ... owner=` line; the PM copies it to `session set --owe` itself. |
| C9 | `refs` must bind a path to a commit. SPEC-IL-01 keeps exact SHAs in `owed.expect`, with no path syntax. | Binding rule below. |

**`refs` binding.** The first `refs` line is `head=<branch-or-pr>@<sha>`, the commit the sender read or ran. A path ref is `<path>@<sha>:<start>-<end>` with `<path>` relative to the repo root, `<sha>` at least 12 hex characters, and `<start>-<end>` inclusive (`63-63` for one line). A path ref without its own `@<sha>` is invalid; the extractor never infers it from `head`. A receiver treats a ref as stale when its `<sha>` is not an ancestor of the receiver's current target ref. `run:<id>` and `log:<absolute path>` need no sha.

**Cap versus evidence.** The 10-line cap counts block lines only. It does not limit the long form, the IL tail or files named by `refs`. When more than 10 lines are needed, the sender merges `open` and `need` lines, then moves overflow to a `log:` file and writes `delta: ... TRUNCATED(see log)`. The extractor never drops a failing gate or an `open` line to meet the cap.

## 5. Extractor contract (plan arm C) {#SPEC-HANDBACK-05~draft}

**ID:** SPEC-HANDBACK-05~draft

Input: a sender transcript's final `<result>` text, plus the dispatch text for `need:` keys. Output: one block as defined in section 2.

The harness engineer builds against these rules:

1. Field line regex: `^(need|delta|open|decided|refs|next): .+$`. Anything else in the block is a parse error.
2. Block length: 1 to 10 lines; each at most 240 characters; fields in the order of section 2; each line ends with `\n` and carries no markdown.
3. `delta`: exactly one; starts with `[VERIFIED]` or `[NEEDS-ATTENTION]`. Gate verdicts are copied verbatim from the transcript, never reworded. A missing exit status is written `EXIT=unknown`.
4. `decided`: matches `^\d{4}-\d{2}-\d{2} [^:]+: ".+" @ (pr#\d+|issue#\d+|commit:[0-9a-f]{12,40}|comment:\S+|none)( \(il:[^)]+\))?$`. The quoted text is a substring of the source text; the extractor fails the line if it is not.
5. `refs`: first line matches `^head=\S+@[0-9a-f]{12,40}$`. Path lines match `^[^\s@]+@[0-9a-f]{12,40}:\d+-\d+$`. Every SHA, run id and path appears in the source text; the extractor adds none.
6. `need`: one line per dispatch key, in dispatch order; a key with no answer in the source gets `UNKNOWN: not in sender output`.
7. `open`: `none` only when the source states no open item; otherwise one line per item with `owner=` (`owner=unassigned` when the source names none).
8. Pass-through: the `Task:` and `CTX-USE:` lines are copied unchanged after the block.
9. Cheap-model fallback is used only for fields 3, 6, 7 when rules fail; the harness records its tokens separately (plan metrics).
10. Hand-check: all 8 task blocks are checked against their sources before the run (plan risks).

### Open questions for the Architect

- **OQ1.** Adoption path if the POC is GO: wrapper in `BASE-AGENT.md` plus an IL spec revision, or a separate skill the PM requests per dispatch? Needs an owner ruling because BASE-AGENT is every agent's prompt.
- **OQ2.** May the BASE-AGENT rule "show raw output" be met by a verbatim final line plus a file path (C2)? The POC can test it; adoption changes a standing rule.
- **OQ3.** Should dispatches carry a `need:` key list (plan, `need` field)? Today no dispatch format has it, so `need` is empty in most replay tasks.
- **OQ4.** Does the `[VERIFIED]` token in `delta` conflict with PM guards or hooks that grep `### Status:`? Not checked here.
- **OQ5.** Is a 10-line cap and 240-character line the right size, or should the POC vary it? The plan fixes 10; the 240 is this spec's choice.

---

# v1: structured IL block

## 6. Purpose {#SPEC-HANDBACK-06~draft}

**ID:** SPEC-HANDBACK-06~draft

Every PM turn and every agent handback carries one fenced block with info string `il`. The model fills it once. Scripts parse it to build dashboards, ledgers, update lists and reports. No second model pass extracts or formats.

````
```il
goal: tt-G01
task: tt-T2f@1112
done: wrote the v1 grammar into DOC-89
pct: 60
next: run check_sld.sh
est: 20m
remaining: validators, fixtures
open: tt-D4e tt-A1c
new: D Which session is exempt? | opts: Architect*; none
evidence: head=docs/11133-il-block-spec@b59075b7b4
evidence: log:/tmp/il133-gates.log
evidence: check_sld.sh => 0 errors; EXIT=0
```
````

## 7. Grammar {#SPEC-HANDBACK-07~draft}

**ID:** SPEC-HANDBACK-07~draft

One `key: value` line per key (key, colon, one space). Order is free. Each line is at most 240 characters; `done` is at most 200.

| Key | Status | Value |
|---|---|---|
| `goal` | required | goal id |
| `task` | required | `<id>@MMDD`, with `<id>` a task id |
| `done` | required | what changed this turn, at most 200 characters |
| `pct` | required | integer 0 to 100 |
| `next` | required | one imperative action, or `none` |
| `est` | optional | time estimate |
| `remaining` | optional | what is left |
| `open` | optional | space-separated D and A ids |
| `new` | optional, repeatable | `D\|A <title> \| opts: a*; b`. `*` marks the recommendation. A decision already made reads `D <title> \| decided: <option> @ <cite>`, with `<cite>` as in C5. |
| `evidence` | optional, repeatable | `head=<branch>@<sha>`, `run:<id>`, `log:<path>`, or one final gate line ending `EXIT=<n>` |
| `error` | optional | an error that fits no other key (prose) |
| `feedback` | optional | prompt feedback (prose) |

An id matches `^[a-z][a-z0-9]{1,7}-[GDAT][0-9a-f]{2}$`. The letter is the type: G goal, D decision, A action, T task. Every key except `new` and `evidence` is a singleton.

### Mapping from the v0 fields

| v0 field | v1 |
|---|---|
| `need` | The answer goes in `done`; an unanswerable key becomes `new: A ...` or `error`. No key of its own, because no dispatch format carries `need:` (OQ3). |
| `delta` | Change text goes in `done`. Gate verdict lines go in `evidence`. `[VERIFIED]` is implied by every gate line ending `EXIT=0`; a failure goes in `error`. |
| `open` | `open`, ids only. A question with no id becomes `new: A`. |
| `decided` | `new` with `decided:`. The verbatim quote moves to the detail text of that item. |
| `refs` | `evidence`. v0 path refs (`path@sha:a-b`, section 4) stay valid inside `evidence`. |
| `next` | `next`. |

## 8. Rejects {#SPEC-HANDBACK-08~draft}

**ID:** SPEC-HANDBACK-08~draft

A parser rejects the block on any of: a line inside the fence that is not a known key; a repeated singleton; a missing required key; an id that fails the pattern; `pct` outside 0 to 100 or not an integer; a span shaped like a secret. A secret is rejected, never masked. The secret patterns are the repository's credential-scan patterns; phase 1 names the file.

## 9. Precedence and coexistence {#SPEC-HANDBACK-09~draft}

**ID:** SPEC-HANDBACK-09~draft

When a response carries both, the block wins over the legacy `Task:` last line. The legacy line stays valid, and the DATG hook rules for it stay in force, until phase 5. Where `Task:` and `Card:` disagree, `Task:` wins (Architect ruling). The Architect's dashboard ingester (ar-T63) keeps its per-source parser for the legacy line; the block is added beside it.

## 10. Validation {#SPEC-HANDBACK-10~draft}

**ID:** SPEC-HANDBACK-10~draft

- **Stop (PM turns).** A validator in `datg.py hook`, modelled on `dash-footer-stop.py`. On a violation it returns `decision:block` with the first violation and the grammar. It allows one retry through `stop_hook_active`. On an internal error it fails open.
- **SubagentStop (handbacks).** New tm code. On a violation it exits 2 and writes the reason to stderr. It also fails open on an internal error.

Both call one parser, the phase-1 parser.

## 11. IL-only mode {#SPEC-HANDBACK-11~draft}

**ID:** SPEC-HANDBACK-11~draft

From phase 4 this is the default for IL sessions (Architect ruling). The response is the block plus the Dashboard footer. Prose is allowed only in the detail of a new D item, in `error:` and in `feedback:`. Agent handbacks are IL-only too; raw gate output goes in a `log:` file named by `evidence`.

IL-only mode overrides:

- Output style "PM Response Format".
- prompt-self-improvement's `## Prompt feedback` section; it becomes `feedback:`.
- The tm-verification-protocols four-part report; it becomes `evidence`.
- BASE-AGENT "Required completion format"; it becomes `done` plus `evidence`.

BASE-AGENT's rule "show raw output" is met by the verbatim final gate line plus the `log:` path (the v0 question OQ2, now carried by IL-only mode).

## 12. Phases {#SPEC-HANDBACK-12~draft}

**ID:** SPEC-HANDBACK-12~draft

1. Parser plus fixtures. It reads both the block and the legacy line.
2. Warn-only validation.
3. Request the block in PM and agent text. Exit criterion: a transcript measurement of output tokens before and after.
4. Enforce the block. Block-only mode becomes the default for IL sessions.
5. Drop the legacy-line fallback.

Token savings are unmeasured; this spec claims no number for the block.

## 13. Open questions {#SPEC-HANDBACK-13~draft}

**ID:** SPEC-HANDBACK-13~draft

- **OQ6.** Is the Architect session exempt from IL-only mode?
- **OQ7.** Where do `log:` files live, and who deletes them?
- OQ1 to OQ5 above stand.

## Option: id-only exchange {#SPEC-HANDBACK-14~draft}

**ID:** SPEC-HANDBACK-14~draft

**Status:** option, not a decision. Source: Bob via the Architect, 2026-11-12 17:08Z.

**How it works.** At conversation start the relevant IL items load once, with their ids. After that only ids pass in and out. A script expands an id to its full item from the local store.

**The four design questions.**

| Question | Proposed answer |
|---|---|
| Cache validity | The store is an append-only table of `(id, version, text)`. A load records the highest version it saw; an id is valid while no later version exists. A change appends a row, never edits one. Proposed. |
| Id scope (2 hex digits wrap at 256) | An id is unique only inside its session prefix and live window. Exchange uses ids qualified as in SPEC-IL-01 section 15 (for example `S14:Ddb`), and a retired id is not reused while a row for it is still in the loaded set. Reuse after wrap needs a rule from the Architect. Open. |
| Unknown-id validator | The Stop and SubagentStop checks resolve every id in the block against the loaded table. An id not in the table is a violation, same path as section 10. Proposed. |
| Expand-on-receipt hook | A hook on the receiving side replaces ids with item text before the model or the owner sees them. Mechanism (PostToolUse or UserPromptSubmit) not chosen. Open. |

**Measured estimate (this sample only).** Tokens use about 4 bytes per token. `datg.py digest` for `tm-trusty-tools` at v159: 1474 bytes (about 369 tokens). Its GOALS and ITEMS sections are empty at v159, so only `owed` is measured: 8 lines, 467 bytes; bare keys, 48 bytes. Those keys are issue numbers, not IL ids. Three distinct Architect brief texts, id-bearing lines against the ids alone:

| Text | Lines | Full line bytes | Ids only bytes | Ratio |
|---|---|---|---|---|
| `il-task-triage-2026-11-02.md` | 167 | 10544 (about 2636 tokens) | 1190 (about 298 tokens) | 8.9 |
| `dream/2026-11-10-digest.md` | 9 | 1961 (about 490 tokens) | 70 (about 18 tokens) | 28.0 |
| `dream/2026-11-11-applied.md` | 1 | 46 (about 12 tokens) | 7 (about 2 tokens) | 6.6 |

Sample size: three brief texts and one digest. They are Architect briefs, not handbacks, and each "full" figure is the whole line, not only the item text. A fourth file (`2026-11-08-digest.md`) matched the 11-10 file byte for byte and is counted once. The ratios apply to this sample and nothing beyond it.

**Risks.**

- Stale cache: a loaded id points at text changed after load. The versioned table detects it only if the validator compares versions, not just presence.
- Misread id: an id with no text beside it cannot be caught by a reader or reviewer; a wrong but valid id passes the unknown-id validator.
- Owner reading raw ids: the owner sees `tt-D4e`, not the decision. The expand-on-receipt hook must cover owner-facing output, or the owner reads ids only.
