# L1-L3 bake-off milestone exit gate

Reference for `tcode bakeoff-gate` and the retained-evidence bundle it reads
(issue #5441, epic #2063, umbrella #2052).

A Trusty Code milestone closes only after a real coding-harness bake-off
completes levels 1, 2 and 3 against the frozen candidate, with no unexplained
regression against the previous accepted baseline. This page is the mechanical
half of that rule: the bundle layout the runner must produce, and the gate that
refuses evidence which cannot support the claim.

## Why a gate and not a checklist

The first independent qualification attempt on #5441 ran all three levels and
passed 24/24 verifier checks, and was still disqualified. The retained metadata
named no candidate commit, no binary hash, no runner or challenge revision and
no instruction/agent/skill digests; the bake-off scripts and results sat in an
untracked dirty checkout; and L3's +27% wall clock, +50% turns and +22% cost had
no recorded disposition. Nothing mechanically rejected any of that. This gate
does.

## When it runs

Once, after a milestone candidate is frozen — not per commit and not in PR CI.
PR CI keeps its focused unit and integration tests; the expensive real-model
L1-L3 run happens on the frozen candidate, and its artifacts are retained as the
next milestone's baseline. The gate itself is cheap, offline, and safe to run as
often as you like: it reads files and never starts a model, a daemon, or the
runner.

## Bundle layout

```
<bundle>/
  dispositions.json        # optional; accepted performance changes
  L1/
    metadata.json          # the retained-evidence document, below
    tcode_report.json      # the run's own report, verbatim
    prompt.txt             # the prompt the level was given
    stderr.log             # the run's stderr, verbatim
    solution.diff          # the solution the run produced
    verifier.json          # the verifier's raw output
  L2/  … same six files
  L3/  … same six files
```

All six files must exist and be non-empty in all three level directories. Extra
files are ignored; extra JSON keys are ignored.

## `metadata.json`

```json
{
  "level": 1,
  "evidence_mode": "real",
  "runner": {
    "path": "/opt/ai-coding-bake-off/scripts/run_tcode_bakeoff.py",
    "revision": "runner-abc1234",
    "dirty": false
  },
  "challenge_revision": "challenges-def5678",
  "invocation": {
    "model": "anthropic/claude-sonnet-4",
    "provider": "openrouter",
    "timeout_secs": 3600
  },
  "build": {
    "version": "0.5.1",
    "commit": "9d9571cd1",
    "commit_date": "2026-08-11",
    "binary_sha256": "4b868f5671…",
    "dirty": false
  },
  "source_digests": {
    "instructions": "sha256:…",
    "agents": "sha256:…",
    "skills": "sha256:…"
  },
  "run": {
    "status": "success",
    "turns": 13,
    "duration_secs": 812.4,
    "cost_usd": 1.23,
    "tokens": {
      "prompt": 1,
      "completion": 1,
      "cache_read": 1,
      "cache_creation": 1
    }
  },
  "verifier": { "checks_total": 10, "checks_passed": 10 }
}
```

Notes on the fields the gate is strict about:

- `evidence_mode` must be `real`. `mock` and any unrecognised value are
  rejected — an offline run may validate plumbing but never satisfies the gate.
- `build.version`, `build.commit` and `build.commit_date` must equal the level's
  own `tcode_report.json` `build` block, and `run.status` must equal that
  report's `status`. This is what makes the metadata evidence rather than an
  unverified assertion.
- `binary_sha256` is what survives when the source tree is gone. It is the only
  provenance field with no counterpart in `tcode_report.json`.
- `dirty: true` on either checkout is rejected: a dirty checkout means the
  recorded revision does not identify what ran.
- The literal string `"unknown"` counts as missing. `tcode --version` emits it
  outside a git checkout, so an emptiness test alone would let it through.
- `source_digests` records the behavior sources the run actually consumed. Under
  R1 the runner still prepares `.claude/agents` and `.claude/skills`; record the
  digests of whatever it used. The gate compares digests across levels and never
  interprets which layout was canonical, so it keeps working across the
  #5425/#5426 source convergence.

## Running the gate

```bash
tcode bakeoff-gate \
  --bundle  results/r1-candidate-9d9571cd1 \
  --baseline results/m3-accepted \
  --expect-commit 9d9571cd1 \
  --expect-binary-sha256 4b868f5671… \
  --expect-runner-revision runner-abc1234
```

Add `--json` for one machine-readable document, and `--tolerance-pct` to widen
or narrow the performance band (default 20%).

Exit codes:

| Code | Meaning |
|---|---|
| 0 | The milestone may close on this evidence |
| 1 | The gate refuses; every reason is printed |
| 2 | The gate could not reach a verdict (missing bundle, unreadable `dispositions.json`) |

The `--expect-*` pins are how the operator asserts *which* candidate was frozen.
Without them the gate still proves the three levels agree with each other; with
them it also proves they are the candidate you meant.

## What it rejects

| Rule | Fires when |
|---|---|
| `incomplete_coverage` | L1, L2 or L3 is absent |
| `missing_artifact` | A required per-level file is missing or empty |
| `malformed_metadata` | `metadata.json` does not parse, declares the wrong level, or its status contradicts `tcode_report.json` |
| `mock_evidence` | `evidence_mode` is not `real`, or the verifier ran zero checks |
| `missing_provenance` | A provenance field is empty, `"unknown"`, or a zero timeout |
| `dirty_checkout` | The runner or candidate checkout had uncommitted changes |
| `stale_runner` | Levels disagree on runner path/revision, challenge revision, or source digests — or a pinned runner revision does not match |
| `build_mismatch` | Levels disagree on the build, the metadata contradicts the report, or a pinned commit/binary hash does not match |

## What it compares against a baseline

Correctness and completion regressions block outright:

- `missing_deliverable` — a level the baseline covered is absent.
- `correctness_regression` — fewer verifier checks pass than in the baseline, or
  a level that finished `success` now reports anything else. Only `success`
  counts as clean; `partial` and `deadline_exceeded` are real distinctions for a
  `run-task` caller but are not milestone-grade completions.

Performance changes do not block, but they must be acknowledged. `turns`,
`duration_secs`, `tokens` (the four counters summed) and `cost_usd` are each
compared as a signed percentage change. An improvement, or a change inside
tolerance, is recorded as a note. A change beyond tolerance fires
`undispositioned_change` unless the bundle carries a written acceptance:

```json
{
  "L3.turns": "accepted: the extra turns are the #2265 partial-retry path, measured over runs 21-23",
  "L3.duration_secs": "accepted: same cause"
}
```

Keys are `L<level>.<metric>`. A `dispositions.json` that exists but does not
parse is exit 2, never "no dispositions" — an operator who wrote a disposition
and typoed the JSON must not be told their regression is undocumented.

A percentage change needs a non-zero baseline, so a metric that was zero or
unpriced in the baseline is skipped rather than reported as an infinite
increase.

## Recording the result

#5441 requires the run URLs/paths and the comparison summary on the milestone
epic and on umbrella #2052 before the milestone closes. `--json` output is the
intended paste: it names the levels read, every violation, and every accepted or
within-tolerance delta the gate looked at.

## Source

- `crates/trusty-code/src/bakeoff/` — the gate; `metadata.rs` owns the schema,
  `preflight.rs` the rejections, `compare.rs` the baseline diff.
- `crates/trusty-code/src/cli/bakeoff.rs` — the `tcode bakeoff-gate` wiring.
- `crates/trusty-code/tests/bakeoff_gate_e2e.rs` — the binary-level proof.
