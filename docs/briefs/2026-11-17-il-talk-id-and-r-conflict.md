# IL-talk: `R` meaning and id grammar conflict

**Decision needed (owner, Duq/Bob):** two requirements in DOC-82 contradict each other, and the Rust port cannot be finished until one wins in each case. (1) Does `R` mean an owner request (LDG-2.x) or a library reply (§9.2)? (2) Are ids `<Prefix><hex2>` (LDG-2.2.1) or base-34 with an `@MMDD` stamp (LDG-9.1.4)? The Architect currently treats LDG-2.x as authoritative. Source: DOC-82 §9.6 open questions 1 and 2 (`docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md`).

## 1. Meaning of `R`

| | Says |
|---|---|
| LDG-2.x | LDG-2.1.1 lists items as "decision, ask, task, request R", id "D, A, T, R + 2 hex". LDG-2.2.1: "R owner request". |
| §9 | LDG-9.2.1: a reply is written `R<2 digits>-<letter>`, e.g. `R02-a sha=abc1234`. R01 to R05 are library replies. |

**If LDG-2.x wins:** the reply library needs a new prefix. Breaks: the prototype's `r-library.json` and `relay`/`expand`, `RREF` rejects, R01 sent to PM panes today, and the `done:` field grammar (LDG-9.1.2, 9.2.1). The reply codes are in running Architect and PM prompts; renaming them is a coordinated prototype change.

**If §9 wins:** the owner-request use of `R` is deleted from `items`. Breaks: LDG-2.1.1 and 2.2.1 text, the `items` id pattern in `items.schema.json`, and any stored `R` item. The Rust `trusty-architect` model is not affected: `ids.rs` lists only G, D, A, T, B for PM ledgers and no R item type is modelled there. The DOC-82 port loses a schema row, nothing more.

**Recommendation:** §9 wins (delete the owner-request `R`), because the Rust crate models no `R` item and the reply codes are live in prompts.

## 2. Id grammar

| | Says |
|---|---|
| LDG-2.2.1 | "Every id MUST be `<Prefix><hex2>` ... Hex digits are 0-9a-f only." Check: `ID_RE` is `^[A-Z][0-9a-f]{2}$`. |
| LDG-9.1.4 | "`[<code>-]<G\|D\|A\|T\|R><2 digits>[@MMDD]`. Digits are base 34 (`0-9`, `a-z` without `o` and `l`)." A stamp mismatch is `STALE-ID`. |

Hex is a subset of base 34, so every LDG-2.2.1 id still parses under LDG-9.1.4. The reverse fails: `T1g` is a valid IL-talk id and not a valid hex id. Base 34 gives 1,156 ids per prefix, not 256.

**If LDG-2.x wins (hex only, no stamp):**
- The datg prototype and `task_tag` regexes (in the Architect's private project, not in this repo) already emit base-34 and stamps. They change, or ids they emit fail `ID_RE`.
- The dashboard Tasks view (`dashboard_tasks.py`) is likewise a prototype script; ids with `g`-`z` digits in the existing store would stop matching.
- The new tm `--turn-task` stamp (#11398) emits `Task: <id>@MMDD` (see `crates/trusty-mpm/src/core/turn_task_stamp_tests.rs`). Its output would be an invalid id under `ID_RE` unless the stamp is stripped first.
- The recycling guard has no stamp, so a task id left in a message after the id is reissued (LDG-2.2.3) cannot be told stale. `STALE-ID` is lost.

**If §9 wins (base 34 plus stamp):**
- `trusty-architect` `ids.rs` (`LedgerId`, `is_bare_id`, test `ledger_id_grammar_accepts_exactly_the_one_form`) must widen digits, and the wrap and "lowest free" logic of LDG-2.2.3 must count to 34 per digit, not `ff`. This also touches the writer-parity fixtures (`crates/trusty-architect/tests/ledger_writer_parity.rs`).
- Every record needs a stored creation day, so `@MMDD` can be checked (open question 2); the schemas and the DOC-82 port gain a field.
- The dashboard `ID_RE` check in LDG-2.2.1 changes, and the "property test rejects every other form" is rewritten.

**Recommendation:** adopt base 34 for ids, and keep `@MMDD` as a message-side stamp, not a stored id part, because hex ids stay valid and the stamp is what makes recycled ids safe across messages. The stored creation day (`created`) already exists on items (LDG-2.3.2) and can back the stamp check.

## Current state

The Architect treats LDG-2.x as authoritative, so the Rust crate rejects base-34 ids and the `@MMDD` stamp today. Until the owner rules, `Task: <id>@MMDD` lines from tm need the stamp stripped before id parsing.

Not verified: the datg, `task_tag` and `dashboard_tasks.py` regexes were not read; their behaviour is taken from DOC-82 §9 and the scripts are outside this repository.

**Ruling (2026-11-17, owner ruling Duq):** §9 wins on both points. `R` means only a library reply; ids are base 34 with `@MMDD` as a message-side stamp. DOC-82 LDG-2.1.1, LDG-2.2.1, LDG-2.2.3 and LDG-9.1.4 amended; see DOC-82 §9.6 and §9.7.
