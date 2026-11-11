# DOC-89 — Compact handback v0

**Status:** Draft
**Subsystem:** Agent handbacks (BASE-AGENT completion format, Intent Ledger handback tail)
**Owner:** tm-dogfood (schema); decisions marked OQ belong to the Architect
**Last-updated:** 2026-11-11
**Spec ID:** `SPEC-HANDBACK-01~draft` … `SPEC-HANDBACK-05~draft` (DOC-89)
**DOC-N claim:** `DOC-89`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). Highest on `origin/main` at 2026-11-11 was `DOC-88`.

Source: agent-comm POC Track 1, plan step 3 (`/Users/masa/trusty-mpm-projects/reviews/agent-comm-poc/plan.md`), owner ruling D0e. This spec proposes a schema for the POC replay. It changes no existing file and is not adopted until the POC verdict is GO.

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
