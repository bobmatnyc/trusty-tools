# contracts fixtures

Inputs for `scripts/check_contracts_selftest.sh`, which tests
`scripts/extract_contracts.py`, `scripts/diff_contracts.py`, and the
`--diff` mode of `scripts/check_contracts.sh` that wraps the latter.

All fixtures here are **hand-written and synthetic** — none is captured from a
real `cargo +nightly rustdoc` build. That is deliberate: `check_contracts.sh
--crate` needs the nightly toolchain and currently fails to build rustdoc JSON
on `main` for an unrelated reason (being fixed separately), and the
extractor's and differ's own fail-closed logic is pure Python that does not
need a real build to exercise. Driving the selftest off fixtures rather than a
live build is what keeps it independent of that build's health.

## rustdoc-JSON fixtures (`extract_contracts.py`)

Each is a minimal document shaped like real rustdoc JSON — just enough for
`scripts/lib/rustdoc_walk.py`'s loader and `SurfaceWalker` to accept it: a
crate-root module (id `0`) containing one public function (id `1`).

- `rustdoc-good.json` — the function's doc comment carries a valid
  `# Code Contract` block (one precondition, one postcondition). Extracting it
  must succeed with exactly 1 contract.
- `rustdoc-bad-contract.json` — same shape, but the block contains a line that
  is neither a section header, a `- ` claim, nor a continuation of one (free
  prose). Must be a parse error — NO VERDICT, exit 3.
- `rustdoc-no-contracts.json` — same shape, but the function's doc comment has
  no `# Code Contract` heading at all. The walk succeeds and finds nothing to
  extract — NO VERDICT, exit 3 (zero contracts is never a silent pass).
- `rustdoc-bad-schema.json` — identical to `rustdoc-good.json` except
  `format_version` is `999`, a value `extract_contracts.py`'s
  `SUPPORTED_FORMAT_VERSIONS = (57, 61)` does not list. NO VERDICT, exit 3.

## Contract-artifact fixtures (`diff_contracts.py`, `check_contracts.sh --diff`)

Each is a minimal artifact in the shape `extract_contracts.py` writes:
`{"artifact_version": 1, "crate": ..., "items": [...]}`.

- `artifact-clean-a.json` / `artifact-clean-b.json` — byte-identical items.
  Comparing them must find one item in common and zero claim changes — exit 0.
- `artifact-drift-a.json` / `artifact-drift-b.json` — same item, one
  postcondition claim reworded (`"returns x doubled"` ->
  `"returns x squared"`). Must be reported as one REMOVED + one ADDED claim —
  exit 1.
- `artifact-bad-version.json` — `artifact_version: 2`, which
  `diff_contracts.py`'s `SUPPORTED_ARTIFACT_VERSIONS = (1,)` does not list.
  Paired against `artifact-clean-a.json`, must be NO VERDICT, exit 3.
- `artifact-empty-common-a.json` / `artifact-empty-common-b.json` — valid,
  `artifact_version: 1`, but name different items (`demo::f` vs `demo::g`), so
  the two artifacts share no item. NO VERDICT, exit 3 — "0 compared" must
  never print as a pass (#5620).
