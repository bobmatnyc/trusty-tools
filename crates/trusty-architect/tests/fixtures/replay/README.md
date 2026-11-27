# Replay fixtures (#12026)

Each `*.json` file is one scrubbed recording written by
`trusty-architect-replay record`. `tests/replay_tool.rs`
(`committed_replay_fixtures_pass_the_refusal_check`) loads every file here, so
a fixture holding a home path or a secret-shaped value fails the suite.

## level_lint

The first end-to-end example: three runs of the Architect's `level_lint.py`,
recorded from `<prototype-dir>/scripts/level_lint.py` and replayed in
`tests/replay_level_lint.rs` against the Rust stand-in
`examples/replay_standin_level_lint.rs`.

Regenerate from a checkout of the prototype (inputs are plain text):

```sh
P=<prototype-dir>/scripts/level_lint.py
printf 'Ran cargo test on src/lib.rs:12 with --no-fail-fast.\nSet RUST_LOG first, then see `poll_once`.\n' > note.md
printf 'Ship it with --force; tmux keeps the pane.\n' > stdin.txt
R=trusty-architect-replay; D=tests/fixtures/replay/level_lint
$R record --out $D/pm-strict-file.json --seed note.md=note.md -- $P --level pm --strict --file note.md
$R record --out $D/unknown-level-stdin.json --stdin-file stdin.txt -- $P --level wizard
$R record --out $D/engineer-quiet.json --seed note.md=note.md -- $P --level engineer --file note.md
```

Check that the Python script and the stand-in both replay clean:

```sh
$R replay $D/pm-strict-file.json $D/unknown-level-stdin.json $D/engineer-quiet.json -- $P
$R replay $D/pm-strict-file.json $D/unknown-level-stdin.json $D/engineer-quiet.json -- <target>/debug/examples/replay_standin_level_lint
```

## spec_drift_check

Twelve runs of the frozen Python reference, the Architect's
`spec_drift_check.py`, recorded on 2026-11-27 with `--clock 1795784400` and
replayed in `tests/replay_spec_drift_check.rs` against
`examples/replay_port.rs`. Each recording passes the root as `--root {ROOT}`.
Every recording is the recorder's capture of the reference's real output;
none was written or edited by hand. The input of each is its `files`: a
curated sample checkout (manifest, spec, instruction files, skills,
references, codex docs, owner-queue items, soak log, improvement ledger)
built with sample names only. It is not a copy of any live tree.

`daily-pass` and `daily-no-log` run the reference on a sample checkout shaped
like the daily input, with no orphan reference, so every check passes or is
pending. `drift-orphan-fail` adds one reference the manifest does not list and
reproduces the reference's current orphan-reference failure (INV-03).

| Recording | Case |
|---|---|
| `daily-pass` | The PASS path (PASS and PENDING only, exit 0) |
| `daily-no-log` | The same with `--no-log`: no writes |
| `drift-orphan-fail` | A reference not in the manifest (INV-03 FAIL); writes the ledger |
| `drift-many-fail` | Every check fails, CRLF and non-ASCII text included |
| `pre-phase-pending` | Before phase 1: no skill index, no owner queue, an unenforced sentence (WARN) |
| `missing-manifest`, `empty-manifest`, `manifest-key-errors` | Error and empty inputs |
| `bad-arg-unrecognized`, `bad-arg-root-value`, `bad-arg-explicit`, `help` | argparse paths |

Regenerate: lay a recording's `files` into a directory `S`, then, with the
script and the `dashboard_*.py` modules it imports copied to a sandbox
directory `P` (never the live checkout; use a path without a symlinked
component, `/private/tmp` rather than `/tmp` on macOS):

```sh
R=trusty-architect-replay; D=tests/fixtures/replay/spec_drift_check
$R record --clock 1795784400 --out $D/daily-pass.json \
  $(for e in $(ls -A $S); do printf -- '--seed %s=%s ' "$S/$e" "$e"; done) \
  -- $P/spec_drift_check.py --root '{ROOT}'
$R replay $D/*.json -- $P/spec_drift_check.py
$R replay $D/*.json -- <target>/debug/examples/replay_port spec_drift_check
```
