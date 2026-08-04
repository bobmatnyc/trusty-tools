---
name: feedback-vmtest-harness-doctrine
description: Non-negotiable working rules for vmtest-harness/ work — harness adapts to product, record-don't-delete docs, never oversell a fix, VM teardown discipline
metadata:
  type: feedback
---

Four rules govern all `vmtest-harness/` work. They are enforced repeatedly and explicitly in task briefs.

**1. THE HARNESS ADAPTS TO THE PRODUCT, NEVER THE REVERSE.** Never change anything under `crates/` to make a harness assertion pass. When harness and product disagree, the product is presumed right and the harness (or its docs) is the defect.
**Why:** the harness exists to detect packaging regressions; if it can edit the product to go green it asserts nothing. Repeatedly, the "product bug" turned out to be a harness misreading of product source (e.g. `tctl port --json-port`'s `.addr` is the host alone, not `host:port`).
**How to apply:** read the product source before assuming a harness failure is real. Quote the product's own comments — they often already record the decision you are about to re-derive.

**2. RECORD-DON'T-DELETE in the doc set** (`docs/research/tart-vm-testing-harness/`). Superseded text is struck through or given a **SUPERSEDED <date>** pointer, never rewritten away. Historical run transcripts in `MANIFEST.md` are never edited. Corrections state what was wrong, why, and what was re-checked-and-correct.
**Why:** the doc set is the audit trail for a harness whose whole value is trustworthiness; a silently-rewritten claim is indistinguishable from one that was never wrong.
**How to apply:** amend in place with a dated note; add new numbered open items rather than renumbering.

**3. NEVER OVERSELL A FIX; UNDERSELLING IS ALSO A DEFECT.** State precisely what a changed predicate accepts and rejects. If a brief hands you a framing ("this is only log-line honesty", "coverage is already asserted elsewhere"), verify it — briefs have been wrong in the *understating* direction, and reporting that back is expected, not pedantic.
**Why:** the owner decides on this evidence. A fix described as cosmetic when it closes a real hole misleads exactly as much as the reverse.
**How to apply:** when relaxing any assertion, write the accept/reject boundary explicitly and confirm the failure case still fails.

**4. VM DISCIPLINE, absolute.** Every VM named `vmtest-*`. Teardown on every exit path via `trap`. Never a bare `tart stop` trusted as completion: `vm_request_stop` → `vm_wait_for_stopped` → `vm_delete`. **Never `tart suspend`.** Run `tart list` before and after and paste both raw; announce loudly if a `vmtest-*` leaks. Host repo is never mounted into the guest.

Git: **merge `origin/main` INTO the branch, never rebase, never force-push** — rebasing makes version-bump and changelog CI checks inherit blame for unrelated upstream commits.

See [[project-vmtest-harness]].
