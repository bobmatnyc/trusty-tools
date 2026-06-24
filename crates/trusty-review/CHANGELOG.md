# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

## [0.5.0] — 2026-06-24

### Added

- per-file map-reduce review for over-cap diffs (closes #1643, refs #1638, #680) ([#1657](https://github.com/bobmatnyc/trusty-tools/pull/1657)) ([`ea5c49b`](https://github.com/bobmatnyc/trusty-tools/commit/ea5c49bd))
- external linked-spec fetch for grounding (closes #1419) ([#1630](https://github.com/bobmatnyc/trusty-tools/pull/1630)) ([`f2e58c2`](https://github.com/bobmatnyc/trusty-tools/commit/f2e58c2f6d92262d0ed9b12d0ec50b9ea77d33d0))
- review-outcome capture — reactions/commits + outcome store + webhook poll (refs #1421) ([#1628](https://github.com/bobmatnyc/trusty-tools/pull/1628)) ([`d95fba9`](https://github.com/bobmatnyc/trusty-tools/commit/d95fba906e90f124cb76a3746fc90bb403ab117f))
- calibration harness + Rust false-positive prompt guardrail (closes #1422) ([#1629](https://github.com/bobmatnyc/trusty-tools/pull/1629)) ([`98d66d5`](https://github.com/bobmatnyc/trusty-tools/commit/98d66d5c412591bdcffb7761317fb2f78202c4cc))
- test-plan & AC conformance gaps as context (closes #1418) ([#1627](https://github.com/bobmatnyc/trusty-tools/pull/1627)) ([`675c5b1`](https://github.com/bobmatnyc/trusty-tools/commit/675c5b10fa17986297f7f8614bf01c12424bcb62))
- prior-PR change-history context source (closes #1423) ([#1626](https://github.com/bobmatnyc/trusty-tools/pull/1626)) ([`dd759d3`](https://github.com/bobmatnyc/trusty-tools/commit/dd759d33304d96569a79677c9ff775bd84d1f8cb))
- add Finding.source_citation + spec-grounding prompt wiring (refs #1419) ([#1625](https://github.com/bobmatnyc/trusty-tools/pull/1625)) ([`41e062e`](https://github.com/bobmatnyc/trusty-tools/commit/41e062e8f24c84ce61ef09a98ae55e7fe1df6026))

### Fixed

- prevent truncated diff from reaching reviewer (closes #1638) ([#1639](https://github.com/bobmatnyc/trusty-tools/pull/1639)) ([`48a7c54`](https://github.com/bobmatnyc/trusty-tools/commit/48a7c549d5482a27736e363a72cea49983784174))

---

## [0.4.0] — 2026-06-18

### Added — output fidelity (epic #1413)

- **Inline per-line PR comments (#1414).** Findings now post as inline review
  comments anchored to the exact file + diff line via the GitHub reviews API
  `comments[]` array, instead of being concatenated into one summary comment. A
  finding whose line is not in the PR diff (or has no line) falls back to the
  review summary body rather than failing the post. The would-be inline comments
  are carried on `ReviewResult.inline_comments` so dry-run / the MCP response
  previews exactly what live mode would post.
- **Committable `suggestion` blocks (#1415).** Findings that carry a concrete code
  fix render a one-click-appliable GitHub ```suggestion block (new
  `Finding.suggested_replacement` field). Malformed or multi-line/prose
  replacements degrade to a prose fix, never a broken block.
- **Consequence + uncertainty signaling (#1416).** Findings carry a new
  `consequence` field ("what goes wrong if unaddressed"), rendered as a
  `_Why it matters:_` line. Low-confidence findings (`confidence < 0.6`) are
  hedged ("This may be an issue: …") rather than asserted.
- **Nit-volume cap + what-not-to-flag guardrails (#1420).** At most 5 low-severity
  nits post inline; the overflow rolls up into a single "+N more minor nits
  suppressed" summary line. The reviewer prompt gains a concise "do NOT flag"
  section (linter-enforced style, speculative/hypothetical concerns, restating
  the diff, and consequence-free preferences).

### Fixed

- **Summary body no longer silently drops same-line findings (#1414).** The
  review summary previously decided which findings to render by matching each
  finding's `(file, line)` against the posted inline comments. Two distinct
  findings at the same `(file, line)` collided: one could be flagged as "inline"
  and then dropped from *both* the inline set and the summary body. The summary
  now partitions by finding identity using the authoritative inline index set
  carried on `ReviewResult.inline_finding_indices` (populated from the
  `InlinePlan`), so every non-inline finding is rendered and none is lost.
- **`github_issues` context query clamped to GitHub's 256-char limit.** An
  over-long assembled search query (`repo:… is:issue <keywords>`) caused the
  GitHub Search API to return HTTP 422 ("The search is longer than 256
  characters"), silently skipping the GitHub Issues context section. The query
  is now truncated to ≤256 chars at a word boundary (debug-logged when it
  occurs).
- **Corrected stale CLI help text.** `run`/`serve` help no longer claims reviews
  are "always dry-run, no comments posted" — they post live when posting is
  enabled (dry-run by default, controlled by `PR_INTELLIGENCE_DRY_RUN`).

### Compatibility

- New finding fields (`consequence`, `suggested_replacement`) and the
  `inline_comments` / `inline_finding_indices` / `suppressed_nits` result fields
  are all `#[serde(default)]` and remain OpenAI strict-mode compliant, so
  older/looser model responses and pre-0.4.0 review logs still parse unchanged.

## [0.3.16] — 2026-06-18

### Changed

- Reviewer prompt: praise from an automated reviewer is held to a higher bar
  than human praise — included only when clearly justified.

## [0.3.15] — 2026-06-18

### Changed

- **Reviewer prompt: praise findings are now optional and rare** (no longer
  mandatory per review); reviews lead with actionable findings. The
  principles addendum no longer requires at least one `praise:` per review —
  praise is reserved for genuinely notable engineering and is never used to
  soften criticism or pad a review.

## [0.3.10] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-review` now produces
  the `trusty-review` binary only. Install the console with
  `cargo install trusty-console`. This is part of the single-owner-per-binary
  fix for the cargo binary-ownership collisions (#1262).

## [0.3.8] — 2026-06-09

### Fixed

- **Verdict calibration: stop Medium findings from over-escalating the rolled-up
  verdict (#1015)** — advisory Medium findings no longer push the rolled-up
  verdict above `APPROVE`. The severity-floor escalation that converts a
  grade-derived verdict to `REQUEST_CHANGES` now only fires for High/Critical
  findings; Medium findings preserve the grade verdict. Prevents mechanical
  `REQUEST_CHANGES` on reviews where all findings are advisory Medium or lower.

---

## [0.3.6] — 2026-06-07

### Added

- **Prebuilt binary distribution via GitHub Releases** — the `trusty-review`
  binary is now published to GitHub Releases on every tagged version for
  `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`. Install without a
  Rust toolchain:
  ```
  curl -L https://github.com/bobmatnyc/trusty-tools/releases/download/trusty-review-v0.3.6/trusty-review-aarch64-apple-darwin.tar.gz | tar xz
  ```
  or via `cargo install --git`:
  ```
  cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-review --locked
  ```
- **README install-convention docs** — README updated with a prebuilt-binary
  install section and explicit `cargo install --git` instructions alongside the
  existing `cargo install trusty-review` (crates.io) path.

### Changed

- **`profile` Cargo feature gate** — the contributor-profile pipeline
  (`tga` + `rusqlite` heavy deps) is now gated behind the opt-in `profile`
  feature (still on by default). Users who only need core review without
  `git2`/`rusqlite` compilation overhead can build with:
  ```
  cargo install trusty-review --no-default-features --features http-server,mcp
  ```
- **Workspace MIT relicense** — the workspace `license` field was changed from
  `Elastic-2.0` to `MIT`; `trusty-review` inherits `license.workspace = true`
  and is now MIT-licensed.

---

## [0.3.5] — 2026-06-03

### Changed

- **Footer attribution (#734)** — the review footer now reads
  `Reviewed by Trusty-Review (\`<model>\`)` instead of `Reviewed by \`<model>\``.
  The brand name `Trusty-Review` appears in plain text; the model slug remains
  in backticks inside parentheses.  Both the grade-prefixed form
  (`Grade: B+ · 🤖 Reviewed by Trusty-Review (\`model\`) · …`) and the no-grade
  form (`🤖 Reviewed by Trusty-Review (\`model\`) · …`) are updated.
  Single source of truth remains `format_review_footer` in `pipeline/post.rs`.

---

## [0.3.4] — 2026-06-03

### Added

- **Letter-grade PR reviews (#732)** — the reviewer LLM now assigns a letter
  grade (A+ through F, 13 half-steps) to every review, and the verdict is
  derived from it per a fixed product mapping (APPROVE floor = B-):

  | Grade band           | Verdict         |
  |----------------------|-----------------|
  | A+, A, A-, B+, B, B- | APPROVE          |
  | C+, C, C-            | APPROVE*         |
  | D+, D, D-            | REQUEST_CHANGES  |
  | F                    | BLOCK            |

  The final verdict is `max(grade_verdict, severity_floor(findings))` so the
  grade never produces a verdict weaker than what the severity floor requires.

- **Grade → verdict reconciliation with verification** — after the adversarial
  verifier (Phase 2, #583) re-derives the verdict, the grade is clamped via
  `clamp_grade_to_verdict` so grade and verdict never disagree in the output.

- **Grade in structured output** — `ReviewResult` gains a `grade: Option<String>`
  field (serde: `skip_serializing_if = "Option::is_none"`); the MCP `review_pr`
  JSON output includes it alongside the verdict.

- **Grade in the review footer** — the posted PR comment footer now reads:
  `Grade: B+ · 🤖 Reviewed by \`model\` · tokens ↑N ↓M · est. $X`

- **Boundary unit tests** — `grade_b_minus_yields_approve`,
  `grade_c_plus/c_minus_yields_approve_star`,
  `grade_d_plus/d_minus_yields_request_changes`, `grade_f_yields_block`,
  plus `derive_verdict_with_grade_severity_overrides_grade_a` (confirmed High
  finding clamps a model "A" grade to F), and `body_footer_contains_grade`
  (pins the exact footer string).

---

## [0.3.3] — 2026-06-03

### Fixed

- **Adversarial verifier calibration — fixes 100% refutation rate** (#726)

  Three root causes were identified and fixed together:

  1. **Token truncation (root cause):** The verifier role's `default_max_tokens`
     was `16`, which is too small to hold a complete `{"judgment":"CONFIRMED","reason":"…"}`
     JSON object. Bedrock truncated the response mid-JSON, making it unparseable,
     which triggered a conservative refutation on every call.  Bumped to `128`.
     The module-level fallback constant `VERIFY_MAX_TOKENS` was also raised from
     `64` to `128` for consistency.

  2. **Biased burden-of-proof prompt:** The verifier system prompt contained
     "The default answer is REFUTED. When in doubt, REFUTE." — a strong default
     that compounded the truncation problem.  Replaced with a symmetric,
     evidence-weighing instruction that asks the model to decide on the actual diff
     content without defaulting to either verdict.  The truncation/hallucination
     hard rule (REFUTE anything referencing a file/line absent from the diff) is
     preserved unchanged.

  3. **Verdict collapse on non-clean refutations:** Unparseable/truncated verifier
     responses were mapped to plain `VerifyOutcome::Refuted`, indistinguishable
     from a clean model REFUTED.  `rederive_verdict` then treated all-refuted
     rounds as "escalation rested on refuted evidence" and dropped the verdict to
     `APPROVE`.  Two fixes:
     - Unparseable responses are now mapped to the distinct `TruncationRefuted`
       variant (already present in `VerifyOutcome`), separating infrastructure
       failures from clean model judgments.
     - `rederive_verdict` uses a three-way baseline rule: (a) any confirmed →
       keep `primary_verdict`; (b) at least one clean model REFUTED → drop to
       `APPROVE`; (c) all demotions were `TruncationRefuted`/`ErrorRefuted` →
       preserve `primary_verdict` (verifier infra failed, not the reviewer's
       reasoning).

  **Regression fixture:** `verify_join_handle_regression_pr720` models the
  dropped-`JoinHandle` finding from PR #720 (the true-positive that was wrongly
  refuted in the original incident).  Asserts that (a) a CONFIRMED judgment
  preserves the finding, and (b) a truncated judgment does NOT collapse the
  verdict to APPROVE.

---

## [0.3.2] — 2026-06-03

### Added

- **Model + token-usage footer on posted reviews** (#728) — every posted PR review
  comment now includes a metadata footer with the reviewer model slug, input/output
  token counts, and cost estimate (e.g., `🤖 Reviewed by us.anthropic.claude-sonnet-4-6 · tokens ↑1234 ↓567 · est. $0.01`).
  Single-source-of-truth with the `ReviewResult` struct fields; appears identically in dry-run output.

---

## [0.3.1] — 2026-06-03

### Fixed

- **Required-dependency health gating** (#722) — `review_health` now reports
  `status: degraded` only when a *required* dependency (trusty-search) is unreachable.
  Non-required dependencies (trusty-analyze) being unavailable does NOT degrade status.
  Centralized `compute_status` logic shared by HTTP + MCP paths.

---

## [0.3.0] — 2026-06-03

### Added

- **Inference reachability probe in health endpoint** (#719) — `review_health` now
  includes an `inference` field (`ok` | `unreachable` | `auth_error` | `unknown`),
  always-on with 10s TTL and 3s timeout. Covers Bedrock + OpenRouter availability.
  When inference != ok, service status is degraded.

---

## [0.2.0] — 2026-06-03

### Added

- **Map-reduce review Phase 1: config + selector** (#690) — structured
  map-reduce review pipeline with per-file configuration and selector logic
  to route files to the appropriate review strategy.

- **Map-reduce review Phase 2: per-file diff splitter** (#698) — per-file
  diff splitter that partitions large diffs into reviewable chunks, enabling
  reviews on PRs that exceed the single-pass diff cap.

- **Auto-derive search index from repo root** (#661) — the reviewer now
  auto-detects the trusty-search index ID from the repository root path,
  eliminating manual index configuration for standard installations.

- **Daemon http_addr discovery + auto-detect** (#665/#676) — the review
  daemon discovers the trusty-search HTTP address automatically via the
  port-lock file; manual `--search-url` override still supported.

- **list_indexes envelope parse + resolve_index wiring** (#672/#670) —
  MCP tool `list_indexes` correctly parses the response envelope and
  `resolve_index` is wired into the review pipeline.

- **GitHub Issues query cap 256 chars** (#675) — GitHub Issues search
  queries are now capped at 256 characters to stay within API limits.

- **BrokenPipe on stdin treated non-fatal** (#702) — a broken pipe on
  the MCP stdio transport is treated as a clean client disconnect rather
  than a crash, improving robustness in CI and pipe-based invocations.

- **redb 4.x dedup store recovery** (#702) — the dedup claim store is
  upgraded to redb 4.x with graceful incompatible-file recovery: existing
  redb 2.x `dedup.redb` is backed up and recreated automatically.

> **OPERATOR NOTE:** Existing `dedup.redb` is backed up to
> `dedup.redb.v2-incompatible` and recreated on first start after upgrade.
> The dedup store only tracks posted reviews (SHA-keyed claim locks), not
> review history or results — no review content is lost.

---

## [0.1.0] — 2026-05-28

### Added

- Initial release: LLM-backed PR-review service with GitHub App auth,
  MCP stdio JSON-RPC service, diff analysis with noise filtering,
  context orchestration, and dedup claim store.
