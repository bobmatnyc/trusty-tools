# SLOC File Size Cap — Mechanics Reference

> **The cap numbers, classification rule, and merge-blocking prohibition are
> in [`CLAUDE.md`](../../CLAUDE.md)** (Key Conventions). This page is the counting definition, the
> ratchet-allowlist mechanics, and the resolved-refactor history — consult it
> when running the gate locally or updating the allowlist, not to decide
> whether a file needs splitting.

## Enforcement

As of issue #610 the production cap is no longer advice: it is gated by
`scripts/check_line_cap.sh`, wired into CI (`.github/workflows/line-cap.yml`)
and the local pre-commit hook (`line-cap`). A new tracked production `.rs` file
over 500 SLOC **cannot merge**; a new test/benchmark `.rs` file over 3000 SLOC
**cannot merge**. Files approaching their limit are a signal to split into
focused submodules before the next feature lands on them. When splitting, prefer:
one public module per logical concept, a thin `mod.rs` that re-exports, and
sibling files with clear single responsibilities.

## SLOC Counting Definition

A line counts only when it contains non-whitespace source code after all
comment matter is stripped. These are **excluded** from the count:
- blank / whitespace-only lines
- `//` line comments (including `///` doc comments and `//!` inner-doc comments)
- `/* ... */` block comments — including multi-line spans; every line inside an
  open block comment is excluded
- lines that consist entirely of a closing `*/`

A line that has code followed by a trailing `// comment` **still counts** — it
has code. The counter is a pragmatic awk heuristic that errs toward leniency:
edge cases (e.g. `//` inside a string literal) may undercount SLOC but will
never falsely fail a legitimate file.

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
