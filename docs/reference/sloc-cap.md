# SLOC File Size Cap — Mechanics Reference

> **The cap numbers, classification rule, and merge-blocking prohibition are
> in [`CLAUDE.md`](../../CLAUDE.md)** (Key Conventions). This page is the counting definition, the
> batch-with-the-next-change scheduling rule, the ratchet-allowlist mechanics,
> and the resolved-refactor history — consult it when running the gate locally,
> updating the allowlist, or deciding which PR carries a split.

## Enforcement

As of issue #610 the production cap is no longer advice: it is gated by
`scripts/check_line_cap.sh`, wired into CI (`.github/workflows/line-cap.yml`)
and the local pre-commit hook (`line-cap`). A new tracked production `.rs` file
over 500 SLOC **cannot merge**; a new test/benchmark `.rs` file over 3000 SLOC
**cannot merge**. Files approaching their limit are a signal to split into
focused submodules as part of the next change that lands on them — see "When a
Violation Gets Fixed" below for why the split rides along. When splitting, prefer:
one public module per logical concept, a thin `mod.rs` that re-exports, and
sibling files with clear single responsibilities.

## When a Violation Gets Fixed — Batch, Never Standalone

A file can cross its cap without anyone touching it: main grows the file under a
branch, a rebase lands both halves, and the gate goes red on a diff that never
added those lines. The fix is a split, and the question is *which PR carries it*.

**It is the PR that next adds to that file, always.** Never open a PR — or
dispatch an agent — whose only deliverable is bringing a file back under cap.
That PR pays a full CI cycle, a review pass, and a merge for zero behavior
change, and it conflicts with whatever branch is already editing the file. Since
a PR that adds to an over-cap file is blocked by the gate regardless, folding the
split into it lands the same work at the same moment for one CI cycle instead of
two.

The rule is about *scheduling* the split, not skipping it. A red gate never
merges (see the ratchet rules below), so if your PR trips the cap, you split in
your PR — the pressure to split arrives exactly when someone is already in the
file and best placed to choose the seam.

Worked example: a rebase pushed `crates/trusty-mpm/src/bin/tm/main.rs` to 505
SLOC because main had grown the file while the branch was adding a third
`RepairAction` arm. The split shipped inside that PR — the dispatch match moved
into a `commands` submodule and `main.rs`'s arm became one line. What the rule
forbids is the opposite shape: a separate PR splitting `main.rs` while nobody is
otherwise touching it.

Two cases sit outside this rule. A file that is genuinely un-growable without a
split (every pending change to it is blocked) is a refactor ticket in its own
right — that is a planned restructuring, not a cap fix. And an allowlisted file
that drops below its budget only needs `check_line_cap.sh --update`, which is a
line in whatever PR shrank it.

## SLOC Counting Definition

A line counts only when it contains non-whitespace source code after all
comment matter is stripped. These are **excluded** from the count:
- blank / whitespace-only lines
- `//` line comments (including `///` doc comments and `//!` inner-doc comments)
- `/* ... */` block comments — including multi-line spans; every line inside an
  open block comment is excluded
- lines that consist entirely of a closing `*/`
- inline `#[cfg(test)] mod <name> { … }` unit-test modules — see below

A line that has code followed by a trailing `// comment` **still counts** — it
has code. The counter is a pragmatic awk heuristic that errs toward leniency:
edge cases (e.g. `//` inside a string literal) may undercount SLOC but will
never falsely fail a legitimate file.

## `#[cfg(test)]` Modules Do Not Count (issue #5153)

Inline unit tests are not production code, and the cap is about production
code. Before #5153 they counted, so a 460-SLOC module that gained tests crossed
500 and had to have its test module mechanically moved into a sibling
`_tests.rs` — a large diff for zero change in what the cap exists to limit.

**What is excluded:** exactly one shape, `#[cfg(test)]` on its own line
followed (blank lines and further `#[…]` attributes permitted) by
`mod <name> {` at the *same indentation*, through the matching close. The
`mod` may carry a `pub` / `pub(crate)` / `pub(super)` visibility. Both the
top-level and the nested/indented forms work.

**What is NOT excluded — each of these is counted as production:**

| Construct | Why it still counts |
|---|---|
| `#[cfg(test)] mod tests;` and `#[cfg(test)] #[path = "x.rs"] mod y;` | Sibling-file declaration, one or two lines. The named file is classified as a test file by its own path anyway. |
| `#[cfg(test)]` on an `fn`, `impl`, `static`, or `use` | Matching them means parsing multi-line signatures and `where` clauses whose opening brace is not on the item's first line — a large fail-open surface for the ~20 occurrences in this workspace. Put test helpers inside the `#[cfg(test)] mod`. |
| `#[cfg(all(test, unix))]`, `#[cfg(any(test, feature = "x"))]`, `#[cfg(not(test))]` | Only the exact literal `#[cfg(test)]` is recognised. `any(test, …)` genuinely compiles into non-test builds. |
| A test module whose brace balance is skewed by a `{` or `}` inside a string, char, or raw-string literal | The counter is line-based, not a Rust parser. It cannot verify the extent, so it does not guess. |
| A test module with no matching close before EOF | Same — no extent, no exclusion. |

**The limitation, stated plainly.** The matcher has no notion of string
literals. It finds a region's end by requiring two independent methods to
agree: the running `{`/`}` balance must return to zero on the same line that
is exactly the attribute's indent followed by `}`. When literals skew the
balance the two disagree, and the region is counted in full. So the failure
mode is a **false cap violation** on an unusual test module, never a silently
under-counted production file. That direction is deliberate: an over-matching
stripper would let real production bloat through a gate reporting OK.

Fixtures for every row above live in `scripts/test-data/sloc-cfg-test-*.rs` and
are asserted by `scripts/check_line_cap_selftest.sh`, which also runs the gate
end to end against a 600-SLOC production decoy (must fail) and a
400-production + 400-inline-test file (must pass).

## The Ratchet — Allowlist That Can Only Shrink

Grandfathered files over their applicable cap are listed in
`.line-cap-allowlist.tsv` (one `relative/path<TAB>budget` line each, where
`budget` is that file's frozen max SLOC count). The gate enforces
per-applicable-cap ratchet semantics:

- SLOC ≤ applicable cap and **not** allowlisted → OK;
- SLOC > applicable cap and **not** allowlisted → **FAIL** (new oversized file — split it);
- allowlisted, current SLOC **exceeds its budget** → **FAIL** (it grew — split it);
- allowlisted, current SLOC **≤ applicable cap** → **FAIL** (drop the entry; ratchet-down forcing function);
- allowlisted, `applicable_cap < SLOC ≤ budget` → OK (grandfathered, not growing).

So allowlisted files may only shrink, and no new oversized file may be added.
As the #607 sweep and per-crate refactors land, the allowlist ratchets down
toward empty.

**Run it locally:** `bash scripts/check_line_cap.sh` (exit 0 = clean). After you
intentionally split a file (or a file otherwise drops below its budget), refresh
the frozen budgets with `scripts/check_line_cap.sh --update` — this only *lowers*
budgets or *removes* entries that fell ≤ their applicable cap; it **refuses** to
add a new oversized file or raise a budget unless you pass `--seed` (initial
bootstrap) or `--force-add` (rare, intentional bump). Commit the regenerated
`.line-cap-allowlist.tsv` alongside your split.

## Resolved Refactor History

Past violations (refactor tickets #170/#171/#172 are CLOSED and the splits have
landed — all three former monoliths are now under the 500-SLOC cap):
- `crates/trusty-agents/src/ctrl/mod.rs` — RESOLVED (#170). Split into focused
  submodules under `crates/trusty-agents/src/ctrl/` (`state`, `config`, `repl`,
  `handlers`, `pm_task`, …); `mod.rs` is now a ~50-line re-export facade.
- `crates/trusty-agents/src/runtime/` — RESOLVED (#171). The original `runtime.rs`
  was split into a `runtime/` module; every submodule is now under the cap.
- `crates/trusty-agents/src/workflow/engine/` — RESOLVED (#172). The original
  `engine.rs` was split into an `engine/` module; every submodule is now under
  the cap (largest is `engine/executor/run.rs` at ~485 lines).

The largest remaining production file in `trusty-agents` (not tied to an open
ticket) is `tm/manager.rs` — file a fresh refactor ticket before growing it
further. Current per-file SLOC budgets live in `.line-cap-allowlist.tsv`.
