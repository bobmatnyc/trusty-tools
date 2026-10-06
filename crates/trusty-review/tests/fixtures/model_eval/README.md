# Model-eval dataset (Q86)

Labelled diffs for comparing reviewer models on recall, false positives,
hallucinations and cost. The harness is `tests/model_eval.rs`; scoring is in
`tests/support/eval.rs`.

## Contents

- `dataset.json` — 40 entries: 24 `seeded` (one planted defect each), 8 `clean`
  (no defect), 8 `real` (R1-R8: the file-scoped diff of the commit that
  introduced a bug a later PR fixed). A labelled entry carries `file`, inclusive
  new-side line `spans`, `anchors` and `kind`. A `removal` label lets the oracle
  read the removed lines too. A `real` entry's `source.regenerate` is the
  `git diff` command that produced its diff. Two lines differ from that
  output: a real username in R2 and R5 is replaced with `user`.
- `diffs/` — one unified diff per entry.
- `recorded/` — recorded reviewer replies, replayed offline by `FakeLlm`:
  `perfect.json` catches every defect; `bad.json` misses, invents and
  hallucinates on purpose (see its `about`).

## Scoring

- **Caught**: a surviving finding whose file matches the label, whose cited
  line is in a span, and whose body names an anchor (case-insensitive).
- **False positive**: any survivor on a clean diff. **Extra**: a survivor on a
  labelled diff that does not catch it (left for human review).
- **Hallucination**: a survivor that `tests/support/oracle.rs` cannot resolve
  at the head. The oracle requires every backtick span of 3+ characters in the
  body to be in the file. A body that names a function that does not exist,
  such as "use `checked_total()`", counts, even when the gate kept it. The
  JSON report lists each one under `unresolved` so a person can review it.
- **Hallucinations on R entries are an upper bound.** The oracle checks quotes
  against the diff's hunks, not the whole file. On a real diff, a correct
  quote of code outside a hunk counts, and so does a fix suggestion in
  backticks.
- **Incomplete rows are not scored.** A row is incomplete when the cost cap
  refused its reviewer or a verifier call, when the reviewer call failed (a
  throttle included; the runner does not retry it), or when a verifier call
  failed with `LlmError::Validation`. It counts in no score: not defects, not
  catches, not false positives, not hallucinations. The report counts
  incomplete rows per model and pass. Its cost still counts.

## Offline leg (CI)

```bash
cargo test -p trusty-review --test model_eval --no-fail-fast
```

This makes no network call. The config is built field by field
(`eval_config`): it reads neither `~/.config/trusty-review/config.toml` nor the
environment, and every external context source (Jira, Confluence, GitHub
issues, conformance, PR history) is disabled.

## Live leg

The live test is `#[ignore]`. It also does nothing unless `TRUSTY_EVAL_LIVE=1`,
so `--include-ignored` alone never calls Bedrock. It needs AWS credentials for
Bedrock in the ambient profile.

```bash
TRUSTY_EVAL_LIVE=1 cargo test -p trusty-review --test model_eval \
  live_model_comparison -- --include-ignored --nocapture
```

| Variable | Default | Meaning |
|---|---|---|
| `TRUSTY_EVAL_LIVE` | unset | `1` enables the run |
| `TRUSTY_EVAL_MODELS` | `COMPARE_CANDIDATE_MODELS` (Haiku 4.5, Sonnet 4.6, Sonnet 5.5, Opus 5.5) | comma-separated reviewer ids |
| `TRUSTY_EVAL_PASSES` | 3 | passes per model |
| `TRUSTY_EVAL_MAX_USD` | 22.00 | cost cap, USD |
| `TRUSTY_EVAL_CONCURRENCY` | 4, clamped to 8 | reviews in flight |
| `TRUSTY_EVAL_ONLY` | all | comma-separated entry ids, for a cheap smoke run |
| `TRUSTY_EVAL_OUT_DIR` | `$CARGO_TARGET_DIR/model_eval` | report directory |

The verifier is always Haiku 4.5 (`DEFAULT_VERIFIER_MODEL`), whatever the
operator's config says.

**Cost cap.** Every reviewer and verifier call is metered with
`estimate_bedrock_cost_usd` against one budget. Once the total reaches
`TRUSTY_EVAL_MAX_USD`, every further call is refused, no new diff is
scheduled, and the run stops. Calls already in flight when the cap is reached
still finish. Each in-flight review runs either one reviewer call or up to 4
verifier calls at once (`DEFAULT_VERIFY_CONCURRENCY`), so the worst-case
overshoot at 8 reviews in flight is 8 reviewer calls or 32 verifier calls.
The reviewer calls cost more: 8 Opus 5.5 calls at about 10.5k input and 4,096
output tokens each come to about $1.10. The run
refuses to start if any model has no Bedrock price, because an unpriced model
would meter as $0 and never reach the cap.

**Comparison.** Models are compared only on the passes every model completed:
a row for every diff, and none cut short by the cap. The line printed above the table and
`compared_passes` in the JSON say how many passes that was.

**Output.** `<out>/<UTC timestamp>.json` holds the config, the git SHA, the
spend, any stop reason, `compared_passes`, incomplete rows per model and pass,
a per-model summary over the compared passes, and one row per model, pass and
diff. Each row has: caught, false positives, extras, hallucinations and the
unresolved survivors, the `incomplete` reason, withheld findings by reason, reviewer and verifier
tokens, cost and latency, and wall time. A markdown summary table is printed
to stdout.
