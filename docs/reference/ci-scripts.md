# CI-only check scripts

Eight scripts in `scripts/` run in a GitHub Actions workflow and nowhere else —
no pre-commit hook, no `Makefile` target, no other doc page. Each was written
for a specific failure and none of them announced itself anywhere a reader
would look, so a change to one of the workflows below could drop it with
nothing to notice.

Scripts that CLAUDE.md or another reference already covers are not repeated
here: `check_line_cap.sh` ([sloc-cap.md](sloc-cap.md)), `check_semver.sh`
([semver-gate.md](semver-gate.md)), `check_changelog_fragment.sh`
([changelog-fragments.md](changelog-fragments.md)), `check_sld.sh`
([DOC-38](../specs/spec-linked-documentation.md)), and
`check_generated_regions.sh` ([generated-doc-regions.md](generated-doc-regions.md)).

Every script here reads its own header first. This table says where it runs and
what it stops; the header says why it exists.

| Script | Workflow(s) | What it gates |
|---|---|---|
| `check_deny_duplicates.sh` | `pre-publish.yml` | Counts `cargo deny check bans` duplicate-version warnings against a frozen budget. `multiple-versions = "warn"` exits 0 while reporting them, so without this the warnings are never read; the count may only ratchet down. |
| `check_test_count.sh` | `test-count.yml` | Wraps a `cargo test` invocation and refuses an aggregate of zero. A filter matching no tests exits 0 printing `0 passed; 775 filtered out` — a green run that proved nothing (#4307). |
| `check_rustdoc_links.sh` | `pre-publish.yml` | Broken intra-doc links, against a frozen baseline of pre-existing ones. docs.rs builds a release's documentation once and never rebuilds it, so a broken link is permanent for that version. |
| `generate-homebrew-formula.sh` | `homebrew-formula.yml`, `release.yml` | Renders `tap/Formula/<crate>.rb` for the `bobmatnyc/homebrew-trusty` tap. It is the single implementation (#5635); `release.yml` calls it instead of carrying an inline heredoc and a second copy of the crate→binary map. |
| `classify-ci-results.sh` | `ci.yml`, `red-main-notify.yml` | Turns a set of job conclusions into the red-main verdict. `cancelled` is not `failure`, so the previous `contains(needs.*.result, 'failure')` test let an all-cancelled run report main verified (#4179). |
| `ci-create-local-main.sh` | `ci.yml`, `pre-publish.yml` | Creates the local `main` branch the `trusty-agents` git tests need, and fails the step when creation genuinely fails. The `git fetch origin main:main \|\| true` it replaced swallowed a GitHub 500 and produced an unrelated test failure eleven minutes later (#5693). |
| `detect-embedder-cuda-relevant.sh` | `ci.yml` | Decides whether a change can affect the `trusty-common` `embedder-cuda` build, so the CUDA leg runs when it is relevant and is skipped when it is not. |
| `check_token_drift.mjs` | `token-drift.yml` | Compares each Tailwind app's hand-transcribed `--color-*` RGB triples against the canonical Foundry `tokens.css`. `ci.yml` deliberately does NOT duplicate it (`ci.yml`, `ui-checks` job): `token-drift.yml` already runs it across all seven crates directly rather than through each `package.json`. |

## Self-tests

Six of the eight have a companion test that proves the gate can still fail — a
gate that cannot fail makes its own green meaningless:

| Script | Its test |
|---|---|
| `check_test_count.sh` | `scripts/check_test_count_selftest.sh` |
| `check_rustdoc_links.sh` | `scripts/check_rustdoc_links_selftest.sh` |
| `generate-homebrew-formula.sh` | `scripts/generate-homebrew-formula-selftest.sh` |
| `classify-ci-results.sh` | `scripts/check-ci-helpers-selftest.sh` |
| `detect-embedder-cuda-relevant.sh` | `scripts/check-ci-helpers-selftest.sh` |
| `check_token_drift.mjs` | `scripts/check_token_drift.test.mjs`, a `node:test` suite `token-drift.yml` runs before the gate |

`check_deny_duplicates.sh` and `ci-create-local-main.sh` have no test of their
own. Both carry a frozen baseline or a fetch that can fail open, which is the
shape a self-test exists to pin, so both are candidates if either is edited.

## Which of these block a merge

None of these workflows appear in `main`'s required-status-check contexts as of
2026-09-02. Read the list live before relying on any of them to stop a merge —
a hand-copied list already cost
[#5836](https://github.com/bobmatnyc/trusty-tools/pull/5836) a merge:

```bash
gh api repos/bobmatnyc/trusty-tools/branches/main/protection \
  --jq '.required_status_checks.contexts'
```
