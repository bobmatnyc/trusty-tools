# Citation corpus (#9188)

Offline cases for the zero-hallucination threshold. Each `*.json` file is one
review: the diff at the head, the reviewer's output, the verifier's answer, and
the ground truth. The runner is `hallucination_count_is_zero` in
`src/pipeline/runner_hallucination_corpus_tests.rs`. It drives `run_review`
with a stub reviewer that returns `reviewer` verbatim and a stub verifier that
answers `verifier` for every finding, so no case depends on a model or the
network.

Fields:

- `leak` — the #9188 leak class the case exercises (`-` for a control).
- `diff` — the unified diff, one line per array entry; or `diff_file`, a
  `.diff` file in this directory (`billing.diff` is shared).
- `pr_description` — optional caller context; context citations resolve in it.
- `reviewer` — `prose`, `verdict`, `grade` and `findings` (the reviewer JSON
  finding shape: `title`, `body`, `severity`, `confidence`, `file`, `line`).
- `verifier` — `CONFIRMED`, `REFUTED` or `UNVERIFIABLE`.
- `hallucinated` — titles of findings that are false by ground truth.
- `forbidden_in_body` — text that names an unbacked defect.
- `expect_survivors` — how many findings must survive.
- `expect_verdict` — the verdict the review must end with, as `run --json`
  prints it (`APPROVE`, `APPROVE*`, `REQUEST_CHANGES`, `BLOCK`, `UNKNOWN`).

A case's hallucination count is: survivors that are labelled hallucinated or
that the runner's own resolver cannot resolve at the head, plus each forbidden
text found in the body, plus one when no finding survived but some were
withheld and the review still blocks, or carries a grade other than the one an
empty survivor set gives its verdict (`A+` for APPROVE, `C+` for APPROVE*, `D+`
for a `suppressed_reject` REQUEST_CHANGES, none for UNKNOWN). An all-withheld
APPROVE or APPROVE* keeps its verdict (AQ-7t, Bob 2026-10-05); a blocking one
becomes REQUEST_CHANGES with `verdict_status: suppressed_reject` (#9310), which
rests on the reviewer's own rejection and does not count as blocking on a
withheld finding. A survivor count or verdict that differs from the case's
expectation fails the run too.

To compare models, replace `reviewer` with a model's recorded output for the
same diff; the ground truth and the runner stay the same.
