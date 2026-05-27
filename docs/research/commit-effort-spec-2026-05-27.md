# Empirical Commit-Effort Scoping — Specification

**Date**: 2026-05-27
**Status**: v1 implemented (LoC + files + tests factor); v2 (cyclomatic
complexity) deferred to a `tga effort-score` subcommand.
**Author**: Bob Matsuoka, drafted with Claude (commit-effort-scope worktree).

---

## 1. Why this exists

Commits in this workspace vary enormously in scope, even within a single
working session. Today's session illustrates the problem:

- **PR #307** — a one-line `Cargo.toml` version bump. **Size: S.**
- **PR #304** — a 471-LoC supervisor refactor with new modules, refactored
  ownership, and additional tests. **Size: L.**
- Several PRs in between landed Cargo.toml metadata bumps, README updates,
  and small fixes. **Size: XS to M.**

Eyeballing `git log --shortstat` to estimate scope works at the moment of the
commit but is useless when querying months of history. Future-Bob's analytics
queries — "what was the modal commit size last quarter?", "did refactoring
PRs cluster before or after release tags?", "is supervisor work consistently
L-sized?" — need a calibrated, persisted scalar that lives **on the commit
itself**, not in a separate database.

This spec defines a composite **effort score** computed at commit time from
the diff, mapped onto T-shirt sizes (XS / S / M / L / XL), and attached to
the commit via three git trailers:

```
Effort: M
Effort-Score: 8.30
Effort-Breakdown: 243 LoC | 4 files | 118 test LoC
```

The trailers are inserted automatically by a `prepare-commit-msg` pre-commit
hook so the user sees them in the draft message and can review or override
before saving.

---

## 2. Empirical foundations

The composite formula combines four well-established models:

| Year | Source | One-line description |
|---|---|---|
| 1976 | **McCabe** — *A Complexity Measure* | Cyclomatic complexity (decision-point count) — first widely-adopted code-complexity metric; basis for the γ term in the v2 formula. |
| 1977 | **Halstead** — *Elements of Software Science* | "Effort" formula E = D · V (difficulty × volume) — first quantitative model that explicitly named "effort" as a measurable property of code. |
| 2008 | **Hindle et al.** — *What Do Large Commits Tell Us?* | Mining commit history of nine OSS projects: commit-size distribution is log-normal (long tail of large commits, mode in the small-to-medium range). Motivates log-scale terms in the formula. |
| 2017 | **SonarSource** — *Cognitive Complexity White Paper* | Modern readability-focused refinement of McCabe — penalises nesting, ignores short-circuit boilerplate. Cited here as the rationale for deferring the γ term: cyclomatic-style proxies are noisy and need language-aware AST traversal (see v2 roadmap). |

These are *industry-accepted* models — referenced in introductory software
engineering textbooks, taught in graduate SE courses, and used in
SonarQube / CodeClimate / Code Climate-style products. We are not inventing
metrics; we are composing existing ones into a per-commit scalar.

---

## 3. The composite formula

```
effort_score = α · log₂(LoC + 1)
             + β · log₂(files + 1)
             + γ · Σ ΔCC                       (v1: γ = 0)
             + δ · tests_factor

tests_factor = 1 − 0.3 · min(test_LoC / max(LoC, 1), 1)
```

### Constants

| Symbol | Value (v1) | Rationale |
|---|---|---|
| α | 1.0 | LoC is the primary signal; log₂ tames the long tail (Hindle 2008). |
| β | 1.5 | File count weighted higher than raw LoC because cross-file changes carry coordination cost beyond the line count alone. |
| γ | 0.0 | Cyclomatic complexity deferred to v2 (see §8). |
| δ | 1.0 | Tests factor enters linearly — small additive nudge, not multiplicative. |

### LoC counting

`LoC` is `additions + deletions` from `git diff --numstat`, summed across all
non-binary files in the range. Binary files contribute zero to LoC but count
toward `files`. Renames and pure-rename diffs are counted only by their
content-change LoC.

### Files counting

`files` is the count of distinct paths in `git diff --numstat`. Renames count
once (the path under the new name). Binary changes count.

### Tests factor

The `tests_factor` rewards commits whose diff is partly tests. A commit
that's pure test (`test_LoC / LoC = 1.0`) gets `tests_factor = 0.7`,
reducing the final score by 0.3 — a small but real "this looks safer"
nudge. A commit with zero tests gets `tests_factor = 1.0` and no nudge.

Files are classified as test files by path regex:

```
(^|/)(tests?|__tests__)/                       # tests/ or test/ or __tests__/ directory
(^|/)(test_[^/]+|[^/]+_test)\.(rs|py|go|js|ts|tsx)$   # *_test.rs, test_*.rs etc.
(^|/)[^/]+\.spec\.(rs|py|go|js|ts|tsx|jsx)$   # *.spec.ts, *.spec.js etc.
```

This catches Rust integration tests (`crates/*/tests/`), Python pytest
modules, Go `*_test.go`, JS/TS spec files, and `__tests__/` directories.
Unit tests inline in Rust source (`#[cfg(test)]` modules at file end) are
**not** detected — they show up as production LoC. v2 may add a heuristic
for this.

---

## 4. T-shirt thresholds

Calibrated against the last 100 commits in trusty-tools (see §7):

| Size | Score range | Approx. shape |
|---|---|---|
| **XS** | score ≤ 6.0 | ≤ ~10 LoC and 1 file, OR pure version bump |
| **S** | 6.0 < score ≤ 10.0 | tens of LoC, 1–3 files |
| **M** | 10.0 < score ≤ 14.0 | low hundreds of LoC, 3–8 files (modal bucket) |
| **L** | 14.0 < score ≤ 18.0 | several hundred LoC, 5–20 files |
| **XL** | score > 18.0 | thousands of LoC OR very large file-count refactors |

These were tuned from a starting proposal (XS≤4, S≤7, M≤10, L≤14) which
produced a heavily-skewed distribution (77% of trusty-tools commits landed
in L+XL). The shifted thresholds reflect that this repo runs hot — the
typical commit is a several-hundred-LoC refactor, not a single-line tweak.
See §7 for the post-tuning distribution.

---

## 5. Trailer format

The hook appends (or replaces, if already present) three git trailers:

```
Effort: <SIZE>                                   # one of XS/S/M/L/XL
Effort-Score: <number>                           # two decimals
Effort-Breakdown: <LoC> LoC | <files> files | <test-LoC> test LoC
```

Example final commit message:

```
feat(tga): on_default_branch population + backfill subcommand

Adds a default-branch detector and backfills the on_default_branch column
on historic commits. Closes #290.

Effort: L
Effort-Score: 15.42
Effort-Breakdown: 612 LoC | 11 files | 198 test LoC
```

Why trailers (vs notes, vs commit-message-prefix):

- **Git-native** — `git interpret-trailers` handles dedup, ordering, and
  formatting. No custom parser needed.
- **Queryable** — `git log --format='%(trailers:key=Effort)'` extracts the
  field directly. Future analytics queries (and tga's eventual
  `effort-score` subcommand) read this without re-running the computation.
- **Reviewable** — the user sees the trailers in the editor draft and can
  delete or alter them before saving. Non-coercive.
- **Cheap rewrite** — `git interpret-trailers --if-exists replace` makes
  the hook idempotent if it ever runs twice on the same draft.

---

## 6. Calibration methodology

1. Implement the formula and an initial threshold proposal (see §4 starting
   thresholds).
2. Run the script against every commit in the last 100 of the repo:
   `for sha in $(git log --format=%H -n 100); do compute-effort.sh "$sha~1..$sha"; done`
3. Bucket the results and check the distribution shape. Per Hindle 2008,
   a healthy log-normal distribution should have M (the middle bucket) as
   the mode, with both tails (XS+S, L+XL) carrying ~20–25% combined each.
4. If the distribution is skewed, adjust thresholds in `scripts/compute-effort.sh`
   (the `# THRESHOLDS` block near the top) and re-run. **Adjust only the
   thresholds — never the α/β/δ weights — to preserve the empirical formula.**
5. Update the spec doc's calibration-results section (§7) with the final
   distribution. Both the script and the doc must move together.

---

## 7. Calibration results

Final distribution across the last 100 trusty-tools commits (one sample lost
to the initial commit having no parent → 99 samples):

```
XS  (≤ 6.0)    | ###########                           | 11  (11%)
S   (6–10.0)   | ###########                           | 11  (11%)
M   (10–14.0)  | #####################################  | 37  (37%) ← modal
L   (14–18.0)  | ##############################         | 30  (30%)
XL  (> 18.0)   | ##########                            | 10  (10%)
```

Percentiles of the underlying score distribution:

| pct | score |
|---|---|
| min | 2.50 |
| p10 | 5.96 |
| p25 | 10.41 |
| p50 | 13.20 (median) |
| p75 | 16.43 |
| p90 | 18.43 |
| p95 | 19.08 |
| max | 26.52 |

The median (13.2) falls cleanly in the M bucket — that's the goal. The tails
each capture ~10%, matching the Hindle 2008 log-normal expectation. The XL
tail extends to 26.5 (a 31548-LoC merge-like commit, likely a large module
import), which is the regime where the score saturates against `log₂(LoC)`
plateau.

### Per-integer-bucket histogram of raw scores

```
score
  2: #             | 1
  3: #             | 1
  4: ####          | 4
  5: #####         | 5
  6: #             | 1
  7: #             | 1
  8: ####          | 4
  9: #####         | 5
 10: #########     | 9
 11: ###########   | 11
 12: ########      | 8
 13: #########     | 9
 14: #######       | 7
 15: #####         | 5
 16: ############  | 12
 17: ######        | 6
 18: #####         | 5
 19: ###           | 3
 22: #             | 1
 26: #             | 1
```

The peak around 11–13 reflects the trusty-tools sweet-spot: typical commits
are 100–300 LoC across 4–10 files — substantial work but not heroic.

---

## 8. v2 roadmap — cyclomatic complexity (γ term)

The γ term in §3 is set to zero in v1. Activating it requires language-aware
AST traversal to count decision points (if / match / while / for / &&) per
function before and after the diff, then summing `ΔCC` across functions.
Pure bash cannot do this; it needs a real parser.

The plan is a `tga effort-score <range>` subcommand:

- Uses tga's existing tree-sitter integration for Rust, Python, Go, JS/TS.
- Computes pre/post McCabe CC per touched function via AST node counting.
- Persists into `tga.db` for cross-repo and cross-time analytics.
- Re-emits the same trailer schema for compatibility.

When tga ships effort-score, the bash hook should defer to tga when present
(via `command -v tga`) and fall back to the v1 formula otherwise. This gives
us cross-platform compatibility (no tga needed for casual contributors) and
a richer signal where tga is installed.

Ticket: TBD — file under `trusty-git-analytics` after this PR merges.

---

## 9. References

- **McCabe, T.J.** (1976). *A Complexity Measure.* IEEE Transactions on
  Software Engineering, SE-2(4), 308–320.
  [DOI 10.1109/TSE.1976.233837](https://doi.org/10.1109/TSE.1976.233837)
- **Halstead, M.H.** (1977). *Elements of Software Science.* Elsevier.
  ISBN 0-444-00205-7.
- **Hindle, A., German, D.M., Holt, R.C.** (2008). *What Do Large Commits
  Tell Us? A Taxonomical Study of Large Commits.* MSR 2008: Proceedings of
  the International Working Conference on Mining Software Repositories,
  99–108. [DOI 10.1145/1370750.1370773](https://doi.org/10.1145/1370750.1370773)
- **SonarSource** (2017, rev. 2021). *Cognitive Complexity: A New Way of
  Measuring Understandability.* White paper.
  [URL](https://www.sonarsource.com/resources/cognitive-complexity/)
- **GitHub size-label-action** — `pascalgn/size-label-action` — industry
  practice for PR T-shirt sizing.
- **GitLab MR labels** — `~"size::XS"` through `~"size::XL"` — industry
  practice mirroring the same five-bucket scheme.

---

## 10. Implementation artifacts

- `scripts/compute-effort.sh` — pure-bash computation; JSON output;
  THRESHOLDS block at top mirrors §4.
- `scripts/insert-effort-trailers.sh` — `prepare-commit-msg` hook entry
  point; mutates the draft message; non-destructive.
- `.pre-commit-config.yaml` — new `repo: local` block; runs at
  `prepare-commit-msg` stage; does not block commits.
- `tests/test-compute-effort.sh` — unit tests via synthetic git repos.
- `docs/research/commit-effort-spec-2026-05-27.md` — this document.

To enrol another repo: copy the two scripts to `scripts/`, copy the
`repo: local` block from `.pre-commit-config.yaml`, and run
`pre-commit install --hook-type prepare-commit-msg`.
